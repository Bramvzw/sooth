#!/usr/bin/env bash
# Real-runner smoke tests: drive the sooth binary against the runner itself.
#
# The contract suite (tests/stdout_contract.rs) uses fake runners. That pins
# what sooth does, but not how a real runner parses the command sooth builds
# or what its reporter writes — which is where the bugs of v0.2 and v0.3 were.
# Each flavour here is a three-test project: one steady test, one that fails
# on every second invocation, one that always fails.
#
#   tests/smoke/run.sh <phpunit|pest|composer|jest|pytest>
#
# Needs the runner's package manager (composer, npm) or the runner (pytest)
# on PATH, and a built binary: target/debug/sooth, or $SOOTH.
set -euo pipefail

flavour=${1:?usage: run.sh <phpunit|pest|composer|jest|pytest>}
root=$(cd "$(dirname "$0")/../.." && pwd)
sooth=${SOOTH:-$root/target/debug/sooth}
[ -x "$sooth" ] || { echo "no sooth binary at $sooth — run cargo build first" >&2; exit 2; }

# Where the fixture comes from, the command under test, and the flags it
# needs. A known runner needs no --preset: that inference is under test too.
project=$flavour
preset=()   # expanded as ${preset[@]+…}: bash 3.2 calls an empty array unset
case "$flavour" in
  phpunit)  runner=(vendor/bin/phpunit);       broken='ShopTest::test_broken';  wobbly='ShopTest::test_wobbly' ;;
  pest)     runner=(vendor/bin/pest);          broken='ShopTest::broken';       wobbly='ShopTest::wobbly' ;;
  composer) runner=(composer test); project=phpunit; preset=(--preset phpunit)
                                               broken='ShopTest::test_broken';  wobbly='ShopTest::test_wobbly' ;;
  jest)     runner=(node_modules/.bin/jest); preset=(--preset jest)
                                               broken='shop.test.js::broken';   wobbly='shop.test.js::wobbly' ;;
  pytest)   runner=(pytest);                   broken='test_shop::test_broken'; wobbly='test_shop::test_wobbly' ;;
  *) echo "unknown flavour: $flavour" >&2; exit 2 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp -R "$root/tests/smoke/$project/." "$work/project"
cd "$work/project"

case "$project" in
  phpunit|pest) composer install --no-interaction --no-progress --quiet ;;
  jest)         npm install --no-audit --no-fund --silent ;;
esac

git init -q
git -c user.name=smoke -c user.email=smoke@example.invalid add -A
git -c user.name=smoke -c user.email=smoke@example.invalid commit -q -m "fixture"

# The wobbly test counts its invocations here: outside the tree, so the
# working tree stays clean and the history counts as evidence.
export SMOKE_COUNTER="$work/counter"

failures=0
output=""
status=0

# run <label> <sooth arguments…>: run sooth, keep its output and exit code.
run() {
  label=$1; shift
  echo "── $flavour: $label"
  set +e
  output=$("$sooth" run --color never "$@" 2>&1)
  status=$?
  set -e
}

# expect_exit <code>, expect <fixed string>, refuse <fixed string>: checks on
# the last run. A failed check prints that run's whole output.
fail() {
  failures=$((failures + 1))
  echo "   FAIL ($label): $1"
  echo "$output" | sed 's/^/   | /'
}
expect_exit() { [ "$status" -eq "$1" ] || fail "exit $status, expected $1"; }
expect() { grep -qF -- "$1" <<<"$output" || fail "missing: $1"; }
refuse() { ! grep -qF -- "$1" <<<"$output" || fail "unexpected: $1"; }
# expect_on_line <test id> <text>: one line names the test and says the text.
expect_on_line() { grep -F -- "$1" <<<"$output" | grep -qF -- "$2" || fail "no line has both '$1' and '$2'"; }

# 1. Repeating the suite tells a flake from a broken test.
echo 0 > "$SMOKE_COUNTER"
run "--runs 4" --no-history --runs 4 ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
expect_exit 1
expect "tests: "
# jest orders its files by what failed last, so there the label reads
# "flaky or order-dependent": the count is what every runner agrees on.
expect_on_line "$wobbly" "flaky"
expect_on_line "$wobbly" "2 of 4 runs now"
expect_on_line "$broken" "✗"
refuse "wrote no JUnit-XML report"

# 2. Re-running only the failures: a reproduced one is real, the other is not.
echo 1 > "$SMOKE_COUNTER"   # the suite run is invocation 1: wobbly fails
run "--verify" --no-history --verify ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
expect_exit 1
expect_on_line "$broken" "real (reproduced on re-run)"
expect_on_line "$wobbly" "flaky"
refuse "unverified"

# 3. The history: a pass and a failure on one clean commit prove a flake.
echo 0 > "$SMOKE_COUNTER"
run "history, run 1 (wobbly passes)" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
expect_exit 1
run "history, run 2 (wobbly fails)" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
expect_exit 1
expect_on_line "$wobbly" "known flake"
expect_on_line "$broken" "never seen passing"
[ -z "$(git status --porcelain)" ] || { label="history"; output=$(git status --porcelain); fail "sooth left the tree dirty"; }

# 4. The gate: a new test file, and only that, repeated.
case "$project" in
  phpunit) new=tests/NewTest.php
    printf '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class NewTest extends TestCase\n{\n    public function test_new(): void\n    {\n        $this->assertTrue(true);\n    }\n}\n' > "$new" ;;
  pest)    new=tests/NewTest.php
    printf "<?php\ntest('new', function () {\n    expect(true)->toBeTrue();\n});\n" > "$new" ;;
  jest)    new=new.test.js
    printf "test('new', () => {\n  expect(true).toBe(true);\n});\n" > "$new" ;;
  pytest)  new=test_new.py
    printf 'def test_new():\n    assert True\n' > "$new" ;;
esac
run "--changed" --no-history --changed=HEAD --runs 3 ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
expect_exit 0
expect "gate: 1 changed test file against HEAD"
expect "$new"
expect "3 of 3 runs, 1 test "

# 5. What each flavour is here for.
case "$flavour" in
  pest)
    # A class-based test in a Pest project is reported under a prettified
    # name; --verify must still find the method.
    rm "$new"
    echo 0 > "$SMOKE_COUNTER"
    run "--verify, class-based test under Pest" --no-history --verify -- "${runner[@]}"
    expect_on_line "ClassicTest::Totals add up" "real (reproduced on re-run)"
    ;;
  jest)
    # The same title in two files is two tests.
    rm "$new"
    echo 0 > "$SMOKE_COUNTER"
    run "identities carry the file" --no-history ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
    expect "shop.test.js::steady"
    expect "cart.test.js::steady"
    ;;
  phpunit)
    # The gate must not run in parallel, and says so before anything runs.
    run "--changed with --parallel" --no-history --changed=HEAD --runs 3 -- vendor/bin/phpunit --parallel
    expect_exit 2
    expect 'Drop `--parallel`'
    ;;
esac

if [ "$failures" -gt 0 ]; then
  echo "$flavour: $failures check(s) failed"
  exit 1
fi
echo "$flavour: all checks passed"
