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
#   tests/smoke/run.sh <phpunit|pest|composer|php|jest|pytest>
#
# Needs the runner's package manager (composer, npm) or the runner (pytest)
# on PATH, and a built binary: target/debug/sooth, or $SOOTH.
set -euo pipefail

flavour=${1:?usage: run.sh <phpunit|pest|composer|php|jest|pytest>}
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
  # PHPUnit as the script a PHP binary runs, behind an option of PHP's own:
  # how a suite that needs more memory is started.
  php)      runner=(php -d memory_limit=1G vendor/bin/phpunit); project=phpunit
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

# 3a. The same proof on uncommitted work: two runs on one unchanged dirty
#     tree ran the same code. A history of its own, so nothing clean helps.
[ ! -d .sooth ] || mv .sooth "$work/sooth-clean"
echo "work in progress" > wip.txt
echo 0 > "$SMOKE_COUNTER"
run "dirty history, run 1 (wobbly passes)" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
run "dirty history, run 2 (wobbly fails)" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
expect_exit 1
expect_on_line "$wobbly" "known flake"
grep -qF '"tree":"' .sooth/history.jsonl || fail "no content recorded for a dirty tree"
rm -rf .sooth wip.txt
[ ! -d "$work/sooth-clean" ] || mv "$work/sooth-clean" .sooth

# 3b. The same history, read for time: a test that takes longer since a
#     commit is named with that commit, at its third run there and not
#     before. Every run here is of the whole suite, which is what counts.
case "$project" in
  phpunit) pace=tests/PaceTest.php; paced='PaceTest::test_pace'
    write_pace() { printf '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class PaceTest extends TestCase\n{\n    public function test_pace(): void\n    {\n        usleep(%s);\n        $this->assertTrue(true);\n    }\n}\n' "$1" > "$pace"; } ;;
  pest)    pace=tests/PaceTest.php; paced='PaceTest::pace'
    write_pace() { printf "<?php\ntest('pace', function () {\n    usleep(%s);\n    expect(true)->toBeTrue();\n});\n" "$1" > "$pace"; } ;;
  jest)    pace=pace.test.js; paced='pace.test.js::pace'
    write_pace() { printf "test('pace', async () => {\n  await new Promise((done) => setTimeout(done, %s / 1000));\n  expect(true).toBe(true);\n});\n" "$1" > "$pace"; } ;;
  pytest)  pace=test_pace.py; paced='test_pace::test_pace'
    write_pace() { printf 'import time\n\n\ndef test_pace():\n    time.sleep(%s / 1000000)\n    assert True\n' "$1" > "$pace"; } ;;
esac
commit() {
  git add -A
  git -c user.name=smoke -c user.email=smoke@example.invalid commit -q -m "$1"
}
write_pace 0
commit "pace: as fast as it gets"
for nth in 1 2 3; do
  run "slower since, run $nth before" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
done
write_pace 80000
commit "pace: 80 ms"
for nth in 1 2 3; do
  run "slower since, run $nth since" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
  # Two slow runs are two slow runs: the third is what names it.
  [ "$nth" -eq 3 ] || refuse "slower since "
done
expect "slower since $(git rev-parse --short=7 HEAD) "
expect_on_line "$paced" "▲"
expect_on_line "$paced" "3 runs before, 3 since"
[ -z "$(git status --porcelain)" ] || { label="slower since"; output=$(git status --porcelain); fail "sooth left the tree dirty"; }

# 3c. And read across environments: a test that waits only where CI is set
#     is named as slow in ci against local, on the one commit both ran.
#     Five tests that take 25 ms everywhere are what the two machines are
#     read from; with fewer, nothing is named.
case "$project" in
  phpunit) place=tests/PlaceTest.php; mailed='PlaceTest::test_mail'
    printf '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class PlaceTest extends TestCase\n{\n' > "$place"
    for n in 1 2 3 4 5; do printf '    public function test_steady_%s(): void { usleep(25000); $this->assertTrue(true); }\n' "$n" >> "$place"; done
    printf '    public function test_mail(): void { if (getenv("CI")) { usleep(80000); } $this->assertTrue(true); }\n}\n' >> "$place" ;;
  pest)    place=tests/PlaceTest.php; mailed='PlaceTest::mail'
    printf '<?php\n' > "$place"
    for n in 1 2 3 4 5; do printf "test('steady %s', function () { usleep(25000); expect(true)->toBeTrue(); });\n" "$n" >> "$place"; done
    printf "test('mail', function () { if (getenv('CI')) { usleep(80000); } expect(true)->toBeTrue(); });\n" >> "$place" ;;
  jest)    place=place.test.js; mailed='place.test.js::mail'
    printf 'const wait = (ms) => new Promise((done) => setTimeout(done, ms));\n' > "$place"
    for n in 1 2 3 4 5; do printf "test('steady %s', async () => { await wait(25); });\n" "$n" >> "$place"; done
    printf "test('mail', async () => { if (process.env.CI) { await wait(80); } });\n" >> "$place" ;;
  pytest)  place=test_place.py; mailed='test_place::test_mail'
    printf 'import os\nimport time\n' > "$place"
    for n in 1 2 3 4 5; do printf '\n\ndef test_steady_%s():\n    time.sleep(0.025)\n' "$n" >> "$place"; done
    printf '\n\ndef test_mail():\n    if os.environ.get("CI"):\n        time.sleep(0.08)\n' >> "$place" ;;
esac
commit "place: a test that waits in CI only"
for nth in 1 2 3; do
  CI= run "slow in ci, run $nth locally" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
done
for nth in 1 2 3; do
  CI=true run "slow in ci, run $nth in ci" ${preset[@]+"${preset[@]}"} -- "${runner[@]}"
  # Two runs in CI are two runs: the third is what names it.
  [ "$nth" -eq 3 ] || refuse "slow in ci"
done
expect "slow in ci, against local on the same commits"
expect_on_line "$mailed" "▲"
expect_on_line "$mailed" "3 runs in local, 3 in ci"
[ -z "$(git status --porcelain)" ] || { label="slow in ci"; output=$(git status --porcelain); fail "sooth left the tree dirty"; }
# They have said what they are for; the steps below count the suite's tests.
rm "$place"
commit "place: done"

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
    # Pest prints through Collision, as `php artisan test` does: its console
    # log names each failure by description, which the history's names
    # (Pest's JUnit reports, from the runs above) turn back into a test.
    echo 0 > "$SMOKE_COUNTER"
    "${runner[@]}" > "$work/collision.log" 2>&1 || true
    label="import a Collision log"
    echo "── $flavour: $label"
    set +e
    output=$("$sooth" import --log collision --env ci --commit "$(git rev-parse HEAD)" "$work/collision.log" 2>&1)
    status=$?
    set -e
    expect_exit 0
    expect "ClassicTest::Totals add up"
    refuse "failed to read Collision log"
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
  php)
    # Handed two files, the gate first asks PHPUnit for its version: through
    # PHP as well, or the script alone would have to be executable.
    chmod -x vendor/bin/phpunit
    second=tests/SecondNewTest.php
    printf '<?php\nuse PHPUnit\\Framework\\TestCase;\nfinal class SecondNewTest extends TestCase\n{\n    public function test_new(): void\n    {\n        $this->assertTrue(true);\n    }\n}\n' > "$second"
    run "--changed, two files" --no-history --changed=HEAD --runs 2 -- "${runner[@]}"
    expect_exit 0
    expect "gate: 2 changed test files against HEAD"
    refuse "could not read a PHPUnit version"
    rm "$second"
    # A script named with -f is not a shape sooth places its flags in: the
    # run is made as typed, and only its exit code is read.
    run "php -f, left as typed" --no-history -- php -f vendor/bin/phpunit tests/NewTest.php
    expect_exit 0
    expect "only the exit code was read"
    ;;
  composer)
    # Composer hands the arguments to every command a script runs: a script
    # of two runners would have each write the report over the other's, so
    # sooth names them and runs nothing. One of them alone runs.
    cp composer.json "$work/composer.json"
    unset COMPOSER
    php -r '$f = "composer.json"; $j = json_decode(file_get_contents($f), true); $j["scripts"] += ["both" => ["@one", "@two"], "one" => "phpunit tests/NewTest.php", "two" => "phpunit tests/NewTest.php"]; file_put_contents($f, json_encode($j, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));'
    run "a script of two runners" --no-history ${preset[@]+"${preset[@]}"} -- composer both
    expect_exit 2
    expect 'to 2 commands: `phpunit tests/NewTest.php` (in `one`)'
    expect 'a script that runs only it (`composer one`)'
    run "one runner of them" --no-history ${preset[@]+"${preset[@]}"} -- composer one
    expect_exit 0
    expect "tests: 1 total"
    cp "$work/composer.json" composer.json
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
  expect_on_line "$wobbly" "the first time"
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
