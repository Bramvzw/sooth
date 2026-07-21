# Decisions

An ADR-style log of non-trivial choices. Newest entries at the bottom. Add an
entry whenever you make a decision that isn't obvious from the code, so
nobody (including future-you) re-litigates it without context.

## Why Rust

Fits the target-language diversity of the test suites `sooth` needs to
observe (it has to run alongside pytest, PHPUnit, Jest, Go — not compete with
their own ecosystem's tooling), gives a single portable binary with no
runtime to install, and matches the language of the recent star-magnets in
this space (`television`, `yazi`, `atuin`, `nextest`). It is also a
deliberate portfolio choice.

## Why local-first instead of a server/dashboard

Tools like ReportPortal and Allure take the server+dashboard+history shape
and plateau in adoption despite company backing. The k6/television/yazi shape
— local, instant, single-binary — is the one that pulls stars in this niche.
`sooth` commits to that shape: no service to run, no account, no history
store. It observes one run and reports on it.

## Why the name `sooth`

Short for "in sooth" (truly) — matches the "the truth about your tests"
positioning without borrowing a generic testing term. The crate name and
GitHub repo (`Bramvzw/sooth`) were claimed early to reserve the name.

## Presets instead of pure framework-agnosticism

There is no real JUnit-XML standard, and producing that report isn't free for
every runner (pytest and PHPUnit emit it natively; Jest needs `jest-junit`;
Go needs `gotestsum`). Claiming "framework-agnostic, works everywhere with no
setup" generates a stream of "doesn't work for me" issues that a solo
maintainer can't sustain. Instead, `sooth` promises: works out-of-the-box
with pytest, PHPUnit, Jest and Go via built-in presets that inject the right
reporter flags; bring your own JUnit XML for everything else.

## Flaky and order-dependence are strictly separated

Shuffling test order and repeating runs are different signals that pollute
each other if combined: a test that only fails under certain orderings looks
"flaky" under shuffled repeats. `sooth` never shuffles and repeats in the same
pass. Flaky detection uses a fixed order repeated N times; order-dependence
detection uses shuffled orders compared against each other, with no
repetition. See `ROADMAP.md` for how this maps to versions.

## Network egress as a separate spike, decoupled from the launch

Per-test attribution of network calls is the hardest part of the whole
project: all tests run in one subprocess, so an external observer (proxy or
sniffer) can't tell which test made a given connection without either
per-framework hooks (which breaks the agnostic promise) or running every test
in its own process (which is slow). HTTPS also requires the target process to
honor a proxy or trust a MITM certificate, and reliable interception
mechanisms (eBPF) are Linux-only for now. False negatives — reporting "no
egress" when there was one — are the worst failure mode for a tool whose
entire value proposition is telling the truth.

Given that, egress detection is a separate, hard-timeboxed (3-4 week) R&D
effort that starts Linux-only, proxy-honoring-clients-only, with documented
limitations. It ships as its own release if it succeeds; the v1.0 launch does
not wait for it and stands on flaky + slow + order-dependence alone.

## Work is tracked in GitHub Issues, not a ticket board

`sooth` is a personal OSS project — no JIRA, no external board. Work is planned
GitHub-native:
- **Milestones = versions** (`v0.1 skeleton`, `v0.2 flaky`, `v0.3 slow+order`,
  `v1.0 launch`, `egress spike`).
- **Epics = theme issues** (label `epic`): Core run pipeline, Flaky detection,
  Slow & order analysis, Distribution & packaging, Launch & docs, Network egress.
- **Stories = issues under an epic** (label `story`), each assigned to a milestone.
- Commit subjects use the `PREFIX:` convention (no ticket numbers); optionally
  reference an issue with `(#n)`.
- Overview lives in GitHub milestones (source of truth) + optionally a GitHub
  Project board; `ROADMAP.md` holds the human-readable narrative.

## clap (derive) for the CLI, and the test command after `--`

The CLI uses `clap`'s derive API. The wrapped test command is captured as a
trailing argument list after `--` (`sooth run -- pytest -k foo`), modeled with
clap's `last = true`. This keeps sooth's own flags (`--preset`, `--runs`, …)
unambiguous from the flags of the command it wraps. Flags sooth cannot honor
fail loudly instead of being silently ignored — a tool whose brand is telling
the truth must not pretend to honor a flag. (Originally `--preset`, `--json`
and `--slowest` were rejected as "not implemented yet"; superseded once
presets landed in story #4 — the rule lives on as "a report-dependent flag
without a report source is an error".)

## The runner inherits the child's stdio and captures only exit status + time

`sooth`'s runner spawns the test command with inherited stdio, so you see your
test output exactly as if you had run it yourself, and records only the exit
code and wall-clock time per run. It deliberately does not buffer the child's
output: the structured signal comes from the JUnit XML the runner produces
(parsed in story #3), not from scraping stdout. Runs execute in a fixed order;
shuffling for order-dependence is a separate pass (see above). Since the v0.1
architecture audit the runner runs exactly *once* per call: repetition is an
orchestration strategy (the v0.2 flaky loop parses the report between runs;
the v0.3 order pass runs different shuffled invocations), so the loop lives
with the orchestrator, not the spawner.

## Pinned Rust toolchain instead of rolling `stable`

`rust-toolchain.toml` pins an exact version (e.g. `1.96.1`) rather than
`stable`. Under `clippy -D warnings`, every new stable Rust can introduce lints
that fail CI on a change that didn't cause them (this bit us once already).
Pinning makes CI reproducible and turns a toolchain upgrade into a deliberate,
reviewable bump. The file is the single place to change: local builds pick it
up automatically, and CI installs it explicitly with a bare `rustup toolchain
install`. Caveat that bit us: toolchain actions (`dtolnay/rust-toolchain` and
friends) export `RUSTUP_TOOLCHAIN`, and that environment variable overrides
`rust-toolchain.toml` — with the action at `@stable`, CI silently ran rolling
stable despite the pin. The main CI jobs therefore avoid toolchain actions;
the MSRV job keeps one (`@1.80.0`) precisely because that override is what an
MSRV check needs.

## Exit codes distinguish "the tests failed" from "sooth failed"

`sooth run` exits `0` when every run passed, `1` when at least one run failed,
and `2` when sooth itself could not do its job (the command could not be
spawned, the report could not be parsed, or a flag cannot be honored yet).
Grep-style: CI can tell a red suite apart from a broken invocation. Fixed
before v0.1 so the codes never have to change under users' feet.

Refinement (story #57): with a report source, "the suite passed" requires the
runner *and* its report to agree. A runner that exits 0 while its report
contains failures/errors (misconfiguration, suppress-exit-code plugins, a
wrapper swallowing the status) makes sooth exit 1 and say so loudly on
stderr — printing "1 failed" while exiting 0 would be two truths at once, and
CI reads exit codes, not warnings. Not exit 2: sooth *could* do its job here
(it knows a test failed), and CI treats 2 as an infra error with different
retry/alert behavior. The rule is monotone — a failure is never upgraded to
success, a clean pair stays 0 — and doubles as v0.2's per-run outcome
definition: a run failed iff the runner exited nonzero or its report shows
failures. Users who deliberately configure exit-0-on-failure runners see
their pipeline turn red behind sooth; that is the product, not a bug. An
explicit escape hatch (e.g. `--exit-code=runner`) waits for real demand.

## `quick-xml` (event-based, not `serde`) for the JUnit parser

There is no real JUnit-XML standard (see above): the union schema `sooth`
needs is "accept a `<testsuites>` or bare `<testsuite>` root, ignore anything
unrecognised, default missing/invalid values instead of failing." That shape
does not map cleanly onto one `#[derive(Deserialize)]` struct — a serde
mapping would need one shape per dialect plus glue to try each in turn, which
is more moving parts than the tolerance rules justify. `quick-xml`'s
event-based `Reader` is used directly instead: a single pass over
`Event::Start`/`Event::Empty`/`Event::End` tracks a generic nesting depth (to
detect truncated/unclosed input) and, whenever a `<testcase>` is open, the
first-seen `<error>`/`<failure>`/`<skipped>` child that outranks whatever was
already recorded. This is iterative, not recursive, so pathologically deep
nesting cannot blow the stack — a real fuzz-test concern for a parser that
promises never to panic. `quick-xml` was chosen over alternatives
(`xml-rs`, `roxmltree`) for its combination of a pull/event API (fits the
tolerant single-pass design), MSRV compatible with `sooth`'s 1.80, and no
required dependencies beyond `memchr`.

A related, non-obvious guard: `Duration::from_secs_f64` panics on negative,
infinite, or NaN input. A JUnit `time="-1"` or `time="nan"` attribute is
exactly the kind of malformed-but-plausible input the parser must survive, so
`time` parsing explicitly checks `is_finite() && >= 0.0` before constructing
the `Duration`, defaulting to zero otherwise — the same fallback already used
for a missing `time` attribute. The same function normalizes a decimal comma
(`time="12,5"` from non-English locales) before parsing: without it those
durations silently become zero and the affected tests vanish from the slowest
ranking — a quiet lie in a ranking that claims to be the truth.

Two more guards that are easy to miss in the code: `parse_file` refuses any
report larger than 256 MiB (`TooLarge`) *before* reading it, so a pathological
or hostile file cannot exhaust memory — real reports are kilobytes to a few
megabytes. And when a `<testcase>` carries more than one status child
(tolerated, rare), the highest severity wins: error > failure > skipped —
never the first or last one seen, so attribute order in the wild cannot flip
a verdict.

## Root-element and truncation detection via a depth counter, not tag-name tracking

Accepting both `<testsuites>` and bare `<testsuite>` roots, plus arbitrary
unknown wrapper elements, means the parser cannot assume a fixed shape for
"did this input have a real root." Instead it tracks two independent,
cheap signals over the single event stream: whether a `<testsuite>` or
`<testsuites>` start/empty tag was ever seen (`MissingRoot` if not — this is
what turns empty input and non-XML text into an error instead of a silently
empty report), and a generic open/close depth counter incremented on every
`Event::Start` and decremented on every `Event::End` regardless of tag name
(`UnexpectedEof` if it is non-zero at `Event::Eof` — this is what turns
truncated XML into an error instead of a partial, silently-accepted report).
Neither check depends on `quick-xml`'s own leniency about unmatched tags at
end-of-input, which is what makes truncation detection reliable.

## Hand-rolled JSON for `sooth run --junit --json`, not `serde_json`

Report output: colored terminal table + `--json` is its own later story
(general reporting for every `sooth run`). This story only needs to honor
`--json` for the JUnit summary it adds, and that shape is small and fixed
(run outcomes, totals, a list of `{name, duration_seconds}`). Adding
`serde_json` for one story's narrow, fixed-shape output is not worth a second
serialization dependency; a small hand-rolled formatter (with a dedicated
`json_escape` for names) covers it. This is revisited once the general
`--json` report lands.

## Local run history amends "observes one run and reports on it"

The local-first entry above says sooth "observes one run and reports on it".
That framing conflated two different promises: no *hosted* history (server,
account, dashboard — still a hard non-goal) and no history at all. The second
half is dropped. Flaky detection needs many observations, and demanding them
via `--runs N` prices the core feature at N× the suite's wall-time — a 5-minute
suite costs 50 minutes to interrogate. Meanwhile teams already run their tests
dozens of times a day; the observations exist, sooth just has to keep them.

So sooth may append per-test observations (identity = JUnit `classname` +
`name`) to a local, user-managed history file (e.g. `.sooth/history.jsonl`)
that never leaves the machine or repo unless the user moves it themselves
(CI cache or artifact). Flaky detection gets two feeds into the same
failure-rate ranking: fixed-order repeats (active, answer now) and accumulated
history (passive, zero marginal wall-time). This turns sooth from an episodic
lab instrument into a flight recorder — the difference between a tool used
once and a tool used daily.

Guardrail: sooth reports what the history shows; it never silently hides or
auto-retries a failure the way retry plugins do. That dishonesty is exactly
what sooth positions against.

## PHP/Laravel is the launch beachhead

Framework-agnostic stays the architecture, not the spearhead of the story. In
the Rust world `cargo-nextest` already ships retries with flaky reporting, and
pytest has a rich plugin landscape — there sooth is "the same but uniform", a
weak pitch. PHP/PHPUnit has neither, and it is the maintainer's daily
environment, so the dogfood story lands there naturally. "The flaky-test tool
PHP never had" is a sharper message than "works with everything". Presets keep
all four runners first-class; marketing (README order, launch channels) leads
with PHP.

## Preset injection goes right after the program name

A preset adds reporter flags to the user's command. They are inserted directly
after the program name, before the user's own arguments: safe for pytest,
PHPUnit and Jest (options may precede arguments) and required for gotestsum,
which stops parsing its own flags at `--`. Jest is the odd one out twice: the
report path travels via `JEST_JUNIT_OUTPUT_FILE` (jest-junit reads its
configuration from the environment), and `--reporters=default` is injected
alongside `--reporters=jest-junit` so the console output the user knows stays
intact — the runner keeps inherited stdio (see above).

The report goes into a fresh, private per-invocation directory under the
system temp dir (mode 0700 on Unix, unpredictable name). Fresh, because a
stale report left behind by a crashed earlier run must never be parsed as this
run's truth; private, because the classic shared-`/tmp` pre-creation/symlink
trick must find no predictable target. The directory is best-effort removed
after parsing; a user's own `--junit` file is never touched. `--preset` and
`--junit` are mutually exclusive (clap `conflicts_with`): a preset manages its
own report, and pointing sooth at a second file at the same time is
contradictory input. clap usage errors exit 2, matching the exit-code
contract.

Known limitation, stated loudly instead of failing confusingly: injection
assumes the program *is* the runner. Wrappers (`python -m pytest`, `npm test`,
`php artisan test`, `poetry run pytest`) would receive the flag themselves and
break — so the `--preset` help text says the command must be the runner
itself, and a preset run that produces no report fails with an actionable
hint rather than a bare parse error about a temp path. Wrapper detection can
come later if real-world issues show it is needed.

## `--json` shares stdout with the runner: last-line contract or a file

Inherited stdio is a core decision (see above): the wrapped command writes its
own output to sooth's stdout, so machine JSON on the same stream necessarily
mixes with it — `sooth run --json ... | jq` broke on the first real pytest
run. Redirecting the child away from stdout would undo "you see your test
output as if you ran it yourself"; JSON on stderr abuses the diagnostics
stream. So the contract is explicit: bare `--json` prints the JSON as the last
line sooth writes to stdout, after the wrapped command has finished (works for
`tail -n 1` consumers), and `--json=PATH` writes it to a file — the robust CI
path — while keeping the human report on stdout. The shape carries
`schema_version` (fields are only added within a version; the number bumps on
an incompatible change) and `sooth_version`. The hand-rolled-JSON decision was
revisited here as promised and kept: the shape is still small and fixed;
revisit again if it grows nested or dynamic.

## A stale `--junit` report is an error, not input

`--junit` means "the report this run produces". Freshness is checked as an
observed fact, not a clock comparison: sooth records the file's state (mtime
plus size) before each run and rejects the report with exit 2 when the run
did not change it — the runner most likely wrote nothing (wrong reporter
flag, crash), and presenting yesterday's suite as today's truth is the worst
failure mode for this tool. State comparison replaced an earlier wall-clock
check with a 60s tolerance: it needs no tolerance window at all, is immune
to clock skew against a network filesystem's server, and works per run under
`--runs N`, where a wall clock cannot (run 1's write is always "recent" by
run 2). Filesystems without mtimes skip the check; a false "stale" on a
fresh report would be its own lie. Presets get the stronger guarantee: their
report file is deleted before every run, so a runner that stops writing
fails loudly with the no-report message instead of re-serving the previous
run's file.

## Color: `--color` beats `NO_COLOR` beats terminal detection

An explicit `--color always|never` is the user speaking now and wins over
everything. Otherwise `NO_COLOR` (set and non-empty, per no-color.org)
disables color; otherwise color only when stdout is a terminal. The per-run
line says `runner exit=N` — never a bare `exit=N` — because `2` means
something else in sooth's own exit-code contract and the two vocabularies
were confused in practice on the first real run. ANSI codes are hand-rolled:
six escape sequences do not justify a color dependency.

## Flaky is mixed outcomes; always-failing is broken, never flaky

A test is flaky iff the observed fixed-order runs show at least one pass
*and* at least one failure or error. A test that fails every observed run is
broken — reported in its own section, never in the flaky ranking: calling a
dead test "flaky" would misstate its nature exactly where this tool promises
truth, and it would send someone hunting for nondeterminism that is not
there. Skipped observations carry no signal and are excluded from the
failure rate. The ranking sorts by failure rate, then id, for stable output;
the healthy majority — tests that passed every run — is not listed at all.
The suite verdict considers every run's report: a failure in run 1 is not
forgiven by a green run 2. This same mixed-outcome definition is what the
v0.2 history file will apply across sessions instead of within one.

## Flaky evidence requires one clean commit; regressions get "failing since"

History spans commits, so "40 green, then red" is ambiguous: nondeterminism
or a regression? Labeling a regression "flaky" sends someone hunting
nondeterminism that isn't there — the exact sin the flaky/broken distinction
guards against, extended to the time axis. So every observation records the
code it ran on (git `HEAD` plus a dirty flag; untracked files count as dirty
because they are code the commit does not describe), and the classification
is strict about proof:

- Mixed outcomes **within one clean commit** prove flaky — same code,
  different result, no other explanation.
- All green until commit X, all red from X on (a trailing streak of at least
  two, anchored on a clean commit) reports **"failing since X"** — a
  regression pointer, never a flaky label.
- Observations on dirty or unknown code count in the totals but are never
  evidence: the code they ran on is unknowable afterwards. One new red
  observation concludes nothing; the verdict converges over subsequent runs
  (or instantly via active `--runs N`).

Git stays optional — no git, no repo, no problem: identity degrades to
unknown and sooth simply makes no identity-bound claims. No new dependency,
no network call.

Mechanics that keep the passive layer passive: recording is on by default
whenever a report source exists (`--no-history` opts out) because passive
accumulation is the point — an opt-in flag would kill the flight-recorder
value. Write or read failures degrade to a stderr warning and never change
the exit contract. The file is append-only JSON lines, hand-written and
hand-parsed like the `--json` output (same no-serde reasoning); unreadable
lines are counted and skipped, never fatal. Analysis looks at each test's
last 50 observations, and loading reads at most the file's last 64 MiB —
the file is append-only and never pruned by sooth, so the read must be
bounded or every run would pay for the entire past. Drift ages out; the
user prunes (or doesn't) a file they own.

## Verification re-runs only the failures; a missed test is never "real"

`--verify` is the layered flaky model's active classifier: after a failing
run it re-invokes the runner on just the failed tests — seconds of re-running
instead of N× the whole suite. Two verification runs, not one: a single pass
already proves "not a deterministic failure", but a second run also catches a
test that is flaky under isolation too (fails the suite, fails verify run 1,
passes verify run 2). The count is fixed; a `--verify-runs` knob can be added
additively if a real need appears.

Restricting a runner to a subset means mapping JUnit identities
(`classname::name`) onto each runner's selector, and that mapping can over-
or under-match. Selection therefore favours over-matching — extra passing
tests merely confirm they pass — and under-matching is never silently
swallowed: a failed test the verification never actually re-ran is reported
as `unverified`, never as `real`. A test that is only *skipped* during
verification was likewise never re-run and lands in `unverified` — skips
carry no signal, here as everywhere. Concretely: phpunit selects on the
method half anchored to `::` — PHPUnit writes its JUnit `classname` dotted
while `--filter` matches the backslashed FQCN (observed live on a production
suite: a full-identity filter matched zero tests), and the method name is
the only half that survives both formats; data-provider rows still match
because `with data set …` follows the method name; pytest selects by method
name with any `[parameter]` suffix stripped
(brackets and spaces would break a `-k` expression, and the base name merely
over-matches) because a JUnit classname is a dotted module path, not a node
id; jest's `-t` matches test names. The go preset declines selection for now
— `-run` must sit after gotestsum's `--` split, and deferring beats placing
it wrong — and that refusal happens at flag validation, before any suite
run is wasted.

Verification is a diagnosis on top of the verdict, never part of it: every
failure mode degrades to a stderr warning and no classification, and the exit
code is untouched. It needs `--preset` (only then does sooth control the
command shape) and conflicts with `--runs N` (the flaky pass already answers
that question). Verification never shuffles — order-dependence stays a
separate pass. Verification runs do not feed the history file either: they
execute a hand-picked subset in isolation, and recording an isolated pass as
an ordinary observation would mint "flaky per history" evidence from a
context the passive layer never saw.

## Quarantine is a committed pardon list; the exit is steered only when asked

Day one in an existing codebase finds twenty flaky tests and nobody can
merge — so known flakes live in a committed `.sooth-quarantine` (unlike the
gitignored `.sooth/` history: the pardon list is a team decision and belongs
in review), and `--fail-on-flaky` pardons them. The rule is all-or-nothing
per invocation: the run exits 0 iff every failure in every run's report is
on the list, and every failed run is explained by its report — a failed run
with a green or missing report is never pardoned, because sooth cannot
attribute that failure to a known flake. The failures are still printed,
and the verdict says why the exit is 0 ("only quarantined flakes failed");
exit steering happens only behind the explicit flag — the file alone changes
nothing, per the guardrail that sooth never silently absorbs a failure.

Attribution has a documented limit: the pardon trusts the report to be the
complete story of why the runner failed. A runner configured to fail on
signals the report does not record (phpunit's `failOnWarning`/`failOnRisky`,
a lint step inside the test command) can be masked by a pardon — sooth
cannot see what the report does not say, the same report-is-truth limit the
rest of the tool lives with. The one detectable subcase is closed: a
signal-killed run is a crash and is never pardoned, even when the report
was written before the kill.

The format is plain lines (one id per line, `#` comments), not TOML: no new
dependency (the hand-rolled-JSON reasoning), trivially diffable, and the ids
are pasted verbatim from sooth's own output — exactly as reports write them,
because identities must round-trip byte-for-byte (the phpunit dotted-vs-
backslashed lesson). A missing file is the normal day-one state (empty
list); an unreadable file warns and pardons nothing — failing the run is
the safe direction. History records pardoned failures as failures: the
record keeps the truth, the pardon only steers the exit.
