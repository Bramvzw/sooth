# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **A Composer script that runs several runners is refused, not half
  read.** Composer hands the arguments after `--` to every command of a
  script and of every script it calls, so each runner wrote the report over
  the one before and sooth took the last one's tests for the whole run: a
  green run said `1 total` where three tests ran, and the history never saw
  the others. sooth now reads the script in `composer.json` first and, when
  more than one command would get its report flag, says which and runs
  nothing (#303).

### Changed

- **A Composer script whose second command ignored the flag now stops
  before it runs.** sooth counts every command of the script that gets the
  arguments, not only those that look like test runners: a wrapper runs
  tests under any name. A script such as `["echo starting", "phpunit"]`
  ran before and is refused now, as is a `--changed` gate of such a script
  with no changed test files, which passed; mark the command that runs no
  tests with `@no_additional_args`.

## [0.4.0] - 2026-11-21

### Added

- **`sooth order`: find the tests whose result depends on the order they
  run in.** `sooth order -- php artisan test` runs the suite once in its
  own order and once shuffled under a seed, and reports the tests whose
  outcome differs — after running both orders a second time, because a
  flaky test differs too. The seed is printed, and `--seed N` replays that
  order. For PHPUnit, Pest, `php artisan test`, Composer scripts and Jest;
  `--json` gives the findings as a document. A test that fails in another
  order fails a run, so the pass exits 1.
- **`sooth order` says which way a dependency runs.** The tests it finds
  (twenty at most) are run by themselves, twice. One that fails alone as
  it failed in the suite needs something a test before it leaves behind;
  one that passes alone is broken by something a test before it leaves
  behind. The first needs a setup of its own, the second a fix in the test
  that leaks. A test that does not do the same thing alone twice is not
  called order-dependent, and a run in which the runner ran other tests
  along is not taken for a run alone.

- **`slow in ci`: the history says which tests are slow in one environment
  and not in another.** On the commits both ran, in runs of the whole
  suite, a test that takes at least twice as long in one place as what
  the other tests show the machine does there, and 20 ms more than it
  explains, is named with its time in both places. Under five other tests
  to read the machines from, nothing is named. In `--json` under
  `history.slow_in`. A finding, not a failure.
- **`slower since <commit>`: the history says which tests take longer than
  they did.** After a run, in `sooth history` and after an import, a test
  is named with the commit since which it takes at least twice as long and
  20 ms more. The evidence is passes on a clean tree in runs of the whole
  suite: three sooth runs on each side, none since as fast as the last
  five before, in one environment — and the other tests kept their time
  over the commit's own runs. When a quarter of them did not, the clock
  moved: no test is named, and one line says how many got slower and why. Every
  commit says how many tests got slower at it, and of how many. In
  `--json` under `history.slower_since` and `history.slower_withheld`. A
  finding, not a failure: no exit code changes.

### Fixed

- **A runner started through PHP is the runner.** `sooth run -- php
  vendor/bin/phpunit` (or `pest`, or `paratest`) read only the exit code,
  and with `--preset phpunit` could not run at all: sooth put its reporter
  flags right behind `php`, which read them as its own. They now go behind
  the script PHP runs, with options of PHP's own in front of it or without
  (`php -d memory_limit=1G vendor/bin/phpunit`, `php -d memory_limit=1G
  artisan test`), and such a command is recognised without `--preset`.

## [0.3.4] - 2026-11-15

### Added

- `sooth history --prune`: rewrite `.sooth/history.jsonl` down to each
  test's last 50 invocations in which it ran, the window every analysis
  reads. The file only grew before (about 1.2 MB per run of a 5,000-test
  suite), and past 64 MiB the oldest observations were out of sooth's
  reach with no tool to do anything about it. A prune removes only what no
  analysis reads. It refuses to overwrite a history that another sooth
  wrote to in the meantime, and stopped halfway it leaves the history as
  it was.
- **Stopping sooth is safe.** On a Ctrl-C, or a `SIGTERM` from CI or a
  timeout wrapper, sooth reports the runs that had finished, with
  `result: interrupted after 5 of 20 runs — none of them failed` in place
  of a verdict, and exits 2. Before, an interrupted `--runs 20` lost
  everything. Under quiet repeats what the cut-short run had written is
  shown: for a run that hung, it is the only trace of where. A `--verify`
  that is cut short exits 2 as well, with the suite's own report and a
  note that its failures are unverified. Under
  `--json` the document carries
  `"interrupted":{"during":"runs","finished_runs":5,"requested_runs":20}`
  (an addition).
- The history records how long each test took: every observation in
  `.sooth/history.jsonl` carries `"ms"`, the test's duration in whole
  milliseconds, when the report has one. Nothing reads it yet — it is what
  the slow-test pass of v0.4 needs to say "slower since `<commit>`". Lines
  written by earlier versions stay valid; a line grows by about 3%.

### Changed

- The release binary is about a third smaller (1.7 MB → 1.2 MB on macOS
  arm64): link-time optimisation and stripped symbols. Nothing else
  changes; `cargo install sooth` takes a few seconds longer to build.

### Fixed

- The closing line counts a run as failed when its report shows a
  failure, whatever the runner exited with. Three runs of which one exited
  1 and one exited 0 over a red report read `1 of 3 runs failed`, under a
  line that said the test failed in two; it now reads `2 of 3 runs failed`.
- `sooth import` no longer says that a green log had nothing to record
  when the import then fails on a later file: an import that did not
  happen says nothing.
- `import --replace` no longer overwrites what another sooth wrote to the
  history while it ran. It fails and says so, where those observations
  used to be lost. The rewritten history also keeps the permissions it had.
- A signal that reached only sooth (`kill`, a CI cancel, a timeout wrapper)
  left the test command running as an orphan and sooth's temp directories
  behind. Sooth now gives the command two seconds to end by itself, then
  stops it, and removes what it created.
- A report with a `time` no duration can hold (`time="1e30"`) no longer
  crashes sooth with an internal error: the test reads as not timed.
- A run that dies on a closed pipe (`sooth run … | head -1`) no longer
  leaves its temp report directory behind. The directory is now removed on
  every way out of a run, a panic included.

## [0.3.3] - 2026-11-15

### Added

- `sooth import --replace`: import a file again in the place of its earlier
  import. A report imported with the wrong `--env` or `--commit` could not
  be corrected — the second attempt was `skipped (already imported)` and
  the wrong observations stayed. The skip message now names the flag.
- `--show-output`: show the test command's output for every run of
  `--runs N`, as before this release. Without `--runs` it is refused: a
  single run always shows its output.

### Changed

- `-h` is one line per flag; `--help` keeps the full text. The two were
  identical, a paragraph per flag, so there was nowhere to look a
  half-remembered flag up at a glance.
- **Repeated runs are quiet.** With `--runs` above 1 the test command's
  output is no longer printed N times. Each run gets its line as it
  finishes (`run 7/20: runner exit=0 (0.47s)`), and if any run failed, the
  output of the first that did is shown once, above the report. A one-file
  gate of 20 runs went from about 230 lines to 26. A single run still
  shows its output live. With bare `--json` the run lines are left out and
  a failing run's output is replayed on stderr, so stdout is the document
  alone. When sooth has to stop at a run (no report, a stale `--junit`
  file), that run's output is shown before the error.
- With `--runs N` the totals line reads `tests (final run): …`. It always
  counted the last run only, while the failures listed under it cover every
  run: `12 passed, 9 failed` above `11 failures` looked like a miscount.
- `slowest tests` lists measured durations only, and only when it ranks:
  a test with `time="0"` (pytest and jest-junit write that for anything
  under a millisecond) no longer appears as `0.00ns`, and a suite with no
  more tests than the cut-off gets no list unless `--slowest` asks for one.
  `--json` is unchanged.

### Fixed

- A usage error says what was meant. `sooth run php artisan test` answered
  `unexpected argument 'php' found` and nothing else; it now adds `tip: the
  test command goes after --: sooth run -- php artisan test`. Likewise
  `--json out.json` and `--changed origin/main` are told to use an `=`.
- `sooth import --help` describes `--env` and `--commit`. Both entries were
  blank, and what they mean was only in the README.
- `--no-history` no longer says what the history holds. A failure read
  `new (nothing in history)` and the headline counted it as `1 new`, about
  a file that was never read. Such a line now ends at the test's name (or
  at what this run itself saw), and the note below it still says the
  history was not consulted.

## [0.3.2] - 2026-11-14

### Added

- `sooth run -- php artisan test` is enough: without `--preset` or
  `--junit`, the preset is read from a command that names its runner —
  `pytest`, `phpunit`, `pest`, `paratest`, `gotestsum`, or Laravel's
  `artisan test` — and sooth says so in one line (`sooth: using `--preset
  phpunit`, inferred from `php artisan test``). `--changed`, `--verify`
  and `--json` then work without the flag too. Never assumed: `jest` (its
  preset needs the `jest-junit` package), a script or wrapper such as
  `composer test`, `npm test` or `sail artisan test`, and a command that
  already asks for its own report (`--log-junit build.xml`, or through
  `PYTEST_ADDOPTS` / `GOTESTSUM_JUNITFILE`).
- `--preset pest` is accepted as a name for the phpunit preset. It was
  refused with the tip "a similar value exists: 'jest'".
- The phpunit preset runs Composer scripts: `sooth run --preset phpunit --
  composer test` (and `composer run-script …`, `php composer.phar …`).
  Composer reads every option in front of a `--` as its own, so the
  reporter flags, a `--verify` selection and the gate's files go after the
  `--` — yours, or one sooth adds. Before, the run died on Composer's
  `Command "…report.xml" is not defined`. A script whose other commands
  refuse the extra arguments (Laravel 12's skeleton runs `config:clear`
  first) still writes no report; the error now names Composer's
  `@no_additional_args` instead of asking whether `composer` is the runner.

### Changed

- **jest identities carry the file.** A test is now
  `src/cart.test.js Cart::renders` (file, `describe` path, title) where it
  was `Cart::renders`. Two files with a `renders` test shared one identity:
  one history, one quarantine entry pardoning both. History recorded and
  quarantine ids written under the old names no longer match; there is no
  migration.
- Whitespace at either end of a report's `name` or `classname` is dropped.
  jest-junit's default templates leave a leading space on a test outside
  any `describe`; such a name could not be selected by `--verify`, and a
  report read through `--junit` or `import` kept it in the identity.
- The gate refuses a command that already selects tests itself.
  `--changed -- vendor/bin/phpunit tests/` ran all of `tests/` plus the
  changed files, `--runs` times, under a line that named only the changed
  ones. It now exits 2 before anything runs and says to give the gate the
  bare runner. Only a certain selection is refused: an argument that
  exists on disk (for go, a `./...` pattern) and cannot be an option's
  value. `pytest -q tests/` is not told apart from a flag with a value and
  still runs the union, as the README says.
- A run of a known runner without `--preset` now injects the reporter
  flags, prints the full report and records history, where it used to print
  only the per-run lines (see Added). Pass `--no-history` to keep it out of
  `.sooth/`.
- A run that had no report to read closes with one line on stderr that
  names the flag that adds one: `--junit <PATH>` or `--preset <RUNNER>`,
  `--preset jest` for jest, `--preset phpunit` for a Composer script, or
  `--junit build.xml` when the command already writes that report. Stdout
  is unchanged.
- `artisan test` is Laravel's own command only as the program or behind a
  PHP binary (`php`, `php8.4`). Behind `sail`, `ddev` or another wrapper it
  runs in a container, so the phpunit preset no longer puts a report path
  of this machine after it.
- The gate refuses a command that runs through paratest: `--changed` with
  `php artisan test --parallel`, `pest --parallel` (or `-p`) or
  `vendor/bin/paratest` exits 2 before anything runs, and says which flag
  to drop. paratest takes a single path, so such a gate worked with one
  changed test file and died on paratest's `Too many arguments` — exit 2,
  push blocked, no reason given — as soon as two had changed. A
  `--parallel` hidden inside a Composer script cannot be seen up front;
  when that gate writes no report, sooth now names paratest as the first
  thing to check. Runs without `--changed` are unaffected.

### Fixed

- The jest gate no longer counts a snapshot as a test.
  `__snapshots__/a.test.js.snap` and `data.test.json` matched because their
  names contain `.test.`; a snapshot-only change then ended in jest's `No
  tests found` and a blocked push. A jest test file needs a source
  extension now (`js`, `jsx`, `ts`, `tsx` and their `m`/`c` forms).
- The gate no longer loses a changed test file whose name git would quote.
  A path holding a `"` or a backslash came back C-quoted, was no test file
  in that form, and silently left the selection — the gate reported fewer
  files than had changed, or "nothing to prove". The listings are read
  NUL-separated now, as written on disk.
- `--verify` in a Pest project: a class-based PHPUnit test is found again.
  Pest reports `test_that_true_is_true` as `That true is true`, so the
  re-run's `--filter` matched nothing and every such failure — the
  skeleton's `ExampleTest` to begin with — read `unverified (the re-run did
  not cover it)`. The filter now matches the name in any spelling it can
  have been prettified from: an optional `test` prefix, a space for an
  underscore or a camelCase hump, either case. Plain PHPUnit names and
  Pest-style descriptions match as before.

### Security

- The gate hands the runner paths, never options. A changed test file
  called `--bootstrap=payloadTest.php` (or `-k_test.py`) reached PHPUnit or
  pytest as an option: it would have run anyway as a test, but this way its
  name alone chose a bootstrap file or replaced the project's
  configuration. Selected paths are now passed as `./<path>`.

## [0.3.1] - 2026-11-14

### Changed

- The first time sooth creates `.sooth/` in a repository that does not
  ignore it, it adds `.sooth/` to `.git/info/exclude` (local to the clone,
  never committed) and says so once. The history then stays out of `git
  status` and `git add -A` without a line in the project's `.gitignore`.
- `--json`: `explanation.failures[].verdict` can be `never_passed` (with
  `first_commit`); a `known_flake` entry and `history.flaky[]` carry
  `red_since` (`null`, or `commit` and `failed_runs`);
  `explanation.counts` and `history` gain `never_passed`. Additions only.

### Fixed

- A pardoned regression is no longer called a flake. With
  `--fail-on-flaky`, a quarantined test the history reports as `failing
  since` a commit (or never seen passing) got the calm `⊘` and the verdict
  `only quarantined flakes failed`. The pardon stands, since the team
  listed the id, but the test keeps its `▼` and the verdict reads `only
  quarantined tests failed (… pardoned, 1 of them not a flake by the
  history)`.
- `--verify`: one exact reproduction is a reproduction. A failure that one
  re-run reproduced exactly and the other failed differently (a teardown
  error, say) was labelled `failed differently … not a reproduction`; it
  now reads `real`. `failed differently` is left for a failure no re-run
  reproduced.
- `--verify` never re-runs the whole suite for a failure it cannot name. A
  failed test case without a `name` built a selector that matched every
  test, and the failure was then labelled from that unrestricted run. Such
  a failure is now left out of the selection and reads `unverified`; when
  no failure can be selected, verification is skipped and says so.
- A test name can no longer repaint sooth's own lines. Control characters
  in text that comes from a report, the history or git (an ANSI escape, a
  carriage return, a newline) are printed as `�`, so a name carrying
  `ESC[2K` and a fake `result: ✓ PASSED` cannot erase and replace the
  verdict in a terminal or CI log. `--json` already escaped them.
- A proven flake no longer drops out of sight after a few pushes. The
  history looked at a test's last 50 *runs*, so three green `--runs 20`
  gates pushed the proof out. It now looks at the last 50 invocations with
  every run they made. When the history file is larger than sooth reads at
  once, the closing line says the count is its newest part.
- `failing since <commit>` now means what the README says: green before,
  red since. A test that failed every observed run is reported as `never
  seen passing (… first on <commit>)` instead of as a regression on the
  first commit sooth happened to see.
- A real break on a test that was once flaky is no longer filed under "all
  known flakes, nothing new". A proven flake that has failed every run
  since a commit, across three invocations or more and never passing on
  those commits, says so (`known flake (…), and every run since <commit> (3
  runs)`) and keeps the run from reading as nothing new.
- sooth never exits 101. A panic is reported as `sooth: internal error …`
  with exit 2, and a reader that closes the pipe (`sooth history | head`)
  ends sooth quietly with exit 2 instead of a panic message.
- A suite that is red everywhere no longer makes sooth itself slow. Listing
  the failed tests was quadratic in their number: 20,000 failures cost 16
  seconds per report, and now 0.3.
- A clean tree stays clean in the history. sooth read the code identity
  after the run and counted every untracked file, so the report a run wrote
  into the repository (the README's own `--junit report.xml` example) and an
  unignored `.sooth/` marked every observation dirty, and no evidence ever
  accumulated. The identity is now read before the first run, `.sooth/` and
  the `--junit` file are left out of the dirty check, and a `HEAD` that moved
  during the run is recorded as unknown. The check no longer takes git's
  index lock.

## [0.3.0] - 2026-11-12

### Added

- `sooth run --preset phpunit -- php artisan test` works, with Pest or
  PHPUnit and with `--parallel`: the preset puts its reporter flags and the
  `--verify` selection after `artisan test` instead of after `php`, and the
  gate reads the PHPUnit version from the project's `vendor/bin/phpunit`.
- Pest works under `--preset phpunit -- vendor/bin/pest`: runs, `--runs`,
  `--verify` and the gate, checked against Pest 4. The gate no longer warns
  that it cannot read a PHPUnit version: for a program named `pest` it asks
  the `phpunit` next to it, the one Pest runs on.
- Prebuilt binaries for macOS and Linux, x86_64 and arm64, attached to
  every GitHub release, and an `install.sh` that downloads the right one,
  checks its SHA-256 and installs it to `~/.local/bin`. Installing sooth no
  longer needs a Rust toolchain.

## [0.2.0] - 2026-11-12

### Added

- Flaky detection, the core value: with a report source and `--runs N`, the
  report is parsed after every run and tests with mixed outcomes are ranked
  by failure rate. Always-failing tests are listed separately as broken —
  never called flaky. Skipped runs carry no signal.

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

- The `--json` shape gains additive `flaky` and `broken` arrays when a
  multi-run analysis ran (still `schema_version` 1).

- Local run history: every run with a report source appends one observation
  per test to `.sooth/history.jsonl` (opt out with `--no-history`), stamped
  with the git commit and a dirty flag. Mixed outcomes on one clean commit
  are reported as flaky per history; a green→red flip at a commit boundary
  as `failing since <commit>` — a regression pointer, never a flaky label.
  Gitignore `.sooth/`. The `--json` shape gains an additive `history` object
  when the pass ran.

- `--verify`: after a failing single run with `--preset`, re-run only the
  failed tests twice and split them into real failures, flaky-or-order-
  dependent ones, and unverified ones (the selection never re-ran them).
  The suite verdict and exit code are unchanged — sooth classifies, it
  never absorbs a failure. The `--json` shape gains an additive
  `verification` object when the pass ran (still `schema_version` 1). Not
  supported for the go preset yet; refused loudly.

- `sooth import --env <LABEL> [--commit SHA] <reports>`: bring JUnit reports
  produced elsewhere — a CI artifact, a colleague's run — into the local
  history. `--env` is required (sooth cannot tell where a downloaded file
  came from); `--commit` asserts a clean checkout so imported observations
  can combine with local ones into flaky proof. A content-hash ledger makes
  re-importing the same file a no-op, and a batch is written all-or-nothing.
  With local greens and CI reds on one commit, the history verdict becomes
  `every failure in ci`. Exit 0, or 2 when a file is unreadable or the
  history unwritable; never 1 — import judges nothing.

- `sooth run --changed[=BASE]`: the pre-push gate. Selects the test files
  that are new or changed against the base (explicit `=BASE`, else
  `@{upstream}`, else `origin/HEAD`; diffed from the merge-base, untracked
  files included) and hands their paths to the runner, repeated `--runs`
  times — a handful of tests twenty times is seconds, and catches a flake
  where it is born. Requires `--preset` and an explicit `--runs` (both
  refused loudly otherwise); with nothing changed it prints one line, exits
  0, and spawns nothing. Go selection is package dirs; jest gets the paths
  regex-escaped (jest reads positional arguments as patterns); the other
  presets take the file paths as-is. The selection survives non-ASCII names
  and runs from subdirectories, skips deleted files, and a failed git
  listing refuses loudly instead of gating nothing. With `--json` the
  document gains a `gate` object, and an empty gate still emits it
  (`"runs":[]`).

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

- The runner/report disagreement note now covers both directions: a failing
  runner whose report shows 0 failing tests is called out on stderr too —
  a swallowed failure would otherwise poison the history with false passes.

- Quarantine for known flakes: a committed `.sooth-quarantine` file (one
  test id per line, `#` comments) plus `--fail-on-flaky` make the run exit 0
  when every failure in every run's report is on the list — reported loudly,
  never hidden. Any unlisted failure, or a failed run its report cannot
  explain, still fails the run. Requires a report source; without the flag
  the file is inert. The `--json` shape gains an additive `quarantine`
  object when a pardon happened (still `schema_version` 1).

### Changed

- The minimum supported Rust version is 1.85 (was 1.80). It already was in
  practice: `clap` 4.6 requires 1.85, so a 1.80–1.84 toolchain could not
  build sooth despite the declared 1.80.
- `--verify` compares the failure's signature, not just pass/fail: a re-run
  that fails with a different kind or exception class than the suite saw is
  no longer `real (reproduced on re-run)` but `failed differently on re-run
  (TypeError in the suite, RuntimeException in isolation — not a
  reproduction)` — its own bucket, also in the JSON's `verification`
  object. Absent `type` attributes never manufacture a difference, and the
  failure message is deliberately not compared (volatile data would read
  real reproductions as different).

- The flaky pass claims only what the run sequence supports. A mixed
  outcome that flipped exactly once and never came back (the new state
  confirmed by at least two observations) leaves the flaky ranking and is reported as the distinct
  finding it is: `green up to run 2, then red for every later run (the
  environment may have changed between runs)` — the suite polluted itself;
  the test is predictable. The reverse flip reads the same way. And a failure
  observed in a single run while absent from the others is no longer
  "broken (1 of 1 runs now)" but `failed its only observed run (absent from
  the other 19 — a name that changes per run hides flakiness)`. The
  `--json` document gains additive `monotone` and `lone_failures` arrays.

- A pass whose report names zero tests says so instead of celebrating: the
  verdict becomes `result: ✓ PASSED — but the report shows 0 tests, so this
  run proved nothing`, in yellow rather than bold green. The exit stays 0 —
  the runner and its report agree nothing failed — but an empty suite, a
  filter matching nothing, or a runner pointed at the wrong directory no
  longer reads as proof.

- A test's state reads at a glance: every per-test line opens with a glyph
  that mirrors the verdict — ✗ attention now (new, real, broken), ~ flake
  evidence, ▼ failing since, ⊘ quarantined — and the verdict line closes
  with ✓/✗. The glyphs are single-width colored characters, not emoji:
  emoji render as boxes in common monospace fonts and cannot carry ANSI
  color, which is half the point. The namespace prefix of an identity is dimmed so the eye lands
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

- A run no longer cites itself as earlier evidence. The history now
  records which invocation made each observation (`"run"` in
  `history.jsonl`), and proves flaky only from a pass in one invocation and
  a failure in another, and "failing since" only from a streak across two
  invocations. A first `sooth run --runs 3` that goes fail, pass, fail no
  longer reads `known flake (2 of 3 in history)` next to "no observations
  from earlier runs yet". Rates still count every run.
- `sooth import --help` and the README no longer claim that `--log phpunit`
  reads `php artisan test` output. Laravel prints that through Collision, a
  different format that the parser refuses.
- The jest preset writes clean test identities: `wobbly` and
  `Cart totals::adds up` instead of ` wobbly:: wobbly` and
  `Cart totals adds up::Cart totals adds up`. With the old names `--verify`
  selected nothing and called every jest failure `unverified`. An empty
  `classname` in any JUnit report now counts as no classname. **Identities
  of jest tests change**, so earlier jest observations in `.sooth/` no
  longer match. The README now says jest needs `jest-junit` installed.
- `sooth run --changed --preset jest` runs the selected test files again.
  jest read the first selected path as another `--reporters` value and every
  jest gate exited 2; the paths now follow a `--`.
- `sooth import --log phpunit` records only failures the summary confirms.
  A green log that echoes PHPUnit-shaped text (`1) Class::method`) no longer
  records a failure for a test that passed, and a red log whose failure
  listing is missing or incomplete — a truncated or regrouped CI log — is
  refused (exit 2) instead of being announced as green.
- A crash in the middle of writing `history.jsonl` no longer merges two
  observations into one phantom. The next run starts on a fresh line, and a
  file that already holds a glued line is read back as the complete record
  it contains plus one unreadable fragment, which the usual warning reports.
- A multi-byte character no longer crashes sooth (exit 101). `sooth import`
  panicked on a `timestamp` with one at byte 10, and every command that
  shows a short commit panicked on a non-ASCII commit in a hand-edited
  `history.jsonl`. Both now degrade the way the rest of the parsing does:
  an unparsable timestamp falls back to the file time, and a commit is cut
  at seven characters instead of seven bytes. Slicing a string by byte
  offset is now denied by clippy, so the class cannot return.
- `sooth import --commit` with an abbreviated sha — the form every CI
  screen shows — is resolved against the local repository and stored as
  the full sha, so imported observations actually combine with local ones.
  Stored as typed it could never match a local observation and the import
  counted in totals while proving nothing, silently. A sha the repository
  does not know is refused (exit 2) instead of stored, and so is a branch or
  revision name, which would resolve to where it points now rather than to
  the commit CI ran on. A full sha in capitals is stored in lowercase, as
  local runs record it.
- A multi-file gate on PHPUnit older than 10 is refused loudly (exit 2)
  instead of silently gating only the first file: old PHPUnit takes a single
  positional path and drops the rest, which made the gate print every
  selected file while proving only one — a false green. The version is
  probed via `--version` only when the phpunit preset selects two or more
  files; an unreadable banner warns and proceeds.

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
