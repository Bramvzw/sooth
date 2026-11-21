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

Sooth only ever guaranteed that *it* does not shuffle — it does not choose the
order at all, the runner does. And runners reorder by default: PHPUnit's
`--order-by=defects` promotes tests that just failed, so run 2 differs from run
1 exactly when `--runs 2` is used to investigate a failure. So the pass now
checks its own precondition by comparing report order across runs, and when it
did not hold it makes a **weaker claim** rather than the same claim with a
disclaimer: `flaky` becomes `flaky or order-dependent`, the vocabulary
`--verify` already uses for this ambiguity. The finding keeps its place in the
ranking — a test that is only green in one order is worth reporting either way,
and telling users to change their flags to suit the tool is the wrong direction.

Only the tests two runs share are compared, so a run cut short by
`--stop-on-failure` is not mistaken for a reordering, and duplicate ids count at
their first appearance. Runner-agnostic by construction: no runner name, no flag
inspection, no threshold on how much difference is too much.

Measured before choosing this: paratest (Laravel `--parallel`) merges its
per-worker reports in a stable order, so parallelism alone does not trip it, and
PHPUnit `--random-order` does change report order across seeds. pytest-xdist,
jest and `go test -parallel` are unmeasured, so the design leans on neither. No
suppression flag until a runner proves noisy — the cost of being wrong here is a
weaker true statement, not a false one (same reasoning that deferred
`--verify-runs`).

Enforcing a fixed order instead of checking for one was rejected: presets inject
reporter flags, not behaviour changes; it would silently override a deliberate
`--order-by=defects`; jest has no clean equivalent; and with `--junit` sooth
controls nothing. Checking works everywhere, enforcing works in some places.

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
- **Milestones = versions** (`v0.1 skeleton`, `v0.2 flaky`, `v0.3 laravel`,
  `v0.4 slow+order`, `v1.0 launch`, `egress spike`).
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
the v0.4 order pass runs different shuffled invocations), so the loop lives
with the orchestrator, not the spawner. (Repeated runs no longer inherit the
terminal: see "Repeated runs are quiet".)

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
the MSRV job keeps one (pinned to the MSRV) precisely because that override is what an
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

Refinement (#231): nothing leaves through Rust's `101`. A panic is sooth
failing, so `main` catches it and exits 2, with one line that says it is a
bug and where to report it. A reader that closes the pipe (`sooth history |
head`) also arrives as a panic — the failed `println!` — and exits 2 as
well, but without a word: nobody is left to read it. Not 0, because a `run`
whose report could not be delivered must never look like a pass, and not
the tests' own verdict, because the reader can leave before there is one.
Any Unix tool under `set -o pipefail` fails the same pipeline (141).

That contract is tested against the binary, which needs a panic to test it
with. The test used the one input known to cause one, a `time` no duration
can hold; that input no longer panics (#249), and none is known that does.
So a debug build panics when `SOOTH_TEST_PANIC` is set. It is compiled out
of a release build: the binary a user runs has no such switch.

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
that never leaves the machine or repo unless the user moves it themselves.
Evidence from elsewhere travels as *report files*, not history files: CI's
job is to retain the JUnit report it can already write, and `sooth import`
is the door (see the import entry). Wrapping sooth around the test command
inside CI remains possible — for `--fail-on-flaky` gating, a different job —
but it is not the evidence route: it puts sooth in the critical path of
someone else's build, which the local-first entry above exists to avoid. Flaky detection gets two feeds into the same
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
come later if real-world issues show it is needed. (It did, twice: see
"Laravel's `artisan test` is the one wrapper the phpunit preset knows" and
"A Composer script gets sooth's arguments after `--`".)

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
  observation concludes nothing; the history's verdict converges over
  subsequent invocations. `--runs N` answers instantly, but through the
  active pass, not the history (see "One invocation is one piece of
  evidence").

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
last 50 invocations (see "One invocation is one piece of evidence"), and
loading reads at most the file's last 64 MiB —
the file is append-only and never pruned by sooth on its own, so the read
must be bounded or every run would pay for the entire past. Drift ages out;
the user prunes (or doesn't) a file they own — since #246 with a tool for
it, see "Pruning is the user's decision and sooth's job".

Time order comes from each observation's `at`, not from file position: the
analysis sorts (stably — one run stamps all its observations alike, so ties
keep run order) before classifying. File position originally doubled as the
time contract, which silently broke the moment lines from elsewhere were
appended — old evidence landed at the tail, exactly where failing-since
reads. A user merging files owns no ordering obligation; the timestamp
already in every line does.

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
carry no signal, here as everywhere. Selection receives the raw `name`
attribute carried separately from the report, never re-split from the joined
`classname::name` identity — a name may itself contain `::` (a jest title,
say), so the join is one-way and any re-split is lossy (#91). Concretely:
phpunit selects on that name half — PHPUnit writes its JUnit `classname`
dotted while `--filter` matches the backslashed FQCN (observed live on a
production suite: a full-identity filter matched zero tests), and the name
is the only half that survives both formats; data-provider rows still match
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

Amended when explain landed (below): the list is read on every failing run,
not only under `--fail-on-flaky`, so its entries can *label* a failure as
known. "The file alone changes nothing" was about exit steering, and that
half stands unchanged — a label is knowledge, not a pardon.

## The report is organised per test, not per pass

Each pass used to print its own section, so a test several passes had an
opinion about appeared several times, each time with a different word attached
— and two independent questions got presented as competing labels:

- *what kind of problem is this* — flaky, broken, failing since a commit
- *did I know about it already* — new, known from history, quarantined

A test can be broken **and** never seen before. Printing those answers in
separate sections produced lines that read as contradictions ("failed all 3
runs" in one section, "new (the history holds no evidence)" in another), and
made a report grow with the number of passes rather than the number of
problems.

So a failing test gets one line carrying both answers, and the sections that
used to hold them — the repeat pass's ranking, verification's verdict, the
pardon list — are gone. What the history knows about tests that did *not* fail
this run keeps its own short section: that is genuinely other news.

This subsumes three fixes that were made by hand while the sections still
existed: the history section skipping this run's failures, the pardon note, and
the duplicate pardon list. Each was a patch on a layout that kept producing
them.

The `--json` shape stays organised per pass and is unaffected: it is frozen at
`schema_version` 1, machines are not bothered by repetition, and a consumer
that wants one view per test can build it from the `explanation` object.

## An observation records where it was made

The failure that hurts most is the one that passes locally and fails on
release. Sooth could not see it: `.sooth/history.jsonl` lives in the directory
you run from, so a laptop and a CI runner accumulate separate files, and an
observation carried `commit` and `dirty` but nothing about its surroundings.
Concatenating a CI history into a local one — trivial, it is JSON lines —
produced an undifferentiated pile in which "passed 40, failed 6" hid the only
fact worth knowing.

So an observation carries an environment, derived rather than configured: `ci`
when `CI` is set and non-empty, `local` otherwise. That variable is the
de-facto standard across GitHub Actions, GitLab and CircleCI, reading it makes
no network call, and it needs nothing from the user on day one. A `--env`
override for teams with several meaningful environments stays available
additively; adding it before anyone asks would be guessing at their taxonomy.

The claim it enables is deliberately narrow: **every failure came from one
environment, and another environment observed the test too**. Both halves
matter. Without a second environment "all failures were local" says no more
than "all runs were local", and a report that draws attention to a
distinction that does not exist is worse than silence. Where the split is
even, nothing is said — an evenly flaky test is just flaky.

Observations written before this carry no environment, and they are read as an
*unknown* environment rather than folded into a labelled one. That keeps two
promises at once: old histories load unchanged (an absent field is not a
corrupt line), and a history that predates the feature cannot produce a
confident claim about environments it never recorded. Same shape as `commit`
and `dirty`: unknown counts in the totals, never as evidence.

A rule that discards evidence has to admit when it discarded all of it.
Measured on a real repo: 9423 observations, every one made on a dirty tree,
and every failure reporting "new (nothing in history)" without a word about
why. Correct on both counts and useless — a history that proves nothing reads
exactly like a history that is empty, and the two are fixed differently (one
fills up on its own, the other never will). So when *every* prior observation
is unusable, the report says so and names the remedy. Only then: one clean
observation is enough for the history to speak for itself, and a note on every
run would be noise. This is the same honesty as the empty-history note — the
absence of a claim must never pass for the absence of a problem.

What this does not do is find the cause. Resources, parallelism, timezone,
seeding, a service that exists in one place only — sooth points at the
difference worth investigating and stops there.

## Explaining a red run: a lookup, never a new conclusion

Finding flakes is not the daily pain; being blocked by them is. So every red
run ends by labeling each of its failures against the evidence that already
exists — proven flake (with its rate), `failing since <commit>`, quarantined,
or new — under a headline that answers the only question a red build asks:
is any of this new? It costs nothing: the history pass and the quarantine
list are already loaded, and the pass is a lookup, never a second analysis.
Reusing the history analyzer verbatim is the point — one definition of
"proven flaky", one of "regression", applied wherever sooth speaks.

Consequences that keep it honest:

- A regression is *known* but never "nothing new": it is a real failure, so
  it is counted and colored apart, and it disqualifies the all-clear
  sentence. Folding it in would be the flaky/broken sin on the time axis.
- The categories partition the failures (a quarantined proven flake counts
  once, as a flake), so the counts always sum to the failure count.
- "New" is only meaningful against evidence. An empty history, or one that
  was not consulted (`--no-history`), says so in a note instead of letting
  absence of evidence read as evidence of novelty.
- The exit code and the verdict are untouched, as with verification: sooth
  classifies failures, it never absorbs them.
- Nothing is said twice. A failure carries its history verdict on its own
  line, so the history section drops it and reports only the flakes that
  stayed quiet this run. Two sections printing the same counts buries the
  run's own news under background.
- "Nothing new" plus exit 1 is a contradiction on its face, and it happens
  whenever `--fail-on-flaky` meets known flakes that are not on the list.
  The report resolves it in place — the pardon rests on the committed list,
  never on sooth's own evidence — and names the file to add the ids to.
  Letting history evidence pardon instead would turn a build green on a
  judgement no human reviewed, which is precisely why the list is committed.

`sooth explain --junit <PATH>` is the same pass over a report you already
have — a CI artifact, a colleague's run, a preset run whose temp report you
kept. It exists because a preset's report is deleted after the run, so
without it the beachhead user (PHPUnit, `--preset`) could never ask the
question twice. It runs nothing and *records* nothing. The principle behind
that, stated once so `import` can share it: **every run is recorded at most
once, by its observer.** Explain never observes a run — whoever executed it
already recorded it — so writing would mint duplicate observations, evidence
out of thin air. Import (see its entry) is the sole observer of a foreign
report, which is exactly why it may write where explain must not. Its
exit is 0 whenever the report could be read (2 when it could not); a
diagnosis that changed CI's verdict would be a second, hidden gate. Freshness
is not checked either — pointing explain at an old report is the use case,
not the error `sooth run --junit` guards against. Its `--json` is a plain
flag rather than run's `--json[=PATH]`: no wrapped command shares this
stdout, so there is no last-line contract to honor and the whole output can
simply be the JSON.

## Import brings foreign reports in; the observer records

The failure that matters most passes locally and fails on release, and no
single machine's history can see it. The route that was rejected: run sooth
inside the pipeline and merge history files. It puts sooth in the critical
path of someone else's build (against local-first), it needs nothing sooth
is good at (fetching and merging is plumbing), and merged files silently
broke the analyzer while append order doubled as the time contract. Instead,
the transport is the *report*: CI retains the JUnit file it can already
write, and `sooth import --env <LABEL> [--commit SHA] <reports>` reads it
into the local history. Downloading is `gh`'s job — sooth makes no network
calls (`SECURITY.md`).

Import records because it is the first and only observer of these runs (see
the explain entry for the principle). What keeps it honest:

- `--env` is required. Sooth cannot tell where a downloaded file came from,
  and a guessed label would poison the environment evidence; the one thing
  worse than no claim is a confident wrong one.
- `--commit` is optional but load-bearing: it asserts a clean checkout, so
  imported observations can combine with local ones into flaky proof.
  Without it they count in totals and are never evidence — the same rule
  dirty local runs live under. One commit per invocation; guessing shas from
  filenames would be magic. An abbreviated sha is resolved against the local
  clone and stored in full — local runs record the full sha, and the history
  compares commits exactly, so an abbreviation stored as typed would never
  combine with anything. One the clone does not know is refused, not stored.
  So is a branch or revision name: it resolves to where it points at import
  time, not to the commit CI ran on — a wrong commit that looks like proof.
- A content-hash ledger (`.sooth/imported`, FNV-1a 64 — hand-rolled because
  `DefaultHasher` is not stable across Rust versions and the input is the
  user's own files) makes re-importing the same download a no-op instead of
  double evidence.
- Validation happens before any write: an invocation imports all of its new
  files or none of them, so a typo in one path cannot leave a half-recorded
  batch.
- `at` comes from the report's own `timestamp` attribute when present
  (pytest writes one; PHPUnit does not), else the file's mtime, else now —
  and the analyzer orders by `at`, so arrival order stopped mattering.
- Exit codes: 0 or 2, never 1. Import judges nothing; failing to write *is*
  failing at the job, unlike `run`, where history is a side effect that must
  never change the verdict.

## Console logs are evidence too — failures only, never passes

A CI that never wrote a JUnit report still printed one thing worth keeping:
the failure blocks in its console log, full test identity included, retained
by GitHub for 90 days. For the codebase that prompted this, that was 259 red
integration-branch runs — three months of CI failure history — available
without changing one line of the host project. That last part is the
principle this feature serves: **sooth never requires the host project to
change for sooth's sake.** Presets inject reporter flags at runtime, import
consumes what already exists, and now the log route mines what CI keeps by
default. Asking a team to add `--log-junit` + an artifact upload stops being
a prerequisite and becomes the gold standard for those who also want rates.

`sooth import --log phpunit` reads those logs. What keeps it honest:

- **Only failures are recorded.** A console log names its failures (the
  `N) Class::method` blocks) and reduces every pass to an anonymous dot, so
  failures are witnessed facts and passes would be deductions ("not named as
  failed" also covers skipped, crashed, and never-ran). Sooth records
  observations, never inferences — the same rule that keeps skips and dirty
  runs out of the evidence. The pass half of a flaky proof comes from where
  passes are witnessed: local runs, or JUnit artifacts when they exist.
- **No denominator, and no pretense of one.** A log-mined history can say
  *that* a test fails in ci, never how often it passes there. Until a pass
  arrives on a shared commit, a log-only test honestly reads as `failing
  since` — and flips to `known flake (… every failure in ci)` the moment one
  clean local green lands.
- **An anchor or a refusal.** Without a recognizable PHPUnit summary line
  (`OK (…`, `Tests: …, Assertions: …`, `FAILURES!`) the file is not treated
  as a PHPUnit log: exit 2, loudly. A green log is valid and records
  nothing, and says so. Sooth does not guess — a fabricated identity in the
  history is worse than a refused file.
- **The summary settles the listings before it.** A `N) Class::method` line
  is only evidence when the summary agrees. Under a green summary (`OK (…`,
  `OK, but …`, `WARNINGS!`) there were no failures, so a listing that summary
  closes is echoed test output and is dropped. Under a red one the listing
  must name at least as many failures and errors as the `Tests:` line
  counts; fewer means a truncated or regrouped log, which is refused (exit 2)
  rather than recorded as partial or announced as green. Inside a listing,
  entries must count up from 1 to the header's N — an identity out of that
  sequence is output the test printed, not an entry.
- **Framework-scoped, like the presets.** The failure-block format is
  PHPUnit's; other runners get their own parser when a real codebase needs
  one, not before. The identity is converted to the JUnit form (backslashes
  to dots in the class half only) so log evidence and report evidence meet
  on the same ids.
- **Acquisition stays outside.** `gh run list` + `gh run view --log-failed`
  fetch; sooth reads local files (`SECURITY.md` unchanged). The log's own
  per-line timestamps date the observations; mtime and now remain the
  fallbacks.

## A test's state reads at a glance

Dogfooding on a production Laravel suite showed the per-test lines stop
being scannable once real identities arrive: a 100-character namespace
buries which test it is, and the verdict exists only as a phrase to read.
Two presentation moves, no information destroyed:

- A glyph opens every per-test line and mirrors the partition the exit logic
  already uses: ✗ attention now (new, real, broken), ~ flake evidence
  (look, don't panic), ▼ failing since a commit boundary, ⊘ quarantined,
  and ✓/✗ on the verdict line. Glyphs survive pipes and `NO_COLOR`, like
  PHPUnit's own ✗. Precedence when the axes disagree:
  quarantine > known flake > failing since > observed flaky > attention.

  The first cut used emoji (❌ 🎲 📉 🚧) and a real terminal refuted it the
  same day: emoji render as anonymous boxes in common monospace fonts, and
  they cannot carry ANSI color at all — so the per-test lines lost their
  color layer exactly where the glyph was meant to reinforce it. Single-width
  characters every monospace font has, painted in the verdict's color, do
  both jobs; with `--color never` the characters stay and only the paint
  drops.
- The namespace prefix of an identity is dimmed (everything up to the class
  name), so the eye lands on `ClassTest::method` and the counts. Every byte
  stays: grep and copy-paste keep working, and with `--color never` the
  output is byte-identical to before.

The machine contract is untouched — `--json` and the exit codes carry the
same facts; the human report is for humans.

## The gate repeats only what changed — prevention priced in seconds

The ideal is that a flaky test never reaches CI. One local suite run cannot
deliver that: a 1-in-20 flake survives it with 95%, and the ~59 runs that
would catch it (see the README's odds) cost hours on a whole suite. The
affordable version exists because flakes are overwhelmingly *born* — in the
commit that adds or changes a test — and a push changes only a handful of
tests. `sooth run --changed[=BASE] --runs 20` repeats exactly those: seconds
of work, high odds, before anyone else is blocked. Witnessed the same week
this shipped: a flake born in a factory-selection change, discovered in CI,
hidden behind a rerun, fixed hours later — a 20× gate on the changed file
would have caught it at the desk.

Choices that keep it honest:

- **A flag on `run`, not a subcommand.** A gate is a run: it spawns tests,
  records observations (pre-push trees are usually clean, so they count as
  evidence), and lives under the same 0/1/2 exit contract. Everything —
  the runs loop, the flaky analyzer, the report, `--json` — is reused.
- **Selection by file paths, not name filters.** Every supported runner
  takes paths (go takes the package dirs), unlike `--verify`'s name-regex
  selection, which go cannot do. Changed *source* files do not select
  anything: mapping code to affected tests is test-impact analysis, a
  different tool; the gate's claim is about tests being born, and the limit
  is documented rather than silently half-covered.
- **Loud refusals instead of silent defaults.** No preset: refused (which
  files are tests is runner knowledge). `--runs` left at 1: refused — "a
  gate of one run proves nothing" — because a gate that silently proves
  nothing is worse than none.
- **A failed git listing refuses, never gates nothing.** `git diff` or
  `ls-files` failing (an index lock, a corrupt repo) is exit 2, not an empty
  selection — the same reasoning as the `--runs 1` refusal. The listings run
  with `-z` and `--relative` and skip deletions, so the selection is what
  the runner, spawned in the cwd, can actually open. (`-z` replaced
  `core.quotepath=false`, which stops git quoting non-ASCII bytes but not a
  quote or a backslash: such a file left the selection without a word,
  #182.)
- **A selected path is handed over as a path.** PHPUnit and pytest get
  `./<path>`, so a test file named `--bootstrap=xTest.php` cannot read as
  an option (#170); git's paths are always relative, which makes the prefix
  always right. jest gets its paths behind a `--` and, being regexes
  matched against absolute paths, unprefixed; go gets `./<dir>` already.
- **The empty gate spawns nothing.** No changed tests → one line, exit 0,
  no runner started: a pre-push hook must cost nothing on the pushes that
  change no tests. It still honors `--json`: nothing ran is a result, so
  the document is emitted with `"runs":[]` and the `gate` object — a machine
  consumer never parses a stale file or a human sentence.
- **The base resolves like a human would**: explicit `=BASE` wins, then
  `@{upstream}`, then `origin/HEAD`, then a loud refusal. The diff is taken
  against the merge-base, so commits on the base branch never read as yours.
- What the gate cannot catch, CI evidence still does: flakes that need CI's
  own conditions (clock, machine, parallelism) — the third layer next to
  the history and the quarantine, not a replacement for them.

## The gate probes PHPUnit's version before selecting several files

PHPUnit before 10 takes a single positional path and silently ignores the
rest (`TextUI/CliArguments/Builder.php` reads `$options[1][0]` only). A gate
that hands it two changed files would repeat one, print both, and exit 0 —
the false green the gate entry above calls worse than no gate (#139).

So when the phpunit preset selects **two or more** files, the gate runs
`<program> --version` once and reads the major out of the banner:

- **Older than 10 → refused, exit 2.** The message names what would have
  been silently dropped and the fix (upgrade to PHPUnit 10+).
- **Unreadable banner → a stderr warning, then proceed.** Refusing on
  ignorance would break exotic-but-working setups; a warning keeps the limit
  out loud without punishing them.
- **One file, or any other preset → no probe.** The probe costs a spawn, so
  it runs only when the failure mode exists.

A `--filter` fallback for old PHPUnit (deriving class names from file
basenames) was rejected: filename→class is a convention, not a guarantee,
and a gate built on a guess can under-select — the exact failure this
decision exists to prevent. Revisit only if PHPUnit ≤ 9 gate demand shows up.

## A zero-test pass keeps its exit but loses its celebration

A run whose report names zero tests used to end in a bold green
`result: ✓ PASSED` (#141). Every cause of an empty report is a mistake —
a `--filter` matching nothing, an empty testsuite in the config, a runner
pointed at the wrong directory — and for a gate it is a false green with
real consequences: the selection printed files, the report proved none of
them.

The exit stays 0: the runner and its report agree that nothing failed, and
that agreement is the whole exit contract. Failing the run (1) would blame
the tests for a vacuum, erroring (2) would blame sooth for the user's
configuration, and runners that consider an empty selection an error
(pytest exits 5) already fail the run through the runner's own exit. What
changes is the sentence: the verdict says `— but the report shows 0 tests,
so this run proved nothing`, in yellow rather than bold green. Vacuous
truth is still truth, but it is not proof, and sooth must not dress it up
as proof.

## The flaky pass reads the shape of the sequence, not only the rate

Two dogfooding findings shared one root: the pass reduced each test's runs
to a pass/fail tally, and the tally supports weaker claims than it made.

- Fifteen tests went green→red at the same run and stayed red — a suite
  polluting its own environment (a reused database checksum), reported as
  fifteen "flaky" tests, sending the reader hunting for nondeterminism that
  is not there (#113). Real flakes bounce; this flips once and never back.
- A test whose name carries per-run random data exists once per name: its
  failing sighting read as "broken (1 of 1 runs now)" — the word for
  fails-every-run, claimed on one observation — while its passing twin
  under the next name hid the flake entirely (#137).

So the pass now keeps each test's per-run sequence and claims only what the
shape supports. Mixed outcomes that flipped exactly once, the new state
confirmed by at least two observations ("never back" needs a chance to have
come back — a single trailing flip stays a flake), leave the flaky ranking:
`green up to run 2, then red for every later run (the environment may have
changed between runs)` — the reverse flip reads the same way. A failure observed
in a single run while *absent* from others is neither flaky nor broken:
`failed its only observed run (absent from the other 19 — a name that
changes per run hides flakiness)`. Present-but-skipped elsewhere stays
broken — a skip is a stable name, and only true absence questions identity.

Normalizing identities instead (stripping data-set suffixes so drifting
names merge into one test with a real flaky claim) was rejected: sooth
would be guessing that two names are one test, the guess can merge rows
that are genuinely different tests, and history, quarantine and explain all
key on the full identity. Facts over inference, the same call as the gate's
PHPUnit probe. The `--json` document gains additive `monotone` and
`lone_failures` arrays beside `flaky`/`broken`.

## A re-run that fails differently reproduced nothing

`--verify` compared pass/fail and nothing else, so a test that failed the
suite with an assertion and died in isolation on a bootstrap error read as
`real — reproduced on re-run` (#103). Nothing was reproduced: the second
failure is an artifact of tearing the test out of its context, and on
suites with shared bootstrap state this misfires systematically — always
toward the confident label.

Verification now compares the failure's *signature*: its kind
(`<failure>` vs `<error>`) plus the exception class from the report's
`type` attribute. Same signature → `real`, as before. Different →
`failed differently on re-run (TypeError in the suite, RuntimeException in
isolation — not a reproduction)`, its own bucket in the report and the
JSON. Two boundaries keep it honest: absent metadata never manufactures a
difference (a report without `type` attributes compares on kind alone),
and the failure *message* is deliberately not compared — messages embed
volatile data (ids, faker values), so a message comparison would read real
reproductions as different. A pass on re-run still outranks everything:
one green isolation run says more than any signature. Among failing
re-runs, one exact reproduction is a reproduction (#172): `failed
differently` is only for a failure that *no* re-run reproduced. The first
version let a single differing re-run win, which threw away a reproduction
sooth had in hand — the one direction this entry says it must never lean.

The third case the issue names — failed alone with the *same* signature
while the suite's normal order was green — is order-dependence and belongs
to #111 (v0.4), not to verification.

## Layering invariants are clippy lints, not a tidy test

The invariants in `AGENTS.md` are phrased as absences — only `report.rs`
prints, only three modules spawn processes — because an absence cannot be
read off the code. The two that break most easily are enforced by clippy
the way cargo enforces its own (`print_stdout`, `print_stderr`,
`disallowed_methods` with a `clippy.toml`), not by a hand-written test that
greps the source: clippy resolves paths, so an aliased or fully qualified
call is caught where a grep would miss it, and the check costs no code of
ours.

An exception is an `#[expect]` on the narrowest item that needs it, and
`#[allow]` is denied (`clippy::allow_attributes`): an expectation that no
longer fires fails the build, so an exception cannot outlive the code that
needed it. `#[expect]` needs Rust 1.81; the MSRV is past that (see below).

## The MSRV is what the dependencies require

`rust-version` is a promise: a toolchain at least that old builds sooth. It
had quietly stopped being true — `clap` 4.6 (Dependabot, #79 onward)
declares `rust-version = "1.85"`, and `cargo check` on 1.80 fails on
`clap_lex`'s edition-2024 manifest, while the declared MSRV stayed 1.80.

The MSRV is therefore the highest `rust-version` in the locked dependency
tree, 1.85 today, not a number chosen apart from it. Holding an older one
would mean pinning `clap` below 4.6 and fencing it off from Dependabot —
standing maintenance, and missed fixes, for users on a toolchain older than
February 2025. A dependency update that raises the floor raises the MSRV in
the same PR.

The job that checks it runs `cargo check --all-targets --locked`: the
committed lockfile, exactly, or a failure — never a fresh resolution of a
dependency set nobody ships (#207). That the job can fail was checked once,
in CI, by running the same command on Rust 1.84: it stopped on
`clap_lex`'s `edition2024`, as it should. An earlier run had been green on a
toolchain that could not build the lockfile locally; the likeliest reason is
that the two were not building the same lockfile (the CI log showed cargo
resolving), which is what `--locked` now turns into an error.

## One invocation is one piece of evidence

The N runs of one `sooth run --runs N` were stored as N observations with
one commit, one `dirty` flag and one `at`, and the history pass had no idea
they belonged together. Two things went wrong (#161):

- A first-ever `--runs 3` that went fail, pass, fail read `known flake (2 of
  3 in history)` next to `no observations from earlier runs yet`. The
  history was answering "did sooth know this already" with the run it was
  explaining.
- For a monotone sequence the active pass refuses to call flaky (see "The
  flaky pass reads the shape of the sequence"), the history pass said
  *proven flaky* in the same report, from the same runs, and kept saying it.

So every observation carries the invocation that made it (`"run"` in
`history.jsonl`: one `sooth run`, or one imported file), and the history's
claims need more than one invocation:

- **Flaky** needs a pass in one invocation and a failure in another, on the
  same clean commit. Mixed outcomes inside one invocation are the active
  pass's to judge, with the shape rules it already has.
- **Failing since** needs its trailing red streak to span at least two
  invocations.
- **Counts stay per run.** `failed 4 of 9 observed runs` keeps every run:
  the rate is a measurement, and only the proof needs independent
  invocations.

Lines written before the field existed have no invocation and each stands
alone, so an existing history keeps the evidence it had.

The window follows the same unit (#228): the analysis looks at a test's
last 50 *invocations*, with every run they made. Counted in runs, the
`--runs 20` the gate itself recommends used up the window in under three
pushes and a proven flake dropped out of sight; gate runs over new,
untracked test files are dirty and filled it without ever being evidence.
When the file is larger than one load reads, the report says the count is
the newest part and not all of it.

Collapsing the N runs into one stored observation was rejected: it throws
away the per-run counts, and which status would the collapsed observation
carry for fail, pass, fail? Keeping the runs and asking the proof to span
invocations loses nothing.

## Prebuilt binaries ship with every release, ahead of v1.0

The roadmap put distribution at the v1.0 launch. It came forward because the
first users sooth is built for are PHP and Laravel developers, and most of
them have no Rust toolchain: `cargo install sooth` is a dead end for exactly
the people the beachhead names. A tool nobody can install gets no feedback,
and the feedback is what the analysis passes and v1.0 need.

Every release now carries static binaries for macOS and Linux, x86_64 and
arm64 (`.github/workflows/binaries.yml`), and `install.sh` downloads the one
for the machine, checks its SHA-256 against the published checksum, and
installs it. What shaped it:

- **Hand-written, not `cargo-dist`.** cargo-dist generates a large workflow
  of actions pinned by tag, owns the GitHub release, and wants to be
  regenerated on upgrade. Here every action is pinned to a SHA, and
  `bin/release.sh` already creates the release with its notes. Four targets
  in one matrix are cheaper to own than a generator.
- **Built after the approved publish.** The binaries job needs the publish
  job, so a tag nobody approved gets neither a crate nor binaries. A pull
  request that touches the build runs the same matrix without uploading.
- **musl on Linux.** One static binary runs on any distribution and in CI
  containers, with no glibc version to match.
- **No Homebrew tap yet.** A tap is a second repository whose formula must be
  updated on every release, which means a write token for it in CI, the one
  long-lived secret the release pipeline does not have. It waits until
  users ask for it.
- **No Windows build.** sooth is tested on Unix only; shipping an untested
  binary would break the "tell the truth" promise on day one.

## Laravel's `artisan test` is the one wrapper the phpunit preset knows

The beachhead is PHP and Laravel, and a Laravel developer runs `php artisan
test`, not `vendor/bin/phpunit`. Under the rule "the command must be the
runner itself" sooth refused the command its first users type every day, and
the documented way around it (paratest with `--runner` and an environment
variable) was not something anyone would adopt.

Checked against Laravel 13 with Pest 4: `artisan test` forwards every option
after the subcommand to Pest or PHPUnit, `--log-junit` included, and with
`--parallel` paratest merges the workers into one report. The only thing that
broke was where sooth put the flag: right after the program, so `php` read
`--log-junit` as its own option. So, for the phpunit preset only:

- **Injection point.** When the program is `artisan`, or the script `php`
  runs is `artisan`, and the next argument is `test`, the reporter flags and
  a `--verify` selection go after `test`. Everything else is unchanged.
- **Version probe.** `artisan test --version` prints Laravel's version, so
  the gate asks the project's `vendor/bin/phpunit` (next to `artisan`), the
  one that will actually run. For a program named `pest` it asks the
  `phpunit` beside it, for the same reason: Pest prints its own banner.

This is recognising one well-known command shape, not general wrapper
detection: `npm test`, `composer test` or `python -m pytest` can hide any
command, and guessing what they run would break the "never guess" rule.
`artisan test` is one fixed command with documented forwarding. (Composer
scripts followed, without a guess: see "A Composer script gets sooth's
arguments after `--`".)

## The identity is the code the run started on, and sooth's own files are not changes

An observation is evidence only when it was made on a clean, known commit.
Two things made clean trees read as dirty (#229, #102):

- The identity was read *after* the run. Whatever the run wrote into an
  unignored path — the report in the README's own `--junit report.xml`
  example — was then an untracked file, and the observation was dirty.
- An unignored `.sooth/` did the same from the second run on.

For such a project the history silently proved nothing, and the note told
the user to "commit or stash" a tree they had not touched. So:

- **Read before the first run.** The identity describes the code that is
  about to run; nothing the run writes can pass for a change to it. If
  `HEAD` is different once the runs are over — a commit or checkout during
  a long suite — the observations cannot be pinned on either commit and are
  recorded as unknown.
- **Sooth's own files are not changes.** `.sooth/` and the user's `--junit`
  file are left out of the dirty check. This does not weaken the rule that
  untracked files count: those two are the only paths sooth knows it and
  the run write. A cache or coverage directory the suite leaves behind is
  still an untracked file like any other, and belongs in `.gitignore`.
- **No index lock.** The check runs `git --no-optional-locks status`: a
  plain `status` refreshes the index under `index.lock` and can collide
  with an IDE or a commit in another terminal while a suite runs.

The evidence no longer depends on `.sooth/` being ignored. Keeping it out
of `git add -A` is the next entry.

## A streak says what was witnessed: regression, never seen passing, or a flake gone red

The history's streak check made two claims the observations did not support
(#226, #227):

- A test that failed from its first observation read `failing since <first
  commit seen>` and was counted as a regression. The label's own definition
  is "green until a commit, red ever since"; without a green there is no
  commit to blame, and `git show` on that commit sends the reader to the
  wrong place.
- A test that had ever been proven flaky was never looked at again: flaky
  proof returned first. When such a test really broke, every run read
  `known flake` and the verdict was "all known flakes, nothing new" until
  the old proof left the window.

So the trailing streak is read for every test, and labelled by what came
before it:

- **Green, then a red streak** across two invocations: `failing since X`.
  Unchanged, and now the only case that is called a regression.
- **Nothing but red** in the window: `never seen passing (failed all N
  observed runs, first on X)`. X is where sooth first looked, said as such.
- **A proven flake, red on every run since a commit**: the flake label
  stays — it is still true — and the streak is added to it: `known flake
  (4 of 8 in history, 50%), and every run since X (3 runs)`. Sooth does not
  conclude "regression": a 50% flake fails three times in a row one time in
  eight. It does stop saying "nothing new", because that is the one place
  where being wrong hides a real break.

What keeps the third case from firing on an ordinary flake: the streak only
counts failures on commits the test was never seen passing on (a failure on
a commit that also passed is the flake being a flake), and it needs three
invocations, not two (`RED_STREAK_INVOCATIONS`): two reds in a row is what
a 50% flake does one time in four.

In `--json` these are additions: verdict `never_passed`, `red_since` on a
known flake, and `never_passed` in the counts and under `history`.

## Sooth never requires the host project to change for sooth's sake

Presets inject reporter flags at runtime instead of asking for config, no
sooth file has to be committed, `import` consumes reports that already
exist, and the log route mines what CI keeps by default. The untracked
`.sooth/` was the one place that broke the rule: the README told the user
to add a line to the project's `.gitignore`, and a warning that says "go
edit your project" is that requirement in disguise (#102).

Git has a mechanism made for this: `.git/info/exclude` — per-clone ignores,
never committed, invisible to teammates. So when sooth creates `.sooth/`
and `git check-ignore` reports it not ignored, sooth appends `.sooth/` to
that file and says so, once:

```
sooth: added `.sooth/` to .git/info/exclude — the history is local and yours, and this keeps it out of `git status` and `git add -A`
```

Boundaries: only at the moment the directory is created, so a later run
never spawns git for it; best-effort and never fatal, like every other
history failure; silent outside a repository; a project that already
ignores `.sooth/` by either mechanism is left untouched. Committing the
line to `.gitignore` stays a fine team choice — sooth just never depends
on it. It is the only file outside `.sooth/` that sooth writes, and it is
one line in the clone's own metadata.

## A Composer script gets sooth's arguments after `--`

`composer test` is the script the Laravel skeleton ships, and many teams run
their suites only through such scripts (`composer test-central`, a script
per tenant type). Under "the command must be the runner itself" sooth put
`--log-junit` right after `composer`, which Composer read as its own option:
the run died before a test started (#232, and #104 before it).

The artisan entry above ruled Composer out because a script "can hide any
command, and guessing what it runs would break the never-guess rule". That
rule stands, and nothing here guesses:

- **What runs is the user's word.** `--preset phpunit` says the script ends
  in PHPUnit, Pest or paratest. Sooth never reads `composer.json` and never
  infers a preset from the word `composer`.
- **Where arguments go is Composer's documented behaviour.** Options in
  front of a `--` are Composer's; what follows it is appended to the
  script's commands. So when the phpunit preset's program is `composer` (or
  `php composer.phar`), the reporter flags, a `--verify` selection and the
  gate's files go after the `--` — the user's own, or one sooth appends.
  Only the program position counts: `phpunit tests/composer` is a path.
- **Version probe.** `composer --version` prints Composer's version, so the
  gate asks the project's `vendor/bin/phpunit`, as under `artisan test`.

The limit, said out loud: Composer appends the arguments to *every* command
of a script unless one is marked `@no_additional_args`. Laravel 13's
skeleton marks its `config:clear` line; Laravel 12's does not, so there
`config:clear --log-junit …` fails and no report is written. Sooth cannot
route around that without rewriting the user's script, so the run ends in
the existing loud "no report" error — which, for this shape, names
`@no_additional_args` and the way out (wrap the runner itself) instead of
asking whether `composer` is the test runner.

With a second wrapper, what sooth knows about a command moved into one
place: `preset::Plan`, resolved once per run from the command and the
preset. Injection, the `--verify` selection, the gate's files, the version
probe and the wording of a missing report all ask it, so a further command
shape is one entry in one `match` rather than a check in three files (#247).

## The gate refuses a parallel command on its first use

paratest — what `--parallel` runs under `artisan test` and Pest — accepts
exactly one path. A gate hands the runner one path per changed test file,
so `sooth run --changed -- php artisan test --parallel` worked while one
test file had changed and died on paratest's `Too many arguments, expected
arguments "path"` the day two had: exit 2, the push blocked, and nothing
that named the cause (#233). The README advertised that very command.

Two ways out were on the table:

- **Drop the flag and say so.** Friendlier, and wrong in practice: checked
  on Laravel 13, `--processes=4` and `--recreate-databases` are unknown
  options once `--parallel` is gone, so sooth would have to know and strip
  every parallel-only option of every paratest version. It would also be
  the first time sooth removes something the user typed.
- **Refuse, with the reason and the fix.** Chosen. It is the neighbour of
  the PHPUnit < 10 refusal above: the gate never hands a runner paths it
  will not run.

The refusal comes **before the selection is resolved**, so even a push that
changes no tests is refused. That is on purpose: a hook that works until
the day two test files change fails at the worst moment, while one that is
refused the first time it runs gets fixed once, at setup. Nothing is lost —
a handful of files gains nothing from parallel workers, and their startup
(a database per process) costs more than the tests.

What is recognised, under the phpunit preset only: `--parallel` or `-p`
among the arguments the runner receives, and a program named `paratest`
(which has no flag to drop, so the message names `vendor/bin/phpunit`).
Another preset's `-p` is another option — pytest loads a plugin with it.

What cannot be recognised is a `--parallel` inside a Composer script. That
gate still fails on its second changed file. Sooth does the one thing left:
when a Composer script gated on two or more files comes back without a
report, the error adds that paratest takes a single path, as the first
thing to check.

## A command that names its runner names its preset

The most natural first command, `sooth run -- php artisan test`, ran the
tests and added two lines: no totals, no failure list, no history, and not a
word about `--preset` being what turns a run into an analysis (#236). The
audit's newcomer got nothing in their first minute.

`pytest`, `vendor/bin/phpunit`, `vendor/bin/pest`, `gotestsum` and `php
artisan test` say which runner they are. The preset list encodes exactly
that fact, so making the user repeat it was a formality. Without `--preset`
or `--junit`, sooth now reads the preset from the program's own name and
says which one it took, on stderr, before the run:

```
sooth: using `--preset phpunit`, inferred from `php artisan test`
```

This is a lookup, not the guess the "never guess" rule forbids. The line
between the two:

- **Only the program position names a runner** (`artisan test` being the one
  known two-word form, as the program or behind a PHP binary). `composer
  test`, `npm test`, `make test`, `sh -c pytest` and `python -m pytest`
  *might* run a known runner; sooth does not look inside and infers nothing.
  Composer is a deliberate case: the phpunit preset can drive a Composer
  script, but only on the user's word. `sail artisan test`, `ddev artisan
  test` and the like are another: they run in a container, where a report
  path on this machine means nothing, so `artisan` behind any program but
  PHP is that program's argument — for an explicit `--preset` as well.
- **An inference must not break a command that works.** Two named runners
  are therefore left alone. `jest`: its preset needs the `jest-junit`
  package, and injecting the reporter into a project without it stops jest
  before a test runs. And a command that already asks for a report of its
  own — `--log-junit build.xml`, `--junitxml=…`, `--junitfile …` on the
  command line, or where the runner reads more of the same: pytest's
  `PYTEST_ADDOPTS`, gotestsum's `GOTESTSUM_JUNITFILE`. The runner writes one
  report, sooth's flag would win, and the file the user asked for would go
  missing.
- **An explicit flag is never second-guessed**, and is silent. The
  announcement is the price of not typing it; a hook that wants one line
  less passes `--preset`.

One case stays out of reach: a report set up in a config file (`<junit
outputFile=…>` in `phpunit.xml`, `addopts` in `pytest.ini`). Sooth reads no
project configuration, so it does not see it, and a preset's flag takes
that report's place for the run — as it always did under an explicit
`--preset`. The README says so, with the way to keep the file: `--junit
<that file>`.

For everything that stays without a report source, the run closes with one
stderr line naming the flag that adds one — tailored where sooth knows
more: `--preset jest` with its package for jest, `--junit build.xml` for
the command that already writes that file, `--preset phpunit` for a
Composer script. Anything else is pointed at `--junit` first, because a
preset only helps when the command is the runner itself. Stdout keeps its
contract.

`--preset pest` is accepted as an alias: Pest runs on PHPUnit and takes the
same flags, and its users look for their runner's name. clap used to answer
with "a similar value exists: 'jest'", which sent them to the wrong preset.

Known limit: there is no flag to force the old report-less run of a known
runner. The only setups that would want it are ones where the runner cannot
write JUnit at all (pytest with `-p no:junitxml`); add an opt-out when one
shows up, not before.

## The phpunit selection matches a name in any spelling its report can have given it

`--verify` selects by the `name` a report carries, and under Pest that name
is not the method's. Pest's JUnit logger prints a class-based test the way
PHPUnit's TestDox does: the `test` prefix goes, underscores and camelCase
humps become spaces, the first letter is raised. `test_that_true_is_true`,
`testThatTrueIsTrue` and an attribute-marked `that_true_is_true` all read
`That true is true`, and `--filter '/::(That true is true)/'` matches none
of them. In a Laravel + Pest app every class-based failure, the skeleton's
`ExampleTest` first, came back `unverified` (#234).

The report does not say which spelling the method has, and the transform is
not reversible (`testFoo` and `testFoo2` can both read `Foo`). So the filter
does not try to reverse it; it accepts every spelling that prettifies to the
name:

```
/::(?:test_?)?(That[_ ]?true[_ ]?is[_ ]?true|…)/i
```

An optional `test` prefix, each space an optional underscore or nothing, and
either case. That is the over-matching the verification entry above already
chose ("extra passing tests merely confirm they pass"), and it is why this
is one rule rather than a Pest mode: a plain PHPUnit name has no spaces
outside its `with data set …` suffix and still matches itself, a Pest-style
description is matched on its own words, and sooth never has to decide which
runner wrote the report.

The issue's other suggestion — select through the report's `class`
attribute — was not needed: the name alone finds the method, and anchoring
on the class would have reopened the dotted-versus-backslashed mismatch the
verification entry describes.

What this does not solve is the identity itself: the same test is
`Tests.Unit.ExampleTest::That true is true` under Pest and
`…::test_that_true_is_true` under plain PHPUnit, so history recorded with one
runner says nothing about runs made with the other. Sooth does not translate
between them: the README says to use the same runner locally and in CI.
Whether history and quarantine should compare such names by a common key
is a separate decision, still open in #235.

The contract test for this applies the filter with perl (PCRE, as PHP's
`preg_match`), because a test that only compares the pattern's text cannot
tell whether it matches anything. It is skipped where perl is missing.

## The gate refuses a selection it is sure of, and only that

The gate appends its files to the user's command. A command that already
selects tests — `vendor/bin/phpunit tests/`, `pytest tests`, `gotestsum --
./...` — then runs the union: the whole directory, `--runs` times, in a
pre-push hook, under `gate: 1 changed test file`. The printed selection no
longer describes what ran, and "a handful of tests twenty times is seconds"
becomes the suite twenty times (#138). The README documented it; the run
itself said nothing.

Refusing is the house rule, but a refusal has to be right: blocking a push
over a wrong guess is worse than the union it prevents. Telling a selection
from an option's value takes each runner's whole option table — `phpunit
--testdox tests/` is a selection, `phpunit --testsuite tests` is a value,
and plugins add options sooth has never heard of. So the gate refuses only
what needs no such table:

- the argument is not an option and does not follow one that may be waiting
  for a value (an option without `=`; a bare `--` waits for nothing), and
- it names something that exists on disk — or, for go, is a `...` package
  pattern, which is no path at all.

Everything else passes as before: `pytest -q tests/` looks exactly like `-q`
taking `tests/` as its value. That gap is stated in the README rather than
closed with a guess. A check that needs no option table would compare the
report's files with the selection after the run; PHPUnit's report carries
them, pytest's and jest's do not by default, so it is left for when the
report layer grows file information.

Like the paratest refusal, this comes before the selection is resolved, so
a hook with such a command is told on its first run.

## A jest test is known by its file, its describes and its title

A jest title is unique within a file at best. `a.test.js` and `b.test.js`
may each have `test('renders', …)`, and under the templates the preset
asked jest-junit for (`{classname}` and `{title}`) both were the test
`renders`: one history, one `failing since`, one quarantine entry that
pardoned both (#238). pytest and PHPUnit never had this problem, because
their classname is the module or the class.

The preset now asks for `JEST_JUNIT_CLASSNAME="{filepath} {classname}"`:
the path relative to jest's root directory, then the `describe` path. The
title stays on its own in `name`, because `-t` selects by it. A test reads
`src/cart.test.js Cart::renders`, or `src/cart.test.js::renders` outside any
`describe`.

That second form is why the parser now trims `name` and `classname`: the
template leaves a trailing space when there is no `describe`, and jest-junit
has no way to say "only if non-empty". The trim is general, not a jest
rule — whitespace at either end of a name is a template's leftover in any
report (jest-junit's own defaults leave a *leading* space on such a test,
which is the bug that made `-t` miss in v0.2), and a runner cannot be
pointed at a name that starts with a space anyway.

This changes every jest identity, which is why it happens now, before the
jest preset has users: history and quarantine ids recorded under the old
names stop matching, and there is no migration.

What `-t` selects is unchanged: the title, in any file. `--verify` may
therefore re-run a same-titled test from another file too, which is the
over-matching the verification entry chose.

## Real runners are tested in a layer of their own

The contract suite drives the binary with `sh` scripts that stand in for
runners. That pins what sooth does with a command and a report; it cannot
see how a real runner parses the command sooth builds, or what its reporter
writes. Every bug of the v0.3 audit's second phase lived there: Composer
reading `--log-junit` as its own option, paratest refusing a second path,
Pest prettifying a method name, jest-junit's templates, jest counting a
snapshot as a test. Each was found by hand, against a real project (#184).

`tests/smoke/` is that hand-work, kept: a three-test fixture per runner —
one steady test, one that fails on every second invocation, one that always
fails — and `run.sh`, which drives the built binary through `--runs`,
`--verify`, two history runs and the gate, and checks the verdict lines.
PHPUnit, Pest, a Composer script, jest and pytest each get a CI job.

Choices:

- **Not in `make check`.** It installs the runners and takes a network. The
  fast gate stays the thing one runs before every commit; `make smoke` is
  for a change to a preset, the gate or `--verify`.
- **The runners' current releases, no lock files.** A runner release that
  breaks sooth should be seen, and a lock file would hide it until someone
  bumped it. The price is that the job can go red on a pull request that
  changed nothing, so it is not a required check, and it also runs weekly
  on `main`, where such a failure has no pull request to be confused with.
- **Text checks in a shell script, not Rust tests.** What is asserted is
  the line a user reads (`known flake`, `real (reproduced on re-run)`);
  `grep -F` says that directly, and a failed check prints the whole run.
- **The flaky test counts invocations in a file outside the tree**, so its
  outcome is decided, the working tree stays clean, and the history flow
  can prove a flake on a clean commit the way a user's history would.
- **No Laravel application.** `artisan test` would need a full skeleton per
  run; its forwarding is covered by the Composer fixture (the same
  "arguments after the wrapper" shape) and by hand before a release.

## The report says only what was read and what was measured

Three lines on the screen claimed more than sooth knew (#243, #193):

- **`new (nothing in history)` under `--no-history`.** The history was not
  read, so "nothing in history" described a file nobody opened, and the
  headline's `1 new` counted a verdict nobody reached. When the history was
  not consulted, a failure with no other label gets none: the line ends at
  the test's name, or at what this invocation itself saw (`flaky (1 of 2
  runs now)`). The note under the list already said the history was not
  consulted; now nothing above it contradicts that. `--json` keeps its
  `unknown` verdict, which never claimed more.
- **`tests: 22 total — 12 passed, 9 failed` above `11 failures`.** With
  `--runs N` the totals are the final run's and the failure list covers all
  runs. Both are right, and together they read as a miscount, so the totals
  line says which run it counts: `tests (final run): …`.
- **`slowest tests: 1. … (0.00ns)`.** pytest writes `time="0.000"` for a
  sub-millisecond test and jest-junit `time="0"`; a missing attribute parses
  as zero too. That is "not measured", and a ranking of unmeasured tests is
  a list in file order under a heading that says otherwise. Zero durations
  are left out, and a ranking is printed only when it ranks — when the
  suite has more tests than the cut-off, or `--slowest` asked for it. Three
  tests have no "slowest ten"; a two-file gate no longer ends in one.

The JSON document is untouched by all three: it carries the numbers, and
these were statements about them.

## Repeated runs are quiet

"You see your test output exactly as if you had run it yourself" is the
right promise for one run. For twenty it is twenty copies of the same
output: a one-file gate on a fresh Laravel app printed 252 lines, and the
one line that mattered was the last (#240). In a pre-push hook the answer
scrolled away.

So with `--runs` above 1 the test command writes to a file in a private
temp directory instead of the terminal, and sooth prints:

- **one line per run, as it finishes** — the same `run 7/20: runner exit=0
  (0.47s)` line the report used to print afterwards, now doubling as
  progress;
- **the output of the first run that failed, once**, between the run lines
  and the report. Later failing runs say the same thing or fail another
  test, and the report names every failing test either way.

Choices:

- **The default, not a flag.** Bram's call, between three options: a
  newcomer's first gate should not be a wall of text they have to learn a
  flag to get rid of. `--show-output` restores the old behaviour for the
  day someone needs to watch run 13.
- **One run is untouched.** It still inherits the terminal, colours and
  progress included; nothing is captured and nothing can be lost.
- **A file, not a pipe.** Both streams go to one file through one shared
  handle, so they stay in the order the runner wrote them, with no reader
  thread and no buffer in sooth's memory while the run lasts. Only the
  first failing run's file is kept; each other run overwrites the last.
- **A run counts as failed by its exit code or its report**, the same two
  signals the verdict uses, so a runner that exits 0 over a red report
  still gets its output shown.
- **When sooth has to stop at a run, that run's output is shown first** —
  no report written, a `--junit` file the run did not touch, a report that
  does not parse. The error ends in "the runner's own output above likely
  explains", and in a quiet run there would have been nothing above. An
  earlier run's failure is shown before it: stopping at run 7 must not
  lose what run 2 said.
- **Both streams are replayed on stdout**, where the report is, in the order
  the runner wrote them. The exception is bare `--json`: there stdout is the
  document and nothing else, so the replay goes to stderr and no run lines
  are printed — the runs are in the document.
- **The capture directory is removed on every way out**, a panic on a
  closed pipe included: it is a value whose `Drop` deletes it.
- **`--show-output` without `--runs` is refused**, like every flag that
  would do nothing.

The cost: a runner that colours or animates its output only for a terminal
prints plainly into the file, so the one failing run shown may look
different from a run made by hand. And of several different failures across
the runs, only the first run's output is seen; the report's per-test lines
are what covers the rest.

## The gate is shared through the project's own tooling, not a sooth file

A pre-push hook in `.git/hooks` is not versioned, so every teammate had to
recreate the README's hook by hand, and the command itself was one long
line typed or copied each time (#241). For a team to adopt the gate, the
command has to live in the repository.

Three ways were on the table: a README recipe, a committed sooth file with
default arguments (so `sooth run` works bare), and a `sooth hook` command
that writes the hook. Bram chose the recipe:

- **The repository already has a place for commands.** A Composer script
  (`composer gate`), an npm script or a Makefile target is where a team
  keeps them, and `git config core.hooksPath .githooks` makes a committed
  hook the clone's hook. Nothing new to learn, and nothing of sooth's to
  commit — which keeps "sooth never requires the host project to change
  for sooth's sake" intact.
- **A file format is forever.** Default arguments read from a file need a
  name, a syntax, and precedence rules against the command line, and all of
  it has to keep working. With preset inference the daily command is
  already short (`sooth run -- php artisan test`); what was left of the
  problem was sharing, and sharing does not need a format.
- **`sooth hook` solves the typing, not the sharing.** The hook it writes
  still lives in `.git/hooks`.

Revisit the file when a team asks for defaults that a script cannot carry —
per-directory settings, say — not before.

## A usage error carries the correction, in clap's own error

clap parses the command line and words its own errors, and for two slips it
can only say `unexpected argument 'x' found`: a test command typed without
the `--` in front of it (`sooth run php artisan test` — the most natural
first command there is), and a value handed to `--json` or `--changed` with
a space where clap's optional-value flags need an `=`. The help text warned
about the second; the error, where the user actually stands, did not (#242,
#195).

`cli::with_tip` looks at the command line as typed and, for those two
shapes only, adds a tip to clap's error through the context clap itself
renders (`ContextKind::Suggested`), so the message keeps clap's layout, its
usage line and its exit code 2. The tip is the corrected command, not a
description of the rule: `sooth run --runs 3 -- pytest -k x`. No hand-rolled
parsing decides anything — clap still rejects the line; sooth only explains.
And the explanation is checked by clap too: the word may stand in more than
one place on the line (`--junit pytest pytest`), so each place is tried and
a corrected command is offered only if clap parses it. Only `sooth run` gets
a tip, and an unknown `--flag` never does: it is not a command.

`-h` and `--help` used to print the same paragraph per flag, because clap
takes both from one doc comment. Each flag's comment is now a one-line
summary, a blank line, and the detail: clap shows the summary for `-h` (the
place to look up a half-remembered flag) and all of it for `--help` (#194).
A unit test keeps every `-h` line of `sooth run` within 100 columns.

## A wrong import is replaced, not doubled

The import ledger skips a file it has seen, by content hash, so importing
twice is harmless. The same rule made a mistake permanent: a report imported
with the wrong `--env` or `--commit` was `skipped (already imported)` on the
second, corrected attempt, and its wrong observations kept counting (#244).

The issue offered a hint in the message or a `--force`. A flag that merely
imports again would leave the wrong observations next to the right ones —
worse than before, since the history would now hold a contradiction. So the
flag is `--replace`, and it does what the word says: the earlier import's
observations go, the new ones come in their place.

That is possible because every observation of an imported file carries the
file's invocation id (`import-<content hash>`, see "One invocation is one
piece of evidence"), so the ones to remove are named exactly. It is the
first time sooth rewrites the history instead of appending to it, which is
why the rewrite is as small and as safe as it can be:

- **One write.** The records that stay and the new observations go into a
  file beside the history, which is renamed over it. A crash or a full disk
  leaves the old history or the new one — never the old one with the file's
  observations gone and nothing in their place.
- **Nothing else is touched.** Every line that holds no record of the file
  is kept byte for byte: a fragment a crash left, a line that is not even
  text, a CRLF. Those are not this command's to judge. Only a line that
  glued one of the file's records to others is rewritten, as its other
  records.
- **Said when it is true.** The per-file lines (`…, replacing its earlier
  import`) are printed after the write succeeded.
- **The same file twice on one command line is one file**, with or without
  `--replace`.

Two cases need no replacing. With no history file left, `--replace` is a
plain import. With a history that holds no observation under the file's id
— a hand-edited one, or an import recorded before v0.3.1, when imports
carried no id — nothing is removed, the file's observations are added, and
sooth says exactly that rather than guessing which lines were the file's.

Another sooth writing to the history while a rewrite runs: the history has
no lock. Appends are single writes and have not needed one, and a lock
file has failure modes of its own (a stale one after a kill; the standard
library's file locks need a newer Rust than the MSRV). The two rewriting
commands, `import --replace` and `history --prune` (#246), read the file,
write their result beside it and rename that over it — so what another
sooth wrote in between was gone. For an import that loss was permanent:
its ledger says the file is in, and a second import answers "already
imported" (found in review, reproduced with a prune of 300 MB).

A rewrite now takes the history's place only when the file is still the
one it read: the same file, the same length, the same modification time.
Otherwise it fails with "the history changed while it was being
rewritten", writes nothing, and the answer is to run it again. What is
left of the window is the instant between that check and the rename, and
an append that had opened the file before that instant and writes after.

Each rewrite writes its temp file under its own process id, where one
shared name had two rewrites truncate each other's file and rename a mix
of both over the history. The temp file is removed when the rewrite
fails, when sooth panics, and when it is stopped: both rewriting commands
listen for SIGINT and SIGTERM as a run does, and stopped they leave the
history as it was. A signal after the rename has nothing left to stop.
Only a kill that cannot be caught leaves the temp file behind. The
rewritten file keeps the permissions the history had.

## Evidence has one definition

"Observations on dirty or unknown code count in the totals but are never
evidence" is a rule the analysis applies and a sentence the report prints
("all 40 earlier observations were made on a dirty tree and cannot be
evidence"). It was written out five times — `dirty != Some(false)` in the
analyzer and again in `run`, `explain` and `history`, where the counts for
that sentence were rebuilt by hand (#187). Change one copy and the sentence
describes a rule the analysis no longer follows; the mutation run showed
two of the copies had no test at all.

`analyzers::history::is_evidence` is now the only place the rule exists,
and `PriorEvidence::of` the only place the counts are made. `PriorEvidence`
moved out of `report.rs` with it: what earlier runs left behind is a fact
about the history, and the report only words it.

The same change gave four invariants the test each lacked (#186), checked
with `cargo mutants` rather than by reading: a dirty red cannot carry a
streak's address, a pardon needs a report for every run, the runner/report
mismatch note fires in both directions and only then, and a self-closing
root element is a report with no tests. One thing the tests turned up is
not an invariant today and is left as a question (#276): a green seen only
on a dirty tree is enough to call the streak after it a regression.

## Exit 2 is decided in one place, and a temp file is owned by a value

The exit contract (0 green, 1 the suite failed, 2 sooth failed) is a public
interface. Exit 0 and 1 were always decided in one expression. Exit 2 was
returned from fifteen sites in `main.rs`, each printing its own message and
returning the code by hand, and on the preset path each also remembering to
remove the temp report. The only way to see the whole contract was to grep
for it, and a new early return could forget the cleanup (#191).

Now a command returns `Result<ExitCode, SoothError>`. `SoothError` is the
lines that say why — written where the context is, because those messages
are the product ("…is `composer` the test runner itself…"), and printed in
one place, `dispatch`, which also returns the 2. A usage error is still
clap's and a panic still the hook's: three sources, each in one spot.

The cleanup stopped being a duty and became ownership. A preset's report
lives in a private temp directory; `TempReport` holds its path and removes
file and directory when it is dropped. That happens after a normal run, on
every `?`, and while a panic unwinds — which is why the release profile
must keep unwinding (`panic = "abort"` would save 150 kB and bring the leak
back). The capture directory of quiet runs works the same way.

What a `Drop` cannot cover is a signal: the default disposition ends the
process without unwinding. That is the rest of #140 and needs sooth to
handle signals itself.

## A run and its report are one thing, defined once

Three steps always go together: make sure the report that will be read is
this run's (delete sooth's own file; remember the state of the user's),
spawn the command, read what it wrote. They were written out in the run
loop and again in the `--verify` loop, and the order pass of v0.4 needs
them a third time (#245).

`reported::run` is those three steps. It takes the command as it will be
spawned, the report source, and optionally a file for the command's output,
and returns how the runner exited plus the report — or why there is none,
as a value (`Untouched`, `Unreadable`), not as text. The callers decide
what is theirs to decide: how many runs, in what order, with which command;
whether a missing report is fatal (the run loop) or a skipped pass
(`--verify`); and how to say it, because only the caller knows the command
("…is `composer` the test runner itself…").

`runner.rs` keeps its narrow job — spawn once, measure — and this module
sits one step above it: a run that reports.

## The history carries durations

Before v0.4 one thing had to be decided, because it shapes the file every
user accumulates: is the slow-test pass a report on one run, or a reading of
the history (#249)? Bram chose the history.

A list of the slowest tests of this run is a commodity — PHP has
`ergebnis/phpunit-slow-test-detector` for it, and sooth has printed one
since v0.1. What sooth adds everywhere else is evidence over time, and the
same is true here: "slower since `<commit>`" is the timing twin of "failing
since `<commit>`", and "slow in ci, fast locally" the twin of "every failure
in ci". Both need a before, so the before has to be recorded, starting now.

The format, and why:

- **`"ms"`, whole milliseconds.** Finer than any slowness worth a line in a
  report, an integer the existing hand-rolled parser already reads, and
  short: a line grows by about 3%, measured on a Laravel suite.
- **In front of `"id"`.** The parser takes the first occurrence of a key,
  and the id is the one value that can contain anything — including text
  that looks like a key. Every other key is written before it.
- **Absent when unknown, zero when measured as zero.** A report without a
  `time` attribute did not measure; pytest's `0.000` and jest-junit's `0`
  did, and rounded. Those are different facts, so `TestCase::duration` is an
  `Option` and the history omits the key for the first and writes `"ms":0`
  for the second. A baseline of "under a millisecond" is exactly what makes
  "now 1.8 s" a finding.
- **Old lines are observations without a duration**, like lines from before
  environments and invocations were recorded. No migration.

Nothing reads the durations yet, on purpose: the pass that does is v0.4's,
and it gets to be designed against real, accumulated data instead of
against a format invented the same week.

## A signal is a request to stop, and sooth's third dependency

Two things went wrong when sooth was signalled (#34, #140). Interrupting
`--runs 20` at run 14 lost the thirteen runs that had finished: the default
action ends the process on the spot. And a signal that reached only sooth —
a CI cancel, a timeout wrapper, `kill` from another shell — left the test
command running as an orphan and sooth's temp directories behind, because
no destructor runs either.

Both need sooth to take the signal itself, and that is the one thing here
the standard library cannot do without `unsafe`, which the crate forbids.
So sooth gets a third dependency. Bram agreed to it on 2026-11-15, and to
which one: `signal-hook`, with its optional features off. It adds four
crates to a tree of about twenty-five (`signal-hook`, its registry, `errno`,
`libc`), where `ctrlc` would have brought Objective-C bindings on macOS. The
only API used is the one that sets a flag.

What happens on a first SIGINT or SIGTERM:

- **Nothing new is started.** The run loop checks the flag before each run,
  and `--verify` before each re-run.
- **The run in progress does not count.** Whatever it exited with and
  wrote, a run that was cut short is not a run that failed. It gets no line
  and no place in the report or the history, and its report is not read.
  Under quiet repeats what it had written is shown after all: for a run
  that hung, that is the only trace of where.
- **In progress means: the command had not been seen to end.** That is
  asked at the moment the runner's exit is observed. A request that comes
  later, while the run's report is being read, finds a run that finished,
  and that run counts. Asking after the report was read threw away a
  finished run whenever the signal landed during the parse, which for a
  large report is not a short moment.
- **The command gets two seconds, then is killed.** At a terminal the
  Ctrl-C went to the whole foreground group, so the runner is already
  winding down — PHPUnit prints what it has — and sooth is out in a
  fraction of a second. When only sooth was signalled the runner knows
  nothing; after the grace sooth kills it. Killing at once would cut the
  first case's teardown short for no gain.
- **The report covers what finished**, and its last line is not a verdict:
  `result: interrupted after 5 of 20 runs — none of them failed`. The suite
  was neither proven green nor seen through to red. The line counts runs
  that finished and does not name the run it was "during": a request can
  land between two runs, and then no sixth run was ever under way. A run
  counts as failed here by the rule the suite's verdict uses — its exit or
  its report — so a run that exited 0 over a red report is not called
  passed. The verdict line of a suite that ran to its end counts the same
  way; it used to count exits only, and said `1 of 3 runs failed` under a
  report that showed the test failing in two (found in review). The
  finished runs are recorded in the history like any others.
- **A verification that was cut short keeps the suite's verdict.** With
  `--verify` the one run had finished, so `result: ✗ FAILED` is true and
  stays; what is missing is the re-runs. A note above the verdict says the
  failures are unverified, and so does the closing line on stderr. The
  note is on stdout because the verdict can read `✓ PASSED` — every
  failure was on the quarantine list — and a log of stdout alone would
  show that beside an exit of 2.
- **The exit is 2**, for cut-short runs and for a cut-short verification.
  Not 0 and not 1: sooth did not do what it was asked. A fourth code (the
  shell's 130) would have widened a contract whose point is that it has
  three; a pre-push hook blocks on any of them. The note that explains an
  exit of 1 (the runner and its report disagreeing) is not printed then.
- **A signal after the last run and re-run changes nothing.** There is
  nothing left to stop; the report is a moment away and the exit is the
  suite's.
- **A second signal ends sooth the old way.** Asking twice means now.

The runner is now looked at every millisecond instead of waited for, which
is what lets the flag be seen while a command runs; the measured wall time
moves by less than that.

The limit: only the command sooth started is killed. Reaching what that
command started in turn (Composer's child, paratest's workers) takes a
signal to a process group, and with the group moved out of the terminal's
foreground a Ctrl-C would no longer reach the runner at all. At a terminal
the whole group is signalled anyway; for a lone SIGTERM the direct child is
what sooth can honestly promise.

A second limit: a handler replaces a signal that sooth was started with
ignored. A `sooth run … &` in a script without job control used to ignore
a Ctrl-C at the terminal and pass that on to its runner; now both stop.
Leaving an inherited ignore alone needs the old disposition, and asking for
it is `unsafe`.

## Pruning is the user's decision and sooth's job

"The user prunes a file they own" was the plan for the history's growth,
and the audit put numbers on why it was not enough (#246). One observation
is about 240 bytes, so one run of a 5,000-test suite is 1.2 MB; the 64 MiB
that a load reads is 56 such runs, two days of work at thirty runs a day;
and the file itself grows 13 GB a year. The user owned the file and had no
tool for it — a hand-rolled format is not something to `sed` safely.

`sooth history --prune` is that tool. What it keeps is not a setting: each
test's last 50 invocations, which is exactly the window every analysis
reads (`WINDOW_PER_TEST`). Anything older cannot change a finding, so
removing it is safe by construction, and a contract test holds the two
together — the history reads the same before and after.

- **A skip takes no place in the window.** The analysis leaves skips out
  before it counts fifty invocations, so a test that has been skipped for a
  hundred runs still has every run before that inside its window. The
  prune first counted every invocation, removed those runs, and a proven
  flake was gone after pruning (found in review, before release). The
  window is now counted the way the analysis counts it. Skips themselves
  are kept for the test's last fifty invocations of any kind, so they are
  bounded as well. A unit test runs the analysis before and after a prune
  over 150 generated histories — skips, ties in time, invocations that
  come back, dirty trees — and requires the same findings.
- **Explicit, never automatic.** A run that silently rewrote the history
  would break "sooth observes"; a file that only ever grows until its owner
  says otherwise does not.
- **By time, not by position.** An import appended last can be the oldest
  evidence in the file. The window is found the way the analysis orders
  observations: by `at`, then by position.
- **A stream, read twice.** First to find where each test's window begins,
  then to copy what lies inside. Memory is a few dozen invocations per
  test, whatever the file's size: 270 MB of history took four seconds.
- **Lines that are not observations are kept.** A fragment, bytes that are
  no text: not this command's to judge, the same as for `import --replace`.
- **One rename.** Written beside the history and renamed over it. When
  nothing is older than a window the history is not rewritten at all.

A side effect worth knowing: for a suite of up to about five thousand
tests the pruned file fits in one load, so evidence that had slid beyond
the 64 MiB tail — a test that runs rarely — is back in reach. A larger
suite's does not: fifty invocations of ten thousand tests are about 120
MB, and a load still reads the newest 64 MiB of that. The note that says
the history was read only in part names the command, and promises no more
than that it drops what no analysis reads; right after a prune it does
not name the command again.

So "a prune changes no finding" is the claim, and it holds for what one
load reads. Two things a prune can change in what sooth says. Evidence
that comes back in reach is found again. And the note about how much the
history is worth counts every observation in the file, not the window:
ten clean runs followed by fifty on a dirty tree say nothing before a
prune and "all 50 earlier observations were made on a dirty tree" after
it. The second is the true one.

The bound is per test id, not per suite. A test that was renamed or
deleted keeps its last fifty invocations under the old id for good, and
ids that carry data (a parametrised test named after its input) add up.

## The commands live in `src/commands/`, and only `report.rs` prints

`main.rs` held all four commands, and about a quarter of it wrote to
stdout and stderr with bare `println!` while `report.rs` called itself the
presentation layer; `AGENTS.md` carried the exception (#192). `import`'s
logic — one file named twice, the fallback from a report's own timestamp
to the file's — could only be reached by spawning the binary.

Each command is now a module with one entry, `run(args) -> Outcome`:
`commands/run.rs`, `explain.rs`, `import.rs`, `history.rs`. `commands.rs`
holds what they share. `main.rs` parses the command line, dispatches, and
turns the outcome into the exit code, and nothing else.

Nothing outside `report.rs` prints any more, and the lint that enforces
that has no exception left. Where the words are written follows what they
are:

- **A line of the report is written in `report.rs`**, as a named function
  beside the others: `print_empty_history`, `print_imported`,
  `print_verdict`. Those lines are the product; tests pin them byte for
  byte, and they belong where the rest of the report is worded.
- **A note about what sooth did, or could not do, is worded where it is
  known** and handed to `report::warn`, `report::note` or — for the lines
  that end in exit 2 — `report::error`. "Could not read a PHPUnit version
  from `vendor/bin/phpunit --version`" is the gate's knowledge. This is
  the rule `SoothError` already followed: the lines say why, in the words
  of the place that knows.

Two functions took more than five parameters and carried the only
exceptions to that rule. What they passed along piecemeal is one thing —
what a run ends with — and is now a type, `report::RunReport`.

The move changes no behaviour, and that was checked rather than assumed:
the binary from before and the one from after were run through the same
45 steps in copies of one repository — every command, its error paths, the
gate, a pardon, `--verify`, both JSON forms, and the history file and
import ledger they leave — and stdout, stderr and exit code were equal,
measured durations aside.

## `report.rs` is three files, one per layer

`report.rs` had grown to 2,100 lines holding four things: the color style,
the view of a run that both renderers read, the printed report, and the
JSON (#190). Human and machine output were already apart at function
level — no function both printed and serialised — so the split is by file:

- `report.rs`: `Style`, `JunitSummary`, `Analyses`, `RunReport` and the
  other values a command hands over, plus the plain lines outside the
  report (`warn`, `note`, `error`, `print_json`).
- `report/human.rs`: every line of the printed report.
- `report/json.rs`: the documents. It returns `String`s and prints nothing.

The last point is enforced. The exception to "no module prints" used to
cover all of `report.rs`; it now sits on `human.rs` and on the four plain
functions, so a `println!` in the serialiser fails the build like one in
`history.rs` would (tried: it does).

One rule had two homes. The category a failure is counted in — known
flake, regression, never passed, quarantined, new — was decided in
`explain::Counts::of` and decided again in the JSON, where "quarantined or
new" was re-derived from the flag. It is now `Explanation::category()`,
which the counts and the document's `verdict` both ask. The names the JSON
gives the categories stay in `json.rs`: they are the document's
vocabulary, not the analyzer's.

The printed report asks it as well, where it tells a quarantined failure
from a new one.

The JSON stays hand-rolled and its bytes are unchanged; the tests that pin
them moved with the functions they test. Checked the same way as the
commands' move: the binary from before both refactors and the one after,
through the same 45 steps, with equal output.

## The order pass: its own command, a seed that is said, and what it takes to be called order-dependent

v0.4's order-dependence detection is `sooth order -- <command>` (#36, #13).

**Its own command.** Bram's decision on 2026-11-15, against a `--shuffle`
flag on `run`. The passes are separate by doctrine (see "Flaky and
order-dependence are strictly separated"), and a command makes that visible
where a flag would have had to be refused next to `--runs`, `--verify`,
`--changed` and `--fail-on-flaky`, one by one.

**The runner shuffles, sooth asks.** Sooth does not order tests; a preset
knows how its runner is asked for a random order under a seed
(`--order-by=random --random-order-seed=N` for PHPUnit, which Pest and
`artisan test` hand through; `--randomize --seed=N` for Jest). Checked on
real runners before building: the same seed gives the same order again.
That reproducibility is the feature — "it failed under an order you can
never get back" is half an answer — so the seed is always printed, and
`--seed N` replays it. A fresh seed has at most six digits, because it is
typed back.

**What is refused.** A runner sooth cannot ask (pytest has no random order
without a plugin; `go test -shuffle` sits behind gotestsum's `--`).
`--parallel`: an order is what one process runs. A command that already sets
an order. Only the command line is seen, so after the fact the pass checks
whether its shuffle changed the order at all — an order set in
`phpunit.xml` outranks the flag — and when it did not, that is exit 2 and
not a green pass that compared an order with itself. A suite of three tests
keeps its order under one seed in six, so a seed nobody asked for is
followed by up to four more before giving up.

**What it takes to be called order-dependent.** Two runs that differ are
what a flaky test produces too. The first design ran the shuffled order a
second time. Tried on the Laravel sandbox, which has a test that fails half
the time, it called that test order-dependent at once: it had failed in the
first run and passed twice shuffled. Replaying only the shuffled order makes
every flake that happens to fail in run one a "test that fails in your
order". So both orders run again, and a test is order-dependent only when
it did one thing twice in the given order and the other thing twice in the
shuffled one. Anything else that differed is listed as a difference that
did not hold.

That is still four observations. A test that fails with probability p
produces the pattern by chance with probability 2p²(1−p)²: 12.5% for a coin
flip, 1.6% at 10%, 0.02% at 1%. So the report says what was seen — "twice
each" — the README says it is a sample, and three things stand between a
flake and the label: the second runs, the history (a test it already proved
flaky is never called order-dependent, however its runs went), and the
seed, which lets anyone run the order a third time. The runs alone, below,
are the fourth.

**Two runs when the orders agree, four when they do not.** The second runs
are made only when a test differed, so a suite without order-dependence
pays for two runs.

**Every heading says what was seen.** A review of the first version found
the report claiming more than its runs: a red pass without a dependency
ended "a run was red, and not because of the order", and every difference
that was not confirmed stood under "that is not an order's doing". Neither
was witnessed — two orders are a sample, and a test can be flaky and
order-dependent at once. The verdict now reads "no test followed the order
in these runs", and the differences are filed by what happened to them:
did the same thing twice in each order (order-dependent); did something
else the second time (differed, and not the same way again); had no result
in a second run (unconfirmed, neither proven nor refuted); held in the
suite and not alone; proved flaky by the history before. One second run
that did something else than its order's first is enough for "not the same
way again", also when the other second run had no result for the test: the
missing run could only have confirmed half of a pattern the first one
already broke.

**What counts as shuffled.** The pass checks that its shuffle changed the
order, over the tests it compared and nothing else: a skipped test that
moved is no change. For Jest, which shuffles inside each file and runs the
files apart in whatever order they finish, only a change inside a file
counts; files swapping places is not an order any test was put in. Fewer
than two compared tests have no order at all, whatever the reports list.

"Inside a file" is read from the report's `file` attribute, which sooth
asks jest-junit for (`JEST_JUNIT_ADD_FILE_ATTRIBUTE=true`). The first
version grouped by classname, and Jest's classname is the describe block:
a file of `describe`s with one test each had no group of two, so a shuffle
that did reorder it was refused as one that changed nothing (exit 2). A
second review found that on a real Jest project. A report without the
attribute falls back to the classname.

A shuffle needs two tests a report tells apart within its reach: the
suite, or for Jest one file. Where no file has them — every file holds one
test, or nothing but the rows of a `test.each` under one title, which read
the same however they were shuffled — no seed can show a change. That is
refused at once, without trying other seeds, and for what it is: "no order
to compare", with the advice to give such rows names of their own. It used
to be told as an order that had stayed, with a pointer to the runner's
configuration. Sooth does not take a difference between the runs as proof
that the rows moved: a flaky row makes one too.

Where a file does have two such tests and they stayed in place, the order
was most likely pinned, and the refusal asks about the configuration or a
script. It says what the report lists, not what ran — rows under one name
may still have moved unseen beside them — and for Jest it says so about the
inside of each file: files that finish in another order put every test
somewhere else in the report.

**The history gets the first run and no other.** The run in the suite's
own order is an ordinary run and is recorded as one. A shuffled run is not:
a test that fails only in another order would be a pass and a failure on
one commit there, which is the history's definition of a flake.

**No new exit code.** A test that fails in another order fails a run, and
"at least one run failed" is exit 1 already. 0 when both orders are green.
2 when the pass could not be made, an interruption included: it compares
whole runs, and half a run compares to nothing.

## Alone, a test says which way its dependency runs

A shuffle shows that a test depends on the order. It does not say which
way, and that decides the fix (#111, found by hand while dogfooding on a
production suite: fifteen tests that needed a `Partner` record some later
test created). So every test the order pass finds runs by itself, twice:

| In the suite | Alone | Meaning | Fix |
|---|---|---|---|
| failed in one order | fails the same way | it needs something a test before it leaves behind | a setup of its own |
| failed in one order | passes | something a test before it leaves behind breaks it | in the test that leaks |

- **Only what the pass found.** Isolating a whole suite boots the
  application once per test. The set a shuffle flagged is small, and at
  most twenty of them are run alone: a suite where everything depends on
  one bootstrap test would otherwise be started hundreds of times. The
  rest keep their place in the report without a direction, and a note
  says so.
- **Twice, like `--verify`**, and for the same reason: once cannot tell a
  result that holds from one that happened.
- **A test that passes alone once and fails alone once is taken back out.**
  Nothing ran before it either time and its result changed anyway, so no
  order explains it, whatever its four runs in the suite looked like. This
  is the cheapest check against the coin-flip test of the section above:
  two more runs of one test, where the suite runs cost two more runs of
  everything. For a test that fails half the time it halves what is left
  of the 12.5%. Measured on the Laravel sandbox, which has such a test: 60
  passes under a seed that exposes no dependency called it order-dependent
  twice, and a difference that did not hold 26 times.
- **The kind of failure counts, not just pass or fail** (#103). A test
  that errors with a `TypeError` in the suite and fails an assertion alone
  did not reproduce its failure; a misconfigured selection that trips the
  bootstrap would otherwise count as confirmation. That reads "alone it
  fails in another way than in the suite" and gives no direction. One
  reproduction in the two runs is a reproduction, as for `--verify`.
- **Alone is checked, not assumed.** The first version reused the
  selection of `--verify`, a pattern on the test's name with no class and
  no end: asked for `user`, PHPUnit also ran `user_is_created`. That is
  harmless for a re-run and the opposite of alone. An independent review
  built the case where it matters — `user` needs `user_is_created` before
  it, both are selected, `user` passes in that company — and sooth said
  "alone it passes: something before it breaks it". The direction was
  reversed, and the advice with it. Two things changed. The selection is
  now the class and the whole name (`/Tests[\\.]Unit[\\.]UserTest::…(user)$/i`;
  checked on real PHPUnit and Pest: one test, data-set rows and prettified
  names included). And whatever the runner did with it, a report in which
  any other test ran is not a run alone: the line says "not seen alone"
  and gives no direction, and a flip there is not held against the test.
  Jest's `-t` sees no file, so the same title in another file still comes
  along; the check is what keeps that honest.

  The check counts cases, not names. Its first form asked whether another
  *id* had run, and a second review found what that misses: the rows of a
  Jest `test.each` with a fixed title are several cases under one id, each
  the other's predecessor, and they passed for a test alone. Every case
  that ran counts now, so two under one name are company. The class in the
  PHPUnit pattern also starts where the string starts, behind nothing or
  behind the `P\` Pest puts in front (`^(?:P\\)?Tests[\\.]Unit[\\.]UserTest::…`),
  so `UserTest` selects neither `AdminUserTest` nor a class of that name in
  a longer namespace. Tried on real PHPUnit and Pest against every test of
  two projects built to break it (39 and 29 shapes of class and name): one
  test each time.
- **A run alone that says nothing leaves the test where it was**:
  order-dependent, without a direction. So does an interruption, which
  still exits 2 after the report: the four runs were made, and what they
  found is not thrown away for the part that was not.
- **Not which test is responsible.** The direction is a fact about one
  test. The predecessor that leaks, or the one that sets up, takes a
  search through orders — `ROADMAP.md` keeps that out.

## Behind a PHP binary, the runner is the script

`sooth run --preset phpunit -- php vendor/bin/phpunit` could not run:
sooth put `--log-junit` right behind the program, and `php` reads what
stands in front of its script as its own options (#294, found by the
review of the order pass and present since the preset existed). Without
`--preset` the command was not recognised, and sooth read the exit code
only. `php -d memory_limit=1G vendor/bin/phpunit` is how a suite that
needs more memory is started, so this is an everyday command.

- **The runner has an index.** `Shape::Runner` carries where the runner
  stands: the program, or under the phpunit preset the script a PHP binary
  runs. Injection, the runner's arguments (the order pass's refusals, the
  gate's "already selects" check), the paratest refusals and the hint for
  a missing report all read that index. Before, the gate took
  `vendor/bin/phpunit` in `php vendor/bin/phpunit` for a selection of the
  command's own, because it exists.
- **The script is found behind the options that only set PHP up**, and
  behind no other: no `php.ini` (`-n`), `-q`, `-C`, `-e`, `-H`, and the
  ones with a value — `-d`, `-c` and, up to PHP 8.4, `-z` — in every
  spelling PHP reads: letters in one word (`-nd memory_limit=1G`), the
  value attached or in the next word, the long forms with and without
  `=`. One function (`php_script`) for `artisan`, `composer.phar` and the
  runners, so `php -d memory_limit=1G artisan test` works the same way.
- **Any other option, and the command is left as typed.** The first
  version skipped every option it did not know, and an independent review
  found what that breaks. Behind `php -f <script>` PHP goes on reading
  options of its own, so `--log-junit` there is PHP's again (PHP 8.5:
  `Error in argument 3`); `-nf` is the same thing in one word; `php -l
  vendor/bin/phpunit` lints the runner; behind `php --` the script comes
  from stdin. A list of what to refuse would always miss one. So the list
  is of what is known to be harmless, and anything else means sooth does
  not read the command: it runs as it did before this change — exit code
  only, or with `--preset phpunit` the flags behind `php`. Placing the
  flags right behind `-f` would mean adding a `--` to the user's command
  and knowing which words behind the script are still PHP's; nobody has
  asked for that shape. One message is lost with it: `--preset phpunit --
  php -f vendor/bin/paratest` was refused as paratest before anything ran,
  and now stops on PHP's own error. Exit 2 either way, and no report in
  either.
- **Named, not guessed.** Without `--preset`, a script is taken for the
  runner only by its name: `phpunit`, `pest`, `paratest`. `php
  bin/run-tests.php` can be anything. With `--preset phpunit` the user has
  said what the script is, and the flags go behind it whatever it is
  called (`php tools/phpunit.phar`).
- **The version is asked the same way.** The gate asks PHPUnit for its
  version before it hands it several files. A script that is run through
  `php` need not be executable, so the probe is the command up to the
  runner, `php` and its options included. Under `--preset phpunit` that
  runs whatever script the command names once more, with `--version`: the
  same trust the gate already put in a runner that is the program.
- **Only for PHP.** `python -m pytest` and `npx jest` are wrappers of
  another kind — a module flag and a package runner, each with its own
  rules for what follows. They are left as they were: the README asks for
  the runner itself there.

## Slower since a commit: what a clock is allowed to say

The slow half of v0.4 reads the durations the history carries since v0.3.4
(#296, #13). Its first finding is the timing twin of "failing since": a
test that takes longer since a commit than before it.

**Measured first.** "The history carries durations" asked for this pass to
be designed against real data. No project history with durations exists
yet, so the data was made: a PHPUnit suite of 233 tests doing real work
(SQLite, files, bcrypt, waits), run 69 times through sooth over four
commits on one machine — idle, with every core busy, with Xdebug tracing
on, locally and as CI — and again on efficiency cores. What it showed:

- 190 of the 233 tests are timed at 0 ms, and the slowest ten carry 86% of
  the suite. Most tests have no duration to speak of.
- One run of a 2 ms test was timed at 11 ms. Tests of 20 ms and more stay
  within about a fifth of their median on an idle machine.
- A machine does not slow every test alike. With every core busy the
  tests that compute took up to 1.6 times as long and the ones that wait
  not at all; under Xdebug tracing a test that hashes in PHP took 73
  times as long and one that calls bcrypt 1.02 times; on efficiency cores
  a 20 ms sleep took 120 ms. No factor takes a machine out of a
  measurement.

**Eight versions, seven reviews.** An independent reviewer went over each
version on the real suite and on histories written to break it.

- The first compared a test's medians on both sides of any change of
  commit. Three runs of one test with `--filter` after a commit that
  changed nothing named that test — alone, it paid for a fixture another
  test builds first — and three runs on efficiency cores named seventeen.
- The second divided a test's time by how much longer the rest of the
  suite took, as a ratio of sums. One heavy test that got slower passed
  for the machine and hid every other finding.
- The third read the machine from the median of the other tests' ratios:
  at 1.5 or more it named "the suite as a whole", under that it divided.
  On a busy CI runner the tests split into two groups, ten at 1.0 and
  eleven at 1.6 to 2.3, and whether one test was in the set decided
  between "the suite" and a test named for the load: 5% of the subsets
  of those runs named one. And the machine was asked after a test's
  commit had been chosen, so a slower machine at a later commit erased an
  earlier finding without a word.
- The fourth divided nothing: a test was named only when a quarter of the
  other tests kept their time over the runs since its commit. Two more.
  Those runs began at the test's own first pass, so a test that failed
  the first runs on a slower machine and passed later was read against
  runs of its own commit, already slow, and named. And the reading was
  made again for every test at every commit: 3 to 8 seconds on a history
  of 5,000 tests.
- The fifth read each commit once, from its first run by any test, over
  its own runs. One more, narrower: a machine that changed partway
  through a commit's runs, under a test that was not timed before the
  change, was averaged away by the runs before it, and the test named.
- The sixth read a commit from its first run and again from the test's
  own, and withheld when either moved. One minor: a machine slower only
  in the commit's first runs, before the test's own, withheld a test
  whose own runs were all on a steady clock.
- The seventh read once: from the test's first run on the commit to the
  next commit's, against the runs before the commit. One more, the mirror
  of the fifth's: a machine that came back after the test's own runs, in
  runs of the commit the test was not timed in, averaged away the runs it
  was in.
- The eighth, below, reads over the invocations the test was timed in,
  and over no others.

**The rule for a test, and where each part comes from.**

- *Runs of the whole suite.* An invocation is a measurement of a test only
  when it ran at least nine tenths of the tests of the largest invocation
  that timed that test. `--filter`, a single file and the gate's selection
  are runs too, and say nothing here.
- *Passes on a clean, known commit, in one environment.* A failure is not
  the test doing its work, a dirty tree is not the commit, and a CI runner
  and a laptop are different clocks.
- *Three invocations on each side* (`RUNS_EACH_SIDE`), the number a red
  streak needs too. An invocation is one measurement, the median of its
  runs: twenty runs in a row share one machine in one state.
- *Twice as long* (`SLOWER_FACTOR`) *and 20 ms more* (`SLOWER_BY_MS`): the
  median of every invocation since against the median of the last five
  before (`RUNS_BEFORE`). Twice is above the 1.6 a busy machine did to a
  test; 20 ms about twice what a single run was off by. A test that goes
  from 2 ms to 15 ms was built into the data and is not named.
- *No invocation since as fast as one of the five before.* Medians alone
  would name a test that has got fast again while its slow runs still
  outnumber the fast ones.
- *A commit seen for the first time, and no earlier commit among the runs
  since.* Code that ran on both sides did not change the clock.
- *The clock held* (`Readings::moved`). The other tests are read over the
  invocations the test itself was timed in on the commit — and when those
  are fewer than three, its first three from there on — against their
  runs before the commit's first run on this clock, by any test. The
  before keeps a test that only passed late from being read against runs
  of its own commit on a machine already slower; the during keeps a
  machine that changed partway through the commit from being averaged
  away by runs the test was not in, whether they came before its runs or
  after them, and keeps a machine that was slower only while the test sat
  out from being held against it. Over those, the other
  tests that took 20 ms or more *before* it — which a slower machine
  cannot fail to slow, and which did not merely enter the set by getting
  slower — timed in three invocations on each side, are laid out by how
  many times as long they take. The one a quarter from the top is read:
  at 1.25 or more (`CLOCK_MOVED_PER_MILLE`) the clock moved, and the test
  is withheld. From five such tests on (`TESTS_TO_READ_A_CLOCK`); under
  that, nothing is read. A reading is worked out once per set of
  invocations and sorted, and a test only steps past its own place in it.
  Most tests of a commit are timed in the same invocations and share one
  reading: 0.4 to 0.6 seconds on histories of 5,000 tests. Its cost grows
  with the number of different sets: 5.5 seconds when all 5,000 got
  slower at once and each failed in different runs since — a suite that
  flaky on a machine that slow. Its readings are kept for the whole pass,
  so that case also holds about half a gigabyte.

  Its own runs and not every run since: what changes at a later commit, a
  machine or other tests' code, is that commit's reading, and takes back
  no test named before it. The test's own time since stays every run
  since, the later ones included.

A quarter and not the typical test, because a busy machine slows what
computes and spares what waits, and half a suite may wait. The quartile,
measured: 1.04 between two stretches of idle runs, 1.01 across the commit
that slowed one test by twenty times, 1.13 with six cores busy — and 1.30
under Xdebug tracing, 1.41 with every core busy, 1.74 as CI with every
core busy, 2.6 and 2.8 on efficiency cores. The mark sits in the gap.

The rule, the clock included, is asked of every commit a test could be
named with. When it holds for more than one, the test got slower in steps,
and the commit of its largest step is named — the earliest when two are as
large. A commit's step is what its own runs, those up to the next commit,
took more than the five in front of it: a test that went 4, 15, 284 ms is
named for the last commit however few runs the middle one had. When the
rule holds for none only because the clock moved, the test is withheld,
for the commit of its largest step.

**Withheld is said.** Per commit and clock, one line: how many tests got
slower by their own clock, that they are not named, and what the other
tests did — as a lower bound that holds for each of them: the least
factor any of them was read at, cut and not rounded, and the fewest
tests any was read from. A withheld test is never silent and never a
name.

**Every commit says how many.** A heading reads "1 test of the 19 that
take 20ms or more": the tests a clock could have named since that commit.
Three or more that are over a fifth of those (`MANY_AT_ONCE`) are said to
be many at once, which may be something they share, and three are shown.
That is the only answer there is to a setting that slows a minority of
the tests: from an idle before, Xdebug's quartile is 1.30 and its runs are
withheld; from a before that was busy itself, it was 1.2 and 29 tests were
named under that heading.

**Replayed and resampled.** One invocation at a time over the 69: nothing
in the first 42, through the idle runs, the busy ones, a commit that
slowed two tests by 11 and 13 ms, and one that made a third slow in CI
only, where it had no before. The slowdown built in at the last commit (14
ms to 284 ms) is named with its third local run, one test of nineteen,
and stays. As CI, where every run on that commit had every core busy, it
is withheld with the tests the load slowed: four at its third run, one
from its fourth on. Resampled:

- every pseudo-commit inside every stretch of the 69 (39) and 1,500
  random permutations of the 48 local runs without the slowed test: no
  finding;
- three sets of 400 random subsets of the 69: the slowed test named
  locally, for its commit, in 398, 400 and 400; in the other two it is
  withheld, the Xdebug runs being most of the runs of its commit that
  were kept, and never named for another commit;
- all 1,764 combinations of 3 to 6 CI runs on each side of the loaded
  stretch: no test named, with the slowed test or without; one to six
  withheld;
- efficiency cores, on two histories: seventeen withheld, none named.

**What it cannot know**, and the README says: the cause; a test that got
slower on a clock that moved at its commit — withheld; a clock that moved
after it, which is in the test's time since; a test with fewer than
three runs on its commit, read over its first runs on the next commit
too, and withheld while the clock moved there; something that slows a
minority of the tests; a suite that got slower as a whole, which is not a
finding here; the shape of the branches, of which the order of the runs
is all sooth has; a suite that changes size by more than a tenth, or CI
shards cut differently; suites with fewer than five other tests of 20 ms
that passed in the runs the test was timed in; what
has left the window. "Nothing stands out" therefore speaks of pointers,
not tests.

**A finding, not a failure.** No exit code changes, and nothing is red. A
gate on slowness would need a budget somebody chose; nobody has.

**In the history's analysis, not a command of its own.** The order pass got
a command because it needs runs of its own. This one needs none: it is one
more reading of the file every run already writes, and it appears where
the other readings do — after a run, in `sooth history`, after an import.
It reads every test's window at once, because of the clock.
`Observation::is_evidence` moved to the type it is about, so the two
analyzers that use it read it from the domain and not from each other.

**To be checked against a real history.** The thresholds are constants
with their measurements beside them. They come from one machine and one
made-up suite; a project history with a few weeks of durations is the
test they have not had.

## Slow in one place: two machines, read from the same runs

The second half of the slow pass (#299): a test that takes longer in one
environment than in another — the timing twin of a flake whose every
failure was in CI, and the "slow in ci, fast locally" `ROADMAP.md` names.

**Two environments are two machines.** "Slower since" could assume one
clock and withhold when it moved. Here a different clock is the normal
case: a CI runner is commonly slower or faster than a laptop for
everything, and not by one factor — measured on the suite of step one, a
CI runner with every core busy made the tests that compute 1.5 to 2.2
times as slow and left the tests that wait alone. So no environment
comparison means anything without a reading of the two machines, and
there is no withholding to fall back on: the reading is part of the rule.

**The rule.**

- *The same code on both sides.* Only commits both environments ran the
  test on, clean, passes only. A code change between commits is then on
  both sides, and the overlap rule below keeps it from reading as a
  place: each side has a run of each commit.
- *The same suite on both sides* (`the_same_suite`): on each commit both
  places ran, the tests both ran there, and the runs that ran nine tenths
  of those. Judged per place, as "slower since" does, a CI that always
  shards has a shard for its whole suite, and a test that pays for a
  fixture in its own job, which a laptop running everything pays once
  elsewhere, read as slow in CI (found in review, with a fixture of 300
  ms: `0ms → 300ms`). Judged by size against the larger place — the first
  fix — a CI that also runs a group the laptop leaves out, a tenth more
  tests or over, made every laptop run a part, and nothing was ever
  compared (found in the second review). What both places ran tells a
  shard, which ran half of it, from extra tests, which leave all of it
  run.
- *Three invocations in each place* (`RUNS_EACH_SIDE`).
- *What the machine does* (`machines`, `quarter_from_top`): the other
  tests that take 20 ms or more in the faster place — which a slower
  machine cannot fail to slow — timed in three of the same invocations on
  each side, by how many times as long they take in the slower place;
  the ratio a quarter from the top, as for the clock of step one. From
  five such tests on; under that, nothing is named. A quarter, because
  a slower machine slows what computes and spares what waits.
- *Twice as long as the reading accounts for, and 20 ms more than it
  accounts for* (`slower_than_the_machine`): the median there at least twice
  the median here times the reading, and 20 ms over the median here times
  the reading. A reading under 1 — a runner faster than the laptop —
  counts as 1: it makes no test slow on it.
- *No run there as fast as one here.*

**Measured.** The 69 invocations of step one hold two commits both
environments ran. On `35a8b25`, six runs in each place, both machines
idle, every test of 20 ms or more locally is within 0.98 and 1.09 of the
other place; one just under, at 19.5 ms, took 1.67 times as long (13 ms
more); and the test built to wait 300 ms in CI only takes 0 ms locally
and 302 ms there. On `1019f96`, over all its runs — 23 locally (idle,
under load, with Xdebug), six as CI with every core busy — the reading
is 1.65, and a test that hashes takes 1.94 times as long (26 ms to 50
ms), which the reading accounts for.
Replayed one invocation at a time, the waiting test is named from the
first point at which three runs stand in each place, on the commit that
ran both, and nothing else is named, to the end. Shuffling the
environment of every run on those two commits, 300 times, with that test
and without it: nothing named.

**The heading carries the reading.** "Where a quarter of the other tests
took 1.2 times as long or more": the least reading of the tests under the
heading, cut and not rounded, which holds for each of them. A reader can
see from it whether the place is a slower machine, and by how much.

**What a machine does to some tests only is the test's.** Review put the
measured suite's three runs on efficiency cores against its five idle
ones, as CI against local: a test that sleeps 2 ms took 66 ms there, and
is named, under a reading of 2.6. The machine stretched a short wait;
the tests the reading comes from, of 20 ms or more, do not show that.
It stays named: it is true that the test is slow in that place, nothing
else is, and what it does there is what to look at. The README says the reading is what the machine does
to most tests, not to each, and lists a coarse timer beside a debugger
and coverage; three or more over a fifth and the heading says many at
once, fewer and it does not. Reading the short tests too would not have
taken it back: on those runs the other tests under 20 ms gained 14 ms at
most, but for a fixture test at 19 ms that gained 46 (and is not named,
under the reading), while this one gained 64.

**What it cannot know**, and the README says: what a machine does to some
tests only (above); a run that leaves out a tenth of the suite or less,
and a parallel run in one place against a serial one in the other, where
the first test of each process pays for what the suite sets up once; a
suite that changed size by more than a tenth, as for "slower since";
commits CI ran and nobody ran locally, such as GitHub's merge commit of a
pull request; two machines with fewer than five other tests of 20 ms to
read them from; lines from before sooth recorded environments, which are
in no place. And like everything the slow pass says, it reads the clock
and not the cause.

**Cost.** Which runs were of the suite both places run is worked out
once per pair of places. A reading is made per pair of sets of
invocations a slow test was timed in, as in step one, but the candidates
are grouped by their set first, and each reading is let go when its
tests are done: one reading held at a time, not one per set (review
found 520 MB in the case below while every reading was kept). A history
of 5,000 tests and 50 runs in two places takes 0.4 seconds, in four
places 0.6; 6 seconds and 140 MB, as much memory as without the pass,
when CI is twice as slow for every test and every test failed in six
different runs — every test a candidate, each with a reading of its own.

**To be checked against a real history** with CI imports, as step one is.

## A Composer script that hands the report flag to more than one command

Found dogfooding v0.4.0 on a production Laravel suite (#303), whose
`composer test` runs several scripts in turn, each with its own runner.
Composer hands the arguments after `--` to every command of a script and,
through a reference (`@test-unit`), to every command of the script it
names. Under the phpunit preset sooth's `--log-junit` is one of those
arguments: every runner wrote the one report over the one before, and
sooth took the last one's tests for the whole run. A green run said
`1 total` where three tests ran, and the history never saw the others —
the kind of thing sooth exists not to say.

**Refused, not repaired.** Composer gives every command the same
arguments, so there is no flag that gives each runner a report of its
own; running the script's commands one by one would mean doing Composer's
work (`@php`, `@putenv`, callbacks, `@additional_args`, the arguments a
reference adds) and getting it subtly wrong. sooth reads the script before
it runs anything and refuses when more than one command would get the
arguments, naming them, and a script to run instead when one of them runs
a single runner (`phpunit`, `pest`, `paratest`, `artisan test`) and
nothing else that gets the arguments. It is the same rule that already
caught Laravel 12's skeleton, whose `config:clear` line lacks
`@no_additional_args`: that one used to fail after the run, with no
report; now it is named before.

**Composer's rules, read from its source** (`EventDispatcher` and the
console `Application` in Composer 2.10) and compared, over two review
rounds, with what real Composer runs:

- A command with `@no_additional_args` gets nothing, and a reference
  marked so hands nothing on.
- `@<name>` calls a script with them — except a reference that writes
  `@additional_args`, which Composer hands nothing (it keeps what
  `array_splice` removed, which is nothing); `@composer …` too.
- `@composer …`, or a command that starts `composer`, `composer.phar` or
  `php composer.phar`, starts Composer again; behind a `--` of its own the
  arguments reach the script it names in the same manifest, and sooth
  follows it there. Without one, the new Composer reads them as its own
  options and stops: one command.
- `@putenv` and a PHP callback get no command line. A command class does:
  Composer runs it with them as its input.
- Anything else, `@php` included, gets them — at the end, or wherever it
  writes `@additional_args`, once for each time it writes it.
- A reference to a script that does not exist runs nothing; a script that
  calls itself is stopped.
- Which script a command line runs: Composer's own commands and their
  aliases first; a script named like one of them, or like a script event
  (in any case, `-` or `_`), is no command. An alias from
  `scripts-aliases` takes its name from whatever had it — a script's own
  name, an alias of Composer's (`u`), and for an abbreviation even one of
  Composer's own names (`inst` for an alias `install`); typed in full,
  Composer's own name runs its own command, since Composer then registers
  no scripts. An alias is a command only, not a name `run-script` takes,
  and `run-script` refuses an event spelled otherwise (`post_install_cmd`). A name that is none of these is looked up as
  Symfony Console does: a unique abbreviation per `:`-separated part,
  ignoring case when nothing matches as typed, an alias counting apart
  only when its script does not match too. The first version gave an
  alias the lowest place, and its own test said so; the second review
  round ran real Composer next to it and found the opposite.

Every command that gets the arguments counts, not only those that look
like runners: a wrapper script runs tests under any name, while a command
that runs none and is refused has a one-word fix that Composer's own
documentation describes. Not modelled: a command class reached from a
script typed as a command gets the arguments even when marked
`@no_additional_args` (Composer hands it the raw input then); one command
line that chains runners itself (`phpunit a && phpunit b`), where only the
last runner gets the flag — the README says to give each its own script;
an alias two scripts claim, which Composer gives to the later in the file,
an order `serde_json` does not keep, so sooth assumes neither; plugin
commands; and `--no-scripts`.

**Only what is certain is refused.** A `composer.json` that cannot be read
or parsed, a script it does not have, a command line that runs no script:
no refusal, and the run goes ahead as before. The manifest is the one in
the working directory or the one `-d`/`--working-dir` names, under the
name `COMPOSER` gives (trimmed, and unset when empty, as Composer reads
it). One that Composer finds in a parent directory — with `use-parent-dir`,
or when a person at a terminal says yes — is not read.

**`serde_json`, a fourth dependency.** Earlier entries keep sooth's JSON
hand-rolled: the `--json` output and the history are shapes sooth writes
itself, fixed and flat. `composer.json` is someone else's free-form JSON —
nesting, escapes, `\u` sequences — and parsing that by hand is where bugs
hide. Bram chose `serde_json` over a reader of our own (2026-11-21). It
reads into `serde_json::Value`, with no derive.

## An unchanged dirty tree is code sooth can name (#119)

"Flaky evidence requires one clean commit" made the history passive in
name only. People test before they commit: on a branch every local run is
dirty, and the dogfood on a production Laravel suite said it plainly — the
tree is never clean, because the next piece of work is open before the
last one is committed. Every one of those runs counted in the totals and
proved nothing, and the only way to get evidence was to make runs nobody
would make otherwise, which is what the history promised not to ask.

**The rule.** Evidence is one *code identity* with a pass and a failure,
from two invocations. A clean checkout is identified by its commit, as
before. A dirty tree is identified by its content: the tree git would
write for `HEAD` with every change and every untracked file that is not
ignored added — `.sooth/` and the run's own report left out, the same two
the dirty check leaves out. Two runs with the same content ran the same
code, committed or not; the moment anything is edited the hash changes and
the chain breaks by itself. No content read — older lines, no git 2.31,
any step failing — is no identity, as before.

**Read without a trace.** The content is written through a copy of the
index (`GIT_INDEX_FILE`) into an object store of sooth's own
(`GIT_OBJECT_DIRECTORY`, with the repository's as an alternate for what it
already has), with the repository's hooks out of reach (`core.hooksPath`
pointed at an empty directory: `post-index-change` fired five times a run
before review caught it), then `git write-tree`. The scratch directory is
private (0700, as for the run's report: it holds a copy of uncommitted
work) and removed on every path; sooth listens for an interrupt before
the read starts, so a Ctrl-C during it removes it too. The repository's
index, objects and status stay exactly as they were — a test checks all
three, another that no hook runs. Copying the index keeps git's record of
unchanged files, so only what changed is read. Paths come from `--git-path`
joined onto the working directory, which every git since 2.5 answers;
`--path-format=absolute` (2.31) is echoed back by an older one, which
review found made it re-read every file of the repository. Keeping hooks
out takes `core.hooksPath`, git 2.9: on an older git a hook can still run
while the content is read.

**One listing, not four.** Whether the tree is dirty and what reading its
content would cost come from one `status -z --untracked-files=all`. Review
measured six full passes over a 50,000-file repository per run when every
file's stat information was stale — a container writing to a host's bind
mount — against `main`'s two; one listing per read makes it four.

**The copy keeps the index's time.** Git trusts an entry as unchanged
when its size and times match and it is older than the index; one written
in the index's own instant it reads again ("racily clean"). A copy of the
index made now is newer than every entry, so a file changed in the index's
last second, at the same size, passed as unchanged — CI's Linux git, which
keeps whole seconds, found it in a test before any user did. The copy is
given the index's modification time.

**What it leaves out, it leaves out by name.** `.sooth/` and the run's
report are excluded by a literal pathspec — a report called `out[1].xml`
is not the glob that takes `out1.xml` along — and only when git does not
already ignore them, since `git add` refuses a pathspec that names an
ignored path.

**Where it says nothing.** Three places where the hash would not name the
code, found by review, give no content and so no proof:

- *A repository within this one* — a submodule, or a package developed in
  place with its own `.git`. The tree records it as its `HEAD`; work
  uncommitted inside it would not change the hash, and a pass before an
  edit there and a failure after it read as a flake. Every gitlink in the
  written index — listed from the top, whatever directory sooth runs in;
  the second review round found a run from a subdirectory saw only its
  own — is asked for its own status, with git's repository variables left
  out; any change, no content. One never checked out has no `.git` and no
  work in it, and is passed over: asked on its own, git would walk up and
  answer for the repository around it. Files put by hand into such an
  empty directory are seen by neither git nor sooth — on a clean commit
  either.
- *More than 5,000 changed and untracked files, or 64 MB in them.* Each is
  hashed, compressed and written, twice a run: an unignored 1 GB dump cost
  37 seconds a run, an unignored coverage directory of 20,000 files 2.5.
  Past the budget a run proves nothing rather than taking long.
- *A directory the repository ignores.* Run from there, the code under
  test is in no tree and no commit, yet the run was recorded on the
  repository around it — clean ones included, a flaw older than this
  change. Such a run now has no identity at all.

**Read twice.** A suite of minutes is long enough to keep working while it
runs. The content is read again after the last run; if it changed, the
content read before names code that did not run from start to finish, and
the observations carry none — they count in the totals, as an unknown tree
does. The same holds for the report the run writes: it is left out of both
reads, so it cannot pass for a change. A clean tree edited during its run
is not caught this way; that was so before and is not changed here.

**Not an address.** "Failing since" and "slower since" point into history,
and a dirty tree's content is no commit anyone can check out. They keep
reading clean commits only (`Observation::clean_commit`); a red on a dirty
tree still counts in a streak and still cannot carry its address. The slow
passes read clean timings only, as before: a laptop's uncommitted work is
no point on the timeline. #276 (a green seen only on a dirty tree anchoring
a regression) is a separate question and stays open.

**The honesty limit is the old one.** The content says the code was the
same, not that everything around it was: an ignored `.env`, a database, a
cache can flip an outcome without changing the hash — and could on a clean
commit too. Dirty trees are now as strong as clean ones, not stronger.

**One code, two names.** Work that ran dirty under content `T` and is then
committed exactly as it was is `Commit(B)` from then on, even though
`B`'s tree is `T`: a pass before the commit and a failure after it are not
put together. Keying clean runs on their tree as well would join them, but
it would also part a clean run recorded before that key existed from one
recorded after; left as it is, erring towards proving less.

**The note.** "All N earlier observations were made on a dirty tree"
stopped being the reason: such observations can be evidence now. The
note fires when nothing earlier names its code, and says what that means
today — lines from before sooth read a dirty tree's content, or a tree
edited during its run.
