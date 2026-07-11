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
      + `cargo test`. No `#[allow]` without a comment saying why; `unsafe` is denied crate-wide.
- [ ] `DECISIONS.md` updated for any non-obvious choice.
- [ ] Docs (`README.md`, `AGENTS.md`) updated if documented behaviour changed.

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
