# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `sooth import --env <LABEL> [--commit SHA] <reports>`: bring JUnit reports
  produced elsewhere — a CI artifact, a colleague's run — into the local
  history. `--env` is required (sooth cannot tell where a downloaded file
  came from); `--commit` asserts a clean checkout so imported observations
  can combine with local ones into flaky proof. A content-hash ledger makes
  re-importing the same file a no-op, and a batch is written all-or-nothing.
  With local greens and CI reds on one commit, the history verdict becomes
  `every failure in ci`. Exit 0, or 2 when a file is unreadable or the
  history unwritable; never 1 — import judges nothing.
- `sooth history`: print the history analysis without running or recording
  anything — the roster of proven flakes and failing-since pointers, the
  all-dirty note when it applies, and the observation total. Diagnosis-only
  like `explain`: an empty history says so and exits 0.
- `sooth import --log phpunit`: import PHPUnit console logs — the failures
  your CI already keeps (90 days on GitHub by default), no project change required. Only the
  named failures are recorded (a log reduces passes to anonymous dots, and
  sooth records witnessed facts, never deductions); a file without a
  PHPUnit summary line is refused loudly, and a green log records nothing
  and says so. Tolerates ANSI colors, CRLF, and the column prefixes and
  per-line timestamps of `gh run view --log` — those timestamps date the
  observations.
- A history made entirely on a dirty tree says so, instead of letting every
  failure read as new without explanation:

  ```
  note: all 9423 earlier observations were made on a dirty tree and cannot be
  evidence — commit or stash to let sooth prove flakiness
  ```

  Dirty observations still count in the totals and are still never evidence
  (unchanged). What changes is that the silence is explained: on a machine
  someone is working on, the tree is dirty nearly always, so the passive
  layer looked broken when it was only blind. Printed only when *every*
  earlier observation is unusable — one clean observation is enough for the
  history to speak for itself.
- Observations record **where** the run happened, so a test that passes
  locally and fails on release can finally be seen as such. The environment
  is `ci` when the `CI` variable is set and non-empty (GitHub Actions,
  GitLab, CircleCI and the rest set it), `local` otherwise — no flag, no
  network call. When every failure of a proven flake came from one
  environment *and* another environment observed the test too, the report
  says which:

  ```
    - App.OrderTest::test_ships — known flake (1 of 4 in history, 25%; every failure in ci)
  ```

  Both halves are required: without a second environment, "every failure was
  local" says no more than "every run was local". Histories written before
  this keep loading — a line without the field is an observation of an
  unknown environment, never a corrupt one — and unknown environments never
  carry the claim. The `--json` shape gains `failures_confined_to` on each
  `history.flaky` entry.
- Every red run is classified against what sooth already knows: each failure
  is labeled a known flake (with its failure rate), a regression (`failing
  since <commit>`), quarantined, or new, under a headline that answers the
  question a red build asks — `2 failures — all known flakes, nothing new`.
  The `--json` shape gains an additive `explanation` object. The verdict and
  exit code are untouched: sooth explains failures, it never absorbs them.
  When nothing new failed and `--fail-on-flaky` still exits 1, the report
  says why — the pardon rests on the committed list, not on sooth's own
  evidence — and names the file to add the ids to.
- `sooth explain --junit <PATH>`: the same classification for a report you
  already have, without running anything. It records nothing, always exits 0
  when it could read the report (2 when it could not), and `--json` makes its
  whole output machine-readable.

- Flaky detection, the core value: with a report source and `--runs N`, the
  report is parsed after every run and tests with mixed outcomes are ranked
  by failure rate. Always-failing tests are listed separately as broken —
  never called flaky. Skipped runs carry no signal.
- The `--json` shape gains additive `flaky` and `broken` arrays when a
  multi-run analysis ran (still `schema_version` 1).
- The runner/report disagreement note now covers both directions: a failing
  runner whose report shows 0 failing tests is called out on stderr too —
  a swallowed failure would otherwise poison the history with false passes.
- `--verify`: after a failing single run with `--preset`, re-run only the
  failed tests twice and split them into real failures, flaky-or-order-
  dependent ones, and unverified ones (the selection never re-ran them).
  The suite verdict and exit code are unchanged — sooth classifies, it
  never absorbs a failure. The `--json` shape gains an additive
  `verification` object when the pass ran (still `schema_version` 1). Not
  supported for the go preset yet; refused loudly.
- Quarantine for known flakes: a committed `.sooth-quarantine` file (one
  test id per line, `#` comments) plus `--fail-on-flaky` make the run exit 0
  when every failure in every run's report is on the list — reported loudly,
  never hidden. Any unlisted failure, or a failed run its report cannot
  explain, still fails the run. Requires a report source; without the flag
  the file is inert. The `--json` shape gains an additive `quarantine`
  object when a pardon happened (still `schema_version` 1).
- Local run history: every run with a report source appends one observation
  per test to `.sooth/history.jsonl` (opt out with `--no-history`), stamped
  with the git commit and a dirty flag. Mixed outcomes on one clean commit
  are reported as flaky per history; a green→red flip at a commit boundary
  as `failing since <commit>` — a regression pointer, never a flaky label.
  Gitignore `.sooth/`. The `--json` shape gains an additive `history` object
  when the pass ran.

### Added

- Flaky detection checks its own precondition. It assumes a fixed order
  repeated N times, but the runner picks the order — and `--order-by=defects`,
  `--random-order` and pytest-randomly all reorder between runs. When two runs
  did not share an order, a mixed outcome is reported as `flaky or
  order-dependent` instead of `flaky`, since order-dependence would look
  identical:

  ```
    - App.OrderTest::test_ships — flaky or order-dependent (1 of 2 runs now;
      run 2 did not share run 1's order), new (nothing in history)
  ```

  Only the tests two runs share are compared, so a run cut short by
  `--stop-on-failure` is not mistaken for a reordering. The `--json` shape
  gains an additive `reordered_runs` array alongside `flaky`/`broken`.
### Changed

- A test's state reads at a glance: every per-test line opens with a glyph
  that mirrors the verdict — ❌ attention now (new, real, broken), 🎲 flake
  evidence, 📉 failing since, 🚧 quarantined — and the verdict line closes
  with ✅/❌. The namespace prefix of an identity is dimmed so the eye lands
  on `ClassTest::method`; every byte stays, and with `--color never` the
  identities are byte-identical to before. `--json` is untouched.
- The report is organised per test instead of per analysis pass. A failing
  test gets one line carrying both answers — what this run saw and whether
  sooth knew it already — so `flaky tests`, `real failures (reproduced on
  re-run)` and `quarantined failures (pardoned by …)` no longer open sections
  of their own:

  ```
  3 failures — 1 known flake, 1 quarantined, 1 new
    - App.MailTest::test_send — broken (2 of 2 runs now), quarantined (listed in .sooth-quarantine)
    - App.NewTest::test_thing — broken (2 of 2 runs now), new (nothing in history)
    - App.OrderTest::test_ships — flaky (1 of 2 runs now), known flake (2 of 6 in history, 33%)
  ```

  Nothing is reported twice, and a test can no longer appear under two labels
  that seem to contradict each other. The `--json` shape is unchanged: it stays
  organised per pass, and machines are not bothered by repetition.

- The quarantine file is now read on every failing run, not only under
  `--fail-on-flaky`: its entries label failures as known. Exit steering is
  unchanged — without the flag the list still pardons nothing.
- The history section no longer repeats tests that failed the run being
  reported: their history verdict rides along with the failure itself. What
  is left over is headed "also flaky per history (these did not fail this
  run)" — the flakes that stayed quiet this time.
- With `--runs N`, the suite verdict now considers every run's report: a
  failure in run 1 is not forgiven by a green run 2.
- `--junit` freshness is now an observed fact instead of a clock comparison:
  the report must actually change during each run (no tolerance window,
  immune to clock skew). Presets delete their report before every run, so a
  runner that stops writing fails loudly instead of re-serving the previous
  run's file.
- Duplicate test ids within one report (data-provider rows, retry reporters)
  collapse to the run's worst status before flaky analysis, so a
  deterministic failure can never be misreported as flaky.

### Fixed

- The history analysis orders observations by their timestamp instead of
  their position in the file. Appending lines from elsewhere (a CI history,
  an older backup) used to land old evidence at the tail of the file —
  exactly where `failing since` reads — so a merge could invent or hide a
  regression. Ties keep file order; ordinary single-machine histories are
  unaffected.

- Verify selection carries the test's raw name instead of re-splitting the
  joined `classname::name` identity, so a test whose own name contains `::`
  (a jest title, say) is actually re-run instead of landing in `unverified`
  (#91).
- The crash context after an unusable report counts against the requested
  `--runs` total: aborting on the first of three runs now says "run 1 of 3
  failed" instead of "run 1 of 1", so the skipped runs are visible (#80).

## [0.1.0] - 2026-07-15

### Added

- `sooth run -- <command>`: run any test command (`--runs N` times, fixed
  order) with inherited stdio, per-run `runner exit=`/`runner signal` lines,
  and a closing `result: PASSED/FAILED` verdict.
- Report sources: `--preset pytest|phpunit|jest|go` injects the right
  reporter flags and manages a private temp report; `--junit <PATH>` reads
  the report your command writes during the run (a file that predates the
  run is rejected as stale).
- Tolerant JUnit-XML parser: accepts a `<testsuites>` or bare `<testsuite>`
  root, ignores unknown attributes and elements, and never panics on
  malformed input.
- Totals and a slowest-N ranking with classname-qualified test names,
  colored terminal output (`--color auto|always|never`, `NO_COLOR`
  respected), and machine JSON via bare `--json` (sooth's final stdout
  line) or `--json=PATH` (a clean file), versioned with `schema_version`.
- An exit-code contract: `0` — the runner and its report agree everything
  passed; `1` — the suite failed (either signal); `2` — sooth itself failed.
  Runner/report mismatches and unusable flag combinations fail loudly.
- When the report is unusable and the runner itself failed (a crashed
  worker, an OOM), sooth keeps the run facts it measured: a second stderr
  line names the failed run, its exit status and duration, and points at
  the runner's own output as the likely story.
