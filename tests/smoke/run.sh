#!/usr/bin/env bash
# Real-runner smoke tests: drive the sooth binary against the runner itself.
#
# The contract suite (tests/stdout_contract.rs) uses fake runners. That pins
# what sooth does, but not how a real runner parses the command sooth builds
# or what its reporter writes — which is where the bugs of v0.2 and v0.3 were.
# Each flavour here is a three-test project: one steady test, one that fails
# on every second invocation, one that always fails. The order pass gets
# four more, written here, that depend on the order they run in.
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

# order <label> <sooth arguments…>: the same for `sooth order`.
order() {
  label=$1; shift
  echo "── $flavour: $label"
  set +e
  output=$("$sooth" order --color never "$@" 2>&1)
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
expect "tests (final run): "
# jest orders its files by what failed last, so there the label reads
# "flaky or order-dependent": the count is what every runner agrees on.
expect_on_line "$wobbly" "flaky"
expect_on_line "$wobbly" "2 of 4 runs now"
expect_on_line "$broken" "✗"
refuse "wrote no JUnit-XML report"
# Repeated runs are quiet: one line per run, one failing run's output.
expect "run 4/4: runner exit="
expect "output of run 1/4, the first that failed:"
[ "$(grep -c -- "^output of run" <<<"$output")" -eq 1 ] || fail "more than one run's output was shown"

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
    # The same title in two files is two tests. The JSON names every test;
    # the human report only the failing ones and, for a larger suite, the
    # slowest.
    rm "$new"
    echo 0 > "$SMOKE_COUNTER"
    run "identities carry the file" --no-history --json="$work/identities.json" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
    output=$(cat "$work/identities.json")
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

# 6. The order pass: four tests that each pass only in their own place in
#    the file, so any shuffle of the file flips at least two of them. Which
#    seed shuffles it is the runner's business: the first of four that does.
rm -f "${new:-}"
case "$project" in
  phpunit) queue=tests/QueueTest.php; queued='QueueTest::test_'
    printf '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class QueueTest extends TestCase\n{\n    private static int $served = 0;\n    public function test_first(): void { $this->assertSame(1, ++self::$served); }\n    public function test_second(): void { $this->assertSame(2, ++self::$served); }\n    public function test_third(): void { $this->assertSame(3, ++self::$served); }\n    public function test_fourth(): void { $this->assertSame(4, ++self::$served); }\n}\n' > "$queue" ;;
  pest)    queue=tests/QueueTest.php; queued='QueueTest::'
    printf "<?php\nfinal class Queue { public static int \$served = 0; }\ntest('first', fn () => expect(++Queue::\$served)->toBe(1));\ntest('second', fn () => expect(++Queue::\$served)->toBe(2));\ntest('third', fn () => expect(++Queue::\$served)->toBe(3));\ntest('fourth', fn () => expect(++Queue::\$served)->toBe(4));\n" > "$queue" ;;
  jest)    queue=queue.test.js; queued='queue.test.js::'
    printf "let served = 0;\ntest('first', () => { expect(++served).toBe(1); });\ntest('second', () => { expect(++served).toBe(2); });\ntest('third', () => { expect(++served).toBe(3); });\ntest('fourth', () => { expect(++served).toBe(4); });\n" > "$queue" ;;
esac
if [ "$project" = pytest ]; then
  # pytest has no random order of its own: refused before anything runs.
  order "order, a runner sooth cannot shuffle" --no-history -- "${runner[@]}"
  expect_exit 2
  expect "cannot shuffle pytest yet"
else
  found=""
  for seed in 1 2 3 4; do
    # wobbly passes in the first run and fails in the second: a difference
    # between the orders that does not hold when both run again.
    echo 0 > "$SMOKE_COUNTER"
    order "order, seed $seed" --no-history --seed "$seed" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
    if grep -qF "on the order" <<<"$output"; then found=$seed; break; fi
  done
  [ -n "$found" ] || fail "none of the seeds 1 to 4 changed the order inside the file"
  expect_exit 1
  expect "shuffled, seed $found: "
  expect "shuffled again, seed $found: "
  expect "your order again: "
  expect_on_line "$queued" "twice each"
  expect_on_line "$wobbly" "the first time only"
  # Alone, a test that is not the first of the four fails as it did when
  # the shuffle put it elsewhere: it says which way the dependency runs.
  expect "alone: "
  expect "a test before it leaves behind"
  expect "replay this order: sooth order --seed $found"
  expect "result: ✗ ORDER-DEPENDENT — "
fi

if [ "$failures" -gt 0 ]; then
  echo "$flavour: $failures check(s) failed"
  exit 1
fi
echo "$flavour: all checks passed"
