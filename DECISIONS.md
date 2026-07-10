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
unambiguous from the flags of the command it wraps. Flags that parse but are
not implemented yet (`--preset`, `--json`, `--slowest`) fail with a clear "not
implemented yet" error instead of being silently ignored — a tool whose brand
is telling the truth must not pretend to honor a flag.

## The runner inherits the child's stdio and captures only exit status + time

`sooth`'s runner spawns the test command with inherited stdio, so you see your
test output exactly as if you had run it yourself, and records only the exit
code and wall-clock time per run. It deliberately does not buffer the child's
output: the structured signal comes from the JUnit XML the runner produces
(parsed in story #3), not from scraping stdout. Runs execute in a fixed order;
shuffling for order-dependence is a separate pass (see above).

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
for a missing `time` attribute.

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
