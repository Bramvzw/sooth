# sooth — Agent Guide

> AI-readable reference for agents working in this codebase.

## Project overview

`sooth` is a local, single-binary Rust CLI: it runs an existing test command, parses the JUnit XML
that command produces, and reports on the run (flaky tests, slow tests, order-dependence). No
server, no dashboard, no AI, no telemetry. See `README.md` for the pitch, `ROADMAP.md` for scope
per version, and `DECISIONS.md` for the reasoning behind non-obvious choices.

## Intended `src/` module structure

Code does not exist yet beyond a placeholder `main.rs` — this is the shape to grow into, one
module per concern:

```
src/
├── cli.rs        # clap definitions: subcommands, --preset, --runs, --json, --slowest, --fail-on-flaky
├── runner.rs      # spawns the test subprocess, captures exit code + wall time; fixed or shuffled order
├── junit.rs       # tolerant JUnit-XML union schema; presets inject the right reporter flags
├── analyzers/     # flaky.rs, slow.rs, order.rs — kept strictly separate (see ROADMAP.md)
└── report.rs      # colored terminal table + --json
```

`egress` (network-egress detection) is a later, separate module tied to the spike in
`DECISIONS.md` — do not start it as part of the core.

One task per module. Do not add empty placeholder modules ahead of the code that fills them —
`clippy -D warnings` treats unused modules as dead code.

## Commit convention

`PREFIX: imperative English description`. Allowed prefixes: `FEAT FIX CHORE DOCS OPS CI SECURITY
REFACTOR PERF TEST STYLE`. Enforced by the tracked `commit-msg` hook (`.githooks/`, install it with
`make setup`) and by `bin/lint-commit-message.sh`. One concern per commit. The full branch → PR → review → merge workflow lives in `CONTRIBUTING.md`.

## Definition of done

- [ ] Behavior change has a test covering it.
- [ ] A line was added under `## [Unreleased]` in `CHANGELOG.md`.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean — no `#[allow]` without a comment
      explaining why.
- [ ] Docs (`README.md`, `AGENTS.md`) are updated if the change alters documented behavior.

## Verification

```
make check   # cargo fmt --check + cargo clippy --all-targets -- -D warnings + cargo test
```

## Non-goals / forbidden

- No server, no dashboard, no hosted history — local and instant only.
- No AI in the tool itself.
- No telemetry, update checks, or network calls made by `sooth` itself (see `SECURITY.md`).
- Flaky detection and order-dependence detection are strictly separate passes — never shuffle test
  order and repeat runs in the same analysis (see `ROADMAP.md`).
- No tautological / assertionless-test detection — deliberately cut, see `ROADMAP.md` and
  `DECISIONS.md`.
- No order-dependence culprit bisection — detection only.
