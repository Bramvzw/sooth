# Contributing to sooth

Thanks for your interest. This is how work flows here — the same process the
maintainer follows, so quality and security stay consistent.

## Development workflow

1. **Pick a story.** Work is tracked as GitHub issues under an epic and a
   milestone (see `ROADMAP.md`). Assign yourself so work isn't duplicated —
   no "starting work" comment needed (see "Where information lives" in
   `AGENTS.md`).
2. **Branch:** `story/<n>-<short-slug>` (or `chore/…`, `fix/…`) off `main`.
3. **Build:** code **plus** a test for the behaviour, a line under
   `## [Unreleased]` in `CHANGELOG.md`, and — for any non-obvious choice — an
   entry in `DECISIONS.md`.
4. **Run the gate locally:** `make check` (must be green).
5. **Commit** with the convention below. One concern per commit.
6. **Open a PR** with `Closes #<n>` in the body. Keep the PR title in the same
   `PREFIX:` form — it becomes the squash-commit on `main`.
7. **CI + review:** CI runs the same gate; logic-heavy changes also get a
   focused code review before merge.
8. **Squash-merge** once green — this closes the issue and ticks the epic.
9. **See it run**, not just the tests, before considering it done.

## Commit convention

`PREFIX: imperative English description`. Allowed prefixes:
`FEAT FIX CHORE DOCS OPS CI SECURITY REFACTOR PERF TEST STYLE`. Enforced locally
by the `commit-msg` hook and in CI by the PR-title check. Reference an issue
with `(#n)` only when it adds clarity; `Closes #n` in the PR body is enough.

## Definition of done (quality gate)

- [ ] Behaviour change has a test covering it.
- [ ] A line was added under `## [Unreleased]` in `CHANGELOG.md`.
- [ ] `make check` is green: `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings`
      + `cargo test`. No `#[allow]` without a reason: a comment on it, or its entry in an
      exception list (`AGENTS.md` invariants, code rules) — not both. `unsafe` is denied crate-wide.
- [ ] `DECISIONS.md` updated for any non-obvious choice.
- [ ] Docs (`README.md`, `AGENTS.md`) updated if documented behaviour changed.
- [ ] Comments follow the comment rule below.
- [ ] Code follows the code rules below; a new test fails against the code before the change.

## Comment rule

A comment exists for the next reader — a fresh AI agent, the owner weeks
later, an external contributor — and states only facts the code cannot show.

- **Rustdoc (`///`, `//!`) on public items**: yes — the definition or
  constraint of the item ("`failed` counts errors too"). Link to
  `DECISIONS.md` for rationale instead of paraphrasing it.
- **Inline comments (`//`)**: only the one constraint line — why *this*
  value or shape, i.e. what a refactor would silently undo.
- **Never**: provenance stories ("observed live: ..."), restating an ADR,
  narrating what the next line does, or arguing the change is correct —
  that is the PR body's job and it dies with the review.
- When in doubt, leave it out. One fact, one home: rationale lives in
  `DECISIONS.md`, change context in the PR body, domain definitions on the
  item that owns them.

## Code rules

The layering lives in `AGENTS.md` (architecture invariants); these are the rules inside a module.
Each is either checked by `make check` (*enforced*) or answers a problem this codebase has had —
a rule with neither is not on the list. The tag names the source: `C-…` the
[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html), `M-…`
Microsoft's [Pragmatic Rust Guidelines](https://microsoft.github.io/rust-guidelines/guidelines/checklist/index.html).

- **One rule, one home.** A domain definition — what counts as evidence, when a run is red, which
  category a verdict gets — is one named function the rest calls. A duplicated rule is always
  fixed; code that merely looks alike waits for its third copy.
- **Solve the problem in front of you.** No generality, option or module for a need that does not
  exist yet (Google's review guide, "over-engineering").
- **One concept, one name** across modules and in a consistent word order (`C-WORD-ORDER`).
- **Meaning lives in types, not in `bool`, `Option` or tuples** at a module boundary
  (`C-CUSTOM-TYPE`). A value with an invariant — a full sha, a test id — gets a newtype that
  guards it (`C-NEWTYPE`, `M-STRONG-TYPES-GUARD`).
- **Domain modules return typed errors; the sentence is built where it is shown** (`C-GOOD-ERR`).
  `Result<_, String>` and `Result<_, ()>` are for the CLI edge only.
- **A bug panics, input never does** (`M-PANIC-ON-BUG`). `unwrap()` is not used outside tests
  (*enforced*); `expect()` states the invariant it relies on (`M-PANIC-MESSAGE`). A `&str` is not
  sliced at a byte offset that input controls (#204).
- **A function fits on a screen and takes at most five parameters** — more is a missing type
  (*enforced*: `too_many_lines`, `too-many-arguments-threshold`). Exception: `emit_output` and
  `verdict` in `main.rs` (#192).
- **A test checks behaviour, not the code's own arithmetic** (`M-TAUTOLOGICAL-TESTS`), is named as
  the sentence it proves, and fails against the code before the change.

## Security

- `sooth` makes no network calls of its own — no telemetry, no update checks
  (see `SECURITY.md`).
- Dependency advisories are scanned by `cargo audit` in CI and weekly;
  Dependabot proposes updates.
- Report vulnerabilities privately — see `SECURITY.md`.

## Setup

```
make setup   # point git at the tracked hooks (run once after cloning)
```

## Releasing (maintainer)

```
make release [bump=minor|major]   # rolls [Unreleased], tags vX.Y.Z, pushes
```

The tag triggers the release workflow, which publishes the crate from CI using
a scoped token stored as a secret — never publish from a laptop.
