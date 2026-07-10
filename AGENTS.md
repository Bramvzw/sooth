# sooth — Agent Guide

> AI-readable reference for agents working in this codebase.

## Project overview

`sooth` is a local, single-binary Rust CLI: it runs an existing test command, parses the JUnit XML
that command produces, and reports on the run (flaky tests, slow tests, order-dependence). No
server, no dashboard, no AI, no telemetry. See `README.md` for the pitch, `ROADMAP.md` for scope
per version, and `DECISIONS.md` for the reasoning behind non-obvious choices.

## `src/` module structure

`cli.rs` and `runner.rs` exist; the remaining modules are the shape to grow into, one module per
concern:

```
src/
├── cli.rs        # EXISTS — clap definitions: subcommands, --preset, --runs, --json, --slowest
├── runner.rs      # EXISTS — spawns the test subprocess, captures exit status + wall time
├── junit.rs       # tolerant JUnit-XML union schema; presets inject the right reporter flags
├── analyzers/     # flaky.rs, slow.rs, order.rs — kept strictly separate (see ROADMAP.md)
└── report.rs      # colored terminal table + --json
```

Flags that parse but are not wired up yet (`--preset`, `--json`, `--slowest`) are rejected with a
"not implemented yet" error — never silently ignored. Exit codes are a contract: `0` every run
passed, `1` at least one run failed, `2` sooth itself failed (see `DECISIONS.md`).

`egress` (network-egress detection) is a later, separate module tied to the spike in
`DECISIONS.md` — do not start it as part of the core.

One task per module. Do not add empty placeholder modules ahead of the code that fills them —
`clippy -D warnings` treats unused modules as dead code.

## Commit convention

`PREFIX: imperative English description`, one concern per commit. The allowed prefixes, the full
branch → PR → review → merge workflow, and the definition of done live in `CONTRIBUTING.md` —
enforced by the tracked `commit-msg` hook (`make setup`) and `bin/lint-commit-message.sh`.

## Where information lives

One durable home per fact — link to it instead of restating it:

- **Why a non-obvious choice was made** → `DECISIONS.md`. The only place rationale must live.
- **What a story is** → the issue body. When scope changes before work starts, edit the body; a
  comment is only for a scope/design change worth an audit trail (e.g. "#49 reshapes this story").
- **What a PR changes and why** → the PR body. It survives the squash-merge; branch commit
  messages do not. Review outcomes and merge-resolution notes go in PR comments.
- No "starting work" comments, and never restate in a comment what the issue body, PR body, or
  `DECISIONS.md` already says — assign yourself to the issue instead.

## Verification & definition of done

Run `make check` (fmt + clippy `-D warnings` + tests) before claiming done. The full definition
of done — test coverage, `CHANGELOG.md`, `DECISIONS.md`, docs — lives in `CONTRIBUTING.md`.

## Non-goals / forbidden

- No server, no dashboard, no hosted history — local and instant only.
- No AI in the tool itself.
- No telemetry, update checks, or network calls made by `sooth` itself (see `SECURITY.md`).
- Flaky detection and order-dependence detection are strictly separate passes — never shuffle test
  order and repeat runs in the same analysis (see `ROADMAP.md`).
- No tautological / assertionless-test detection — deliberately cut, see `ROADMAP.md` and
  `DECISIONS.md`.
- No order-dependence culprit bisection — detection only.
