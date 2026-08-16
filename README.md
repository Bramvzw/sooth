# sooth

**The truth about your tests.**

`sooth` runs your existing test command, reads the report it produces, and tells you what your
tests *actually do* — which ones are flaky, which are slow, and which depend on run order. Local,
instant, single binary. No server, no dashboard, no AI, no accounts.

```
sooth run --preset pytest -- pytest
```

> **v0.1.0 is out** — `cargo install sooth`. Flaky detection (v0.2) is in active development.
> See `ROADMAP.md` for the full plan and `DECISIONS.md` for why it's built this way.

## Usage

```
# a known runner: the preset injects the reporter flags and reads the report
sooth run --preset pytest -- pytest
sooth run --preset phpunit -- vendor/bin/phpunit

# any other runner: point sooth at the JUnit-XML report your command writes
sooth run --junit report.xml -- ./run-tests.sh

# machine-readable JSON, straight to a file
sooth run --preset pytest --json=sooth.json -- pytest
```

The command after `--` must be the test runner itself — not a wrapper like
`python -m pytest` or `npm test`. Laravel's `php artisan test --parallel` is
such a wrapper; invoke paratest directly instead (it merges the per-worker
reports itself):

```
LARAVEL_PARALLEL_TESTING=1 sooth run --preset phpunit -- \
  vendor/bin/paratest '--runner=\Illuminate\Testing\ParallelRunner' [phpunit args...]
```

Exit codes: `0` — the runner and its report agree everything passed; `1` —
the suite failed; `2` — sooth itself failed (your CI can tell a red suite
from a broken invocation).

## Run history

Every run with a report source appends one observation per test to
`.sooth/history.jsonl` in the directory you run from — add `.sooth/` to your
`.gitignore`. Flaky evidence then accumulates from runs you make anyway:

- **flaky per history** — the same test both passed and failed on one clean
  commit: proven nondeterminism, ranked by failure rate.
- **failing since `<commit>`** — green until a commit, red ever since: a
  regression pointer, deliberately *not* called flaky. Start at `git show
  <commit>`.

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
commit. Importing the same file twice is harmless: sooth keeps a ledger and
skips it.

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
deductions. Backfill months of CI failures without changing a line of your
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
never counts as "nothing new"), `quarantined`, or `new`. Those two are
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

- **real** — reproduced on every re-run: fix the test or the code.
- **flaky or order-dependent** — passed on re-run in isolation.
- **unverified** — the re-run did not cover it; sooth does not guess.

The suite verdict and exit code are unchanged: sooth classifies failures, it
never absorbs them the way retry plugins do. Requires `--preset` (sooth must
re-invoke your runner on a subset) and a single run; not supported for the
go preset yet.

```bash
sooth run --verify --preset phpunit -- vendor/bin/phpunit
```

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

Needs `--preset` (which files are tests is runner knowledge) and an explicit
`--runs`. Give the gate the bare runner: it appends the changed files itself,
and a command that already carries its own selection (`./...`, a `tests/`
path) runs the union — the whole suite, `--runs` times. Deleted or
renamed-away tests select nothing (there is nothing left to run), and PHPUnit
before 10 accepts a single path argument, so gating several changed files at
once needs PHPUnit 10+. What the gate cannot catch — flakes that need CI's
own clock, machines, or parallelism — the history's CI evidence still does.

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
Any new failure, new flakiness, or a failed run the report cannot explain
still fails the build. Without the flag the list still labels its entries as
known (see above), but it pardons nothing and steers no exit code.

## Status

- [x] **v0.1** — `sooth run -- <cmd>` runs your suite once, parses the JUnit XML it produced, and
      prints a summary (total, pass/fail/skip, slowest N) + `--json`. Released.
- [ ] **v0.2** — flaky detection: failure-rate ranking fed by fixed-order repeats *and* a local
      run history that accumulates observations from runs you make anyway (zero extra wall-time).
- [ ] **v0.3** — refined slow-test analysis + order-dependence *detection* (no culprit bisection).
- [ ] **v1.0** — polish, distribution, launch.
- [ ] **spike** (post-v1, timeboxed) — network-egress detection: flag tests that hit the real
      network instead of a mock.

## Framework support

`sooth` is not a "works with anything, zero setup" tool — there is no real JUnit-XML standard, and
producing that report isn't free for every runner. Instead:

- **Built-in presets** (inject the right reporter flags automatically): pytest, PHPUnit, Jest, Go.
- **Bring your own JUnit XML** for everything else — point `sooth` at the report file your runner
  already produces.

## The no-telemetry promise

`sooth` makes zero network calls of its own: no telemetry, no update checks, no crash reporting,
no API keys, no accounts. It reads a file your test command wrote to disk and prints a report —
nothing leaves your machine. Any run history `sooth` keeps (v0.2) is a plain local file in your
repo that you own and move yourself. See `SECURITY.md`.

## Install

```
cargo install sooth
```

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
