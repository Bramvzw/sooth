# sooth

**The truth about your tests.**

`sooth` runs your existing test command, reads the report it produces, and tells you what your
tests *actually do* — which ones are flaky, which are slow, and which depend on run order. Local,
instant, single binary. No server, no dashboard, no AI, no accounts.

```
sooth run -- pytest
```

> **v0.3.2 is out** — a prebuilt binary, no Rust needed (see [Install](#install)). For Laravel,
> `sooth run -- php artisan test` needs no flags, Pest and Composer scripts work, and the
> pre-push gate repeats only the tests you changed. Slow-test and order-dependence analysis
> are next. See `ROADMAP.md` for the full plan and `DECISIONS.md` for why it's built this way.

## Usage

```
# a runner sooth knows: its preset injects the reporter flags and reads the report
sooth run -- pytest
sooth run -- vendor/bin/phpunit
sooth run -- php artisan test

# a command that does not name its runner: say which preset it is
sooth run --preset phpunit -- composer test

# jest: the preset needs the jest-junit package, so it is never assumed
sooth run --preset jest -- node_modules/.bin/jest

# any other runner: point sooth at the JUnit-XML report your command writes
sooth run --junit report.xml -- ./run-tests.sh

# machine-readable JSON, straight to a file
sooth run --json=sooth.json -- pytest
```

Presets: `pytest`, `phpunit` (also PHPUnit's relatives Pest and paratest;
`--preset pest` is accepted), `jest` (needs the
[`jest-junit`](https://github.com/jest-community/jest-junit) package) and
`go` (gotestsum). When the command names its runner — `pytest`,
`vendor/bin/pest`, `php vendor/bin/phpunit`, `php artisan test`,
`gotestsum` — sooth reads the preset from it and says so in one line; pass `--preset` yourself to choose, or to
drop that line in a hook. Two things are never assumed: `jest`, because its
preset needs a package your project may not have, and a script such as
`composer test` or `npm test`, which can run anything. A run that ends up
without a report says which flag adds one.

If your runner's own configuration already writes a JUnit report (`<junit
outputFile="…"/>` in `phpunit.xml`, `addopts` in `pytest.ini`), a preset's
report takes its place for that run. To keep the file, point sooth at it
instead: `sooth run --junit build/junit.xml -- vendor/bin/phpunit`.

A preset needs the command after `--` to be the test runner itself — not a
wrapper like `python -m pytest`, `npm test` or `npx jest` (the wrapper would
get the flags). PHP is the exception: the
phpunit preset knows `php artisan test`, Composer scripts and a runner
started through `php`, and puts its flags where each of them hands
arguments on, so Pest, PHPUnit and `--parallel` all work the way you run
them every day:

```
sooth run -- php artisan test
sooth run -- php artisan test --parallel
sooth run -- vendor/bin/pest
sooth run -- php -d memory_limit=1G vendor/bin/phpunit
sooth run --preset phpunit -- composer test
```

A Composer script works when it passes its arguments on to the runner, the
way `composer test -- --filter=X` does. Composer appends them to *every*
command of a script, so the skeleton Laravel 12 shipped needs
`@no_additional_args` on its `config:clear` line, as Laravel 13's has;
until then sooth says so and you can wrap `php artisan test` instead.

Exit codes: `0` — the runner and its report agree everything passed; `1` —
the suite failed; `2` — sooth itself failed, or was stopped before it had
made every run (your CI can tell a red suite from a broken invocation).

Stopping sooth is safe. On a Ctrl-C, or a `SIGTERM` from CI or a timeout
wrapper, it reports the runs that had finished, stops the test command if
that is still running, removes its temp files and exits 2. A second Ctrl-C
ends it on the spot. Two limits: sooth stops the command it started, not
what that command started in turn — a Ctrl-C at a terminal reaches those
too, a lone `SIGTERM` does not — and a signal that arrives when the last
run is over changes nothing: the report follows and the exit is the suite's.
The one exception is `--verify` with failures still to re-run: that
verification is then cut short, and the exit is 2.

## Run history

Every run with a report source appends one observation per test to
`.sooth/history.jsonl` in the directory you run from. You do not have to
ignore it: the first time sooth creates `.sooth/` in a repository that does
not ignore it, it adds the directory to `.git/info/exclude` (local to your
clone, never committed) and says so once. Sooth itself never counts it, or
the `--junit` report a run writes, as a change to your code. Flaky evidence
then accumulates from runs you make anyway:

- **flaky per history** — the same test both passed and failed on one clean
  commit: proven nondeterminism, ranked by failure rate.
- **failing since `<commit>`** — green until a commit, red ever since: a
  regression pointer, deliberately *not* called flaky. Start at `git show
  <commit>`.
- **never seen passing** — red in every observed run. No commit is blamed:
  sooth only knows where it first looked.
- **slower since `<commit>`** — taking longer since a commit than before
  it, on the runs it passed. See "Which tests got slower?" below.

A proven flake that has failed every run since a commit, in three
invocations or more, says so on its line (`…, and every run since <commit>
(3 runs)`) and no longer counts as "nothing new": a flake can break for
real too.

Runs on a dirty working tree count in the totals but are never used as
evidence — sooth does not draw conclusions from code it can no longer
identify. If that leaves nothing to go on, the report says so rather than
letting every failure read as new:

```
note: all 9423 earlier observations were made on a dirty tree and cannot be
evidence — commit or stash to let sooth prove flakiness
```

Opt out per run with `--no-history`; delete or trim the file whenever you
like, it is yours. CI evidence travels as report files, not history files —
see "Bring your CI evidence home" below: twenty pipeline runs a day become
twenty observations a day, and CI trees are clean, so those observations are
the ones that prove things.

Each observation also records **where** it was made — `ci` when the `CI`
variable is set, `local` otherwise.

Look at the evidence any time with `sooth history` — the same analysis every
run ends with, without running or recording anything.

### Which tests got slower?

A list of the slowest tests tells you where the time goes. It does not tell
you what changed. The history does, because it has a before:

```
slower since 1019f96 (local) — 1 test of the 19 that take 20ms or more:
  ▲ ExportTest::test_exports_orders 14ms → 284ms (5 runs before, 3 since)
```

A test is named with a commit when all of this holds:

- **Three sooth runs on each side** of the commit. A run counts when the
  test passed in it, on a clean tree, and it **ran the whole suite**: what
  a test costs depends on what ran before it, so a run of one file or one
  `--filter` is no measurement of the tests in it. A `--runs 20` is one run
  here, with the median of its twenty — evidence is counted the way it is
  for flakiness.
- **Twice as long, and 20 ms more**: the median of every run since the
  commit against the median of the last five before it.
- **No run since as fast as one of those five**, and no commit among the
  runs since that also ran before it. A test that got fast again is not
  named, and neither is code that was fast once and is slow now.
- **On one clock.** Local runs are compared with local runs, CI with CI.
- **The other tests kept their time.** A clock cannot tell a slower test
  from a slower machine. So sooth looks at the other tests that took 20 ms
  or more before the commit, over the commit's own runs: when a quarter of
  them take a quarter longer or more, the clock moved, and no test is
  named against it. One line says how many got slower and why they are
  not named:

```
slower since 95330b3 (local) — 17 tests by their own clock, not named: over the same runs, a quarter of the other tests that took 20ms or more before took 2.6 times as long or more, read from at least 18 of them. The machine, a setting or something the tests share.
```

That is real output: three runs on efficiency cores after a commit that
changed no test. A laptop on battery, a busy CI runner, a fixture every
test uses that got slower — the line is the same, and finding which is
yours.

A test that got slower in steps is named for the commit of its largest.
Every heading says how many tests got slower at the commit and of how many
could have; when that is three or more and over a fifth of them, the
heading says so many at once may be something they share, and shows three.

What it cannot know:

- **The cause.** The commit is where the clock changed. That is where to
  start looking, not a verdict.
- **A test that got slower on a clock that moved.** It is withheld with the
  rest: on a busy CI runner the test that really got slower is withheld
  along with the ones the load slowed. The other tests are read over the
  runs the test itself was timed in on its commit, against their runs
  before the commit; so what changes at a later commit — a machine, other
  tests — is that commit's reading and does not take back a test named
  before it, and runs the test sat out are not its clock. Two things
  follow. The test's time since is every run since, so a machine that
  changed later shows in it. And a test with fewer than three runs on its
  commit is read over its first runs on the next commit as well: when the
  clock moved there, it is withheld for as long as the runs before its
  commit are in its window.
- **Something that slows a minority of the tests.** A debugger or coverage
  that slows the tests which compute and leaves the ones which wait reads
  as those tests getting slower; the heading says many at once.
- **A suite that got slower as a whole.** Only tests are findings here.
- **Your branches.** Sooth knows the order of your runs, not the shape of
  your history: a slowdown that began on a branch, with runs elsewhere in
  between, is named for the first commit of the current run of slow runs.
- **A suite that changes size.** "The whole suite" is nine tenths of the
  largest run among a test's last 50. Grow or shrink the suite by more
  than a tenth, or reshuffle CI shards, and the runs on one side are parts
  until the window has moved on. A run that leaves out the one test which
  builds a fixture is still the whole suite, and the next test pays.
- **Small suites.** With fewer than five other tests that took 20 ms
  before and passed in the runs the test was timed in, there is nothing to
  read a clock from, and a slow machine can name a test.
- **What is behind it.** A finding lasts as long as the runs before its
  commit are among the test's last 50; after that the test is no longer
  named, though it is no faster.

Nothing here changes the exit code: slower is a finding, not a failure.
Ten tests are listed per commit, and `--json` has all of them under
`history.slower_since`, the withheld under `history.slower_withheld`.

Durations are in the history since v0.3.4, so the before starts there.

The history only grows: a 5,000-test suite adds about a megabyte per run.
Every analysis looks at each test's last 50 invocations in which it ran, so
what is older is dead weight. `sooth history --prune` rewrites the file down
to that window — a 270 MB history of that suite comes back as 54 MB in four
seconds. It removes only what no analysis reads, and it steps back when
another sooth is writing to the history at that moment: run it again.

## Bring your CI evidence home

The failure that hurts most passes locally and fails on release. CI does not
need sooth for this — it only needs to keep the report it already can write
(`--log-junit report.xml`, uploaded as an artifact). Download the artifacts
and import them:

```bash
sooth import --env ci --commit 8cbf89a report-8cbf89a.xml
```

`--env` is required — nothing in a JUnit file says where it was produced
(PHPUnit's carries no hostname or timestamp at all), and even provenance
would not name *your* environment taxonomy, so sooth records your assertion
rather than a guess. Want certainty the file really came from your pipeline?
Verify it at download time, where the network lives: GitHub's
`gh attestation verify` checks an artifact's signed provenance before you
import it. `--commit` asserts the reports came
from a clean checkout of that commit; without it the observations count in
totals but can never be proof, because flaky proof needs a known clean
commit. The abbreviated sha your CI shows is fine — sooth resolves it against
your clone, and refuses one your clone does not know. Importing the same
file twice is harmless: sooth keeps a ledger and skips it. Imported it with
the wrong `--env` or `--commit`? `sooth import --replace …` imports the file
again in the place of its earlier observations.

One thing has to match for local runs and CI reports to meet: the runner. A
test is known by the name its report gives it, and Pest (`php artisan test`
included) writes a class-based test as `That true is true` where
`vendor/bin/phpunit` writes `test_that_true_is_true`. Use the same runner
in both places; the same goes for the ids in `.sooth-quarantine`.

With local runs and CI reports in one history, the difference becomes
visible:

```
  ~ App.OrderTest::test_ships — known flake (6 of 52 in history, 12%; every failure in ci)
```

That is the line that saves an afternoon: stop reading the test, start
comparing the two environments. Sooth only says it when a second environment
actually observed the test — otherwise "every failure was local" would mean
no more than "every run was local".

### Mine your CI's past

No JUnit artifacts in your pipeline (yet)? The console logs your CI already
keeps name every failure with full test identity, and GitHub retains them
for 90 days by default (organisations can shorten that — check yours). `sooth import --log phpunit` reads them — failures only: a log
reduces passes to anonymous dots, and sooth records witnessed facts, never
deductions. It reads PHPUnit's own output (`vendor/bin/phpunit`, paratest);
a log from `php artisan test` is Collision's format and is refused. Backfill months of CI failures without changing a line of your
project:

```bash
gh run list --branch main --status failure --limit 1000 \
  --json databaseId,headSha --jq '.[] | "\(.databaseId) \(.headSha)"' |
while read id sha; do
  gh run view "$id" --log-failed > "ci-logs/$id.log"
  sooth import --log phpunit --env ci --commit "$sha" "ci-logs/$id.log"
done
```

Stick to your integration branch — red PR runs are usually honestly broken
work-in-progress, not flake signal. A log-mined test reads as `failing
since` until a pass on a shared commit arrives (a log has no denominator);
your next clean local green flips it to `known flake`, `every failure in
ci`. Keeping JUnit artifacts stays the gold standard: reports carry the
passes too, and with them the failure rates.

## Explain a red run

The daily pain is not finding flakes, it is being blocked by them. Every
failing run therefore closes with each failure labeled against what sooth
already knows:

```
3 failures — 1 known flake, 1 quarantined, 1 new
  ⊘ App.MailTest::test_send — broken (2 of 2 runs now), quarantined (listed in .sooth-quarantine)
  ✗ App.NewTest::test_thing — broken (2 of 2 runs now), new (nothing in history)
  ~ App.OrderTest::test_ships — flaky (1 of 2 runs now), known flake (2 of 6 in history, 33%)
```

Every failing test gets one line with two answers. **What this run saw** —
`flaky`, `broken`, or, after `--verify`, `real` / `flaky or order-dependent` /
`unverified`. (With `--runs N`, `flaky` weakens to `flaky or order-dependent`
when the runs did not share one order — `--order-by=defects` and
`--random-order` do that, and then order-dependence looks exactly like a flake.) And **whether sooth knew it already** — `known flake` (proven by
the history), `failing since <commit>` (a regression: known, but real, so it
never counts as "nothing new"), `never seen passing`, `quarantined`, or `new`. Those two are
independent: a test can be broken *and* never seen before.

The verdict and exit code do not change: sooth explains failures, it never
absorbs them. If nothing new failed and `--fail-on-flaky` still exits 1, the
report says why — pardoning needs the committed list below, not sooth's own
evidence.

For a report you already have — a CI artifact, a run someone else made —
`sooth explain` does the same thing without running anything:

```bash
sooth explain --junit report.xml
sooth explain --junit report.xml --json | jq .explanation.counts
```

It reads the history and the quarantine list, writes nothing, and exits 0
whenever it could read the report (2 when it could not).

## Verify failures

`--verify` removes the `--runs N` cost from daily use. When a run fails,
sooth re-runs *only the failed tests* twice — seconds instead of N× the
suite — and each failure's line above says what came of it:

- **real** — failed on every re-run, at least once with the same failure
  the suite saw: fix the test or the code.
- **flaky or order-dependent** — passed on re-run in isolation.
- **failed differently** — the re-run failed with another exception than
  the suite saw (`TypeError` vs `RuntimeException`): an artifact of
  isolation, not a reproduction.
- **unverified** — the re-run did not cover it; sooth does not guess.

The suite verdict and exit code are unchanged: sooth classifies failures, it
never absorbs them the way retry plugins do. Requires `--preset` (sooth must
re-invoke your runner on a subset) and a single run; not supported for the
go preset yet.

```bash
sooth run --verify --preset phpunit -- vendor/bin/phpunit
```

## Does the order matter?

A test that passes only because another one ran before it is a failure
waiting for the day the order changes: a new test file, `--parallel`, a
colleague who runs it alone. `sooth order` finds those tests before that day:

```bash
sooth order -- php artisan test
```

```
your order: runner exit=0 (12.31s)
shuffled, seed 48213: runner exit=1 (12.90s)
shuffled again, seed 48213: runner exit=1 (12.74s)
your order again: runner exit=0 (12.28s)
alone: 2 tests, twice each
tests: 412 tests compared in both orders
2 tests depend on the order
  ✗ Tests.Unit.PartnerTest::sends a message — passed in your order, failed shuffled, twice each
      alone it fails the same way: it needs something a test before it leaves behind
  ✗ Tests.Unit.MailTest::delivers real mail — passed in your order, failed shuffled, twice each
      alone it passes: something a test before it leaves behind breaks it
replay this order: sooth order --seed 48213 -- php artisan test
result: ✗ ORDER-DEPENDENT — 2 of 412 tests (50.23s total)
```

The suite runs once the way it always runs and once in an order the runner
shuffles under a seed. Two runs that differ are also what a flaky test
produces, so when a result differs, both orders run a second time. A test is
called order-dependent when it did one thing twice in your order and the
other thing twice in the shuffled one. A difference that does not hold is
listed as exactly that, and a test the history already proved flaky is never
called order-dependent.

Each test it finds — the first twenty, when there are more — is then run by
itself, twice, because that is what decides the fix. A test that fails alone
the way it failed in the suite **needs something a test before it leaves
behind**: give it a setup of its own. A test that passes alone is **broken
by something a test before it leaves behind**: the fix is in the test that
leaks, and this one is only where it shows. Which test that is, sooth does
not say; finding it takes a search through the orders that no
runner-agnostic tool can do honestly.

"By itself" is checked, not assumed. PHPUnit and Pest are asked for a test
by its class and whole name; Jest by its whole title, which is all `-t` can
say. When the runner ran anything else along with it — another test, or a
second case under the same name, like the rows of a `test.each` that share
one title — the line says `not seen alone` and gives no direction: a test
that passes in the company of the very test it depends on would point the
wrong way.

- **The seed is the finding.** Every pass prints it, and `--seed N` runs that
  order again: on your machine, a colleague's, or in CI.
- **It is a sample, not a proof.** Two orders that agree say nothing about a
  third; another seed is another sample. And a handful of runs cannot tell
  a test that fails half the time from one that depends on the order with
  certainty: a test that does not do the same thing alone twice is taken
  back out, and replaying the seed is the last word.
- **Exit codes as everywhere.** A test that fails in another order fails a
  run, so the pass exits 1; 0 when both orders are green; 2 when the pass
  could not be made.
- **Its own command, on purpose.** `run --runs N` repeats one order to find
  flakes. Shuffling inside it would make every order-dependent test look
  flaky (see `DECISIONS.md`).
- **Where it works.** PHPUnit, Pest, `php artisan test`, Composer scripts
  and Jest, which shuffles the tests inside each file. Not with
  `--parallel`: an order is what one process runs. Not yet for pytest and Go.
- The run in your own order is recorded in the history like any run. The
  shuffled runs are not: a test that fails only when shuffled would read as
  a flake there.

## How many runs do I need?

`--runs N` is a binomial experiment: a flake that fails a fraction *p* of
runs survives all N of them with probability (1−p)^N. To see it fail at
least once with confidence C you need `N ≥ ln(1−C) / ln(1−p)` runs — in
table form, pick the failure rate you want to rule out and the confidence
you can live with:

| a flake failing… | 90% confidence | 95% | 99% |
|---|---|---|---|
| 1 in 2 runs (50%) | 4 | 5 | 7 |
| 1 in 3 (33%) | 6 | 8 | 12 |
| 1 in 5 (20%) | 11 | 14 | 21 |
| 1 in 10 (10%) | 22 | 29 | 44 |
| 1 in 20 (5%) | 45 | 59 | 90 |
| 1 in 50 (2%) | 114 | 149 | 228 |
| 1 in 100 (1%) | 230 | 299 | 459 |

Read it in both directions. Five runs only catch a flake that fails more
than ~37% of the time (at 90% confidence) — "ran it a few times, found
nothing" rules out almost nothing. The gate's suggested `--runs 20`
catches a 1-in-10 flake with ~88% confidence, and stays cheap because it
repeats a handful of changed files, not the suite.

Three caveats keep the table honest:

- It assumes runs are independent coin flips. A flake tied to the clock, a
  fixed random seed, or leftover state (the monotone-flip line) is not —
  repeats then prove less than the table promises.
- A green N-run invocation proves an upper bound, never absence: "no flake
  above ~10%, at 95%" is the strongest claim 29 green runs support.
- You rarely need the big numbers. The run history accumulates the same
  evidence for free from runs that happen anyway, and `sooth import`
  brings CI's runs into it — `--runs` is the lab instrument, the history
  is the security camera.

## Gate your push

The ideal is that a flake never reaches CI — and one plain test run cannot
promise that: a 1-in-20 flake survives it with 95%, and the ~59 runs that
would catch it are hours on a full suite. But flakes are overwhelmingly
*born* in the commit that adds or changes a test, and that is only a handful
of files. The gate repeats exactly those:

```bash
sooth run --changed --runs 20 --preset phpunit -- vendor/bin/phpunit
```

`--changed[=BASE]` selects the test files that are new or changed against
the base (default: your upstream, else `origin/HEAD`) — including uncommitted
and untracked ones — and hands their paths to the runner. A handful of tests,
twenty times, is seconds; a flake being born shows up as
`~ … flaky (3 of 20 runs now)` and exits 1 before anyone else meets it. No
changed tests means one line, exit 0, and no runner spawned, so it is cheap
enough for a pre-push hook:

```bash
# .git/hooks/pre-push
exec sooth run --changed --runs 20 --preset phpunit -- vendor/bin/phpunit
```

### Share the gate with your team

`.git/hooks` is not part of the repository, so a hook there is yours alone.
To give everyone the same gate, commit the command and the hook, and point
git at them once per clone:

```jsonc
// composer.json
"scripts": {
    "gate": "sooth run --changed --runs 20 -- php artisan test"
}
```

```bash
#!/bin/sh
# .githooks/pre-push — committed, and executable (chmod +x)
exec composer gate
```

```bash
git config core.hooksPath .githooks    # once per clone
```

`composer gate` is then also what you type to run the gate by hand. Outside
PHP the same works with an npm script or a Makefile target, and a project
that already has a hook manager (CaptainHook, lefthook, husky) adds the one
command there instead. Everyone who pushes needs `sooth` on their `PATH`
(see [Install](#install)). Composer stops a script after five minutes; a
gate over a handful of files takes seconds, but if yours can run longer,
make `"Composer\\Config::disableProcessTimeout"` the script's first entry.

Repeated runs are quiet. With `--runs` above 1 — the gate always is — the
test command's output stays off the screen: you get one line per run, and if
any run failed, the output of the first one that did, once. Twenty copies of
the same output would bury the verdict. `--show-output` brings every run's
output back; a single run always shows it, as if you had run the tests
yourself.

Needs a preset — given, or read from the command — because which files are
tests is runner knowledge, and an explicit `--runs`. Give the gate the bare runner: it appends the changed files itself,
and a command that already carries its own selection (`./...`, a `tests/`
path) would run the union — the whole suite, `--runs` times. Sooth refuses
such a command when it is sure the argument is a selection; behind a
value-less flag (`pytest -q tests/`) it cannot tell it from that flag's
value, and the union runs. Deleted or
renamed-away tests select nothing (there is nothing left to run). PHPUnit
before 10 accepts a single path argument, so gating several changed files at
once needs PHPUnit 10+ — the gate checks `--version` and refuses loudly
rather than gate one file while claiming them all. For the same reason the
gate's command must not run in parallel: paratest (`--parallel`, `-p`)
takes a single path too, so sooth refuses that command on its first use
and tells you to drop the flag. A handful of files gains nothing from it.
What the gate cannot
catch — flakes that need CI's own clock, machines, or parallelism — the
history's CI evidence still does.

## Quarantine known flakes

Day one in an existing codebase finds twenty flaky tests — quarantine keeps
them from blocking every merge while you fix them. Commit a
`.sooth-quarantine` file in the directory you run from (one test id per
line, copied from sooth's own output; `#` comments allowed) and run with
`--fail-on-flaky`:

```bash
sooth run --fail-on-flaky --preset phpunit -- vendor/bin/phpunit
```

The run exits 0 only when *every* failure is on the list — the pardoned
failures are still printed, and the verdict says exactly what happened
(`result: ✓ PASSED — only quarantined flakes failed (2 tests pardoned)`).
The list pardons what you put on it, but sooth does not call it a flake
when the history says otherwise: a listed test that is `failing since` a
commit keeps its `▼`, and the verdict reads `only quarantined tests failed
(2 tests pardoned, 1 of them not a flake by the history)`.
Any new failure, new flakiness, or a failed run the report cannot explain
still fails the build. Without the flag the list still labels its entries as
known (see above), but it pardons nothing and steers no exit code.

## Status

- [x] **v0.1** — `sooth run -- <cmd>` runs your suite once, parses the JUnit XML it produced, and
      prints a summary (total, pass/fail/skip, slowest N) + `--json`. Released.
- [x] **v0.2** — flaky detection: failure-rate ranking fed by fixed-order repeats *and* a local
      run history that accumulates observations from runs you make anyway (zero extra wall-time). Released.
- [x] **v0.3** — Laravel: `php artisan test` and Pest under the phpunit preset; prebuilt binaries
      and `install.sh`, no Rust needed. Released.
- [ ] **v0.4** — refined slow-test analysis + order-dependence *detection* (no culprit bisection).
- [ ] **v1.0** — polish, distribution, launch.
- [ ] **spike** (post-v1, timeboxed) — network-egress detection: flag tests that hit the real
      network instead of a mock.

## Framework support

`sooth` is not a "works with anything, zero setup" tool — there is no real JUnit-XML standard, and
producing that report isn't free for every runner. Instead:

- **Built-in presets** (inject the right reporter flags automatically): pytest, PHPUnit, Jest, Go.
  Two runners need a reporter package in the project, because they do not ship one: Jest needs
  `jest-junit` as a devDependency, and Go needs `gotestsum` as the test command.
- **Bring your own JUnit XML** for everything else — point `sooth` at the report file your runner
  already produces.

## The no-telemetry promise

`sooth` makes zero network calls of its own: no telemetry, no update checks, no crash reporting,
no API keys, no accounts. It reads a file your test command wrote to disk and prints a report —
nothing leaves your machine. Any run history `sooth` keeps (v0.2) is a plain local file in your
repo that you own and move yourself. See `SECURITY.md`.

## Install

A prebuilt binary for macOS and Linux (x86_64 and arm64), no Rust needed:

```
curl -fsSL https://raw.githubusercontent.com/Bramvzw/sooth/main/install.sh | sh
```

It installs to `~/.local/bin` (`SOOTH_INSTALL_DIR` changes that, `SOOTH_VERSION=v0.2.0` pins a
release) and checks the archive's SHA-256 before installing. The archives are also attached to
every [GitHub release](https://github.com/Bramvzw/sooth/releases). With a Rust toolchain:

```
cargo install sooth
```

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
