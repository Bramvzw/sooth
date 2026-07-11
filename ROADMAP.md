# Roadmap

`sooth` starts narrow — one thing done excellently before breadth. The version
taxonomy below is the source of truth for scope; GitHub milestones track the
day-to-day breakdown per version.

| Version | Scope |
|---|---|
| **v0.1** | Skeleton: `sooth run -- <cmd>` runs the command once, parses the JUnit XML it produced, prints a summary (total, pass/fail/skip, slowest N) + `--json`. Built-in presets for pytest, PHPUnit, Jest, Go. |
| **v0.2** | **Flaky detection** — two feeds into one failure-rate ranking: fixed run order + N repeats (active, answer now) and a local run-history file accumulating observations from runs that happen anyway (passive, zero extra wall-time; see `DECISIONS.md`). Plus classifying a red run against known flakes. The core value. |
| **v0.3** | **Slow-test analysis** refined + **order-dependence: detection only** (flag "result differs between orderings", no culprit bisection). |
| **v1.0** | Polish + distribution + launch — on the core, not gated on network egress. Includes the PR-subset flake check (repeat only new/changed tests). |
| **spike** (post-v1) | **Network egress** detection — a separate, timeboxed R&D effort (see `DECISIONS.md`). Ships as its own release if it succeeds; does not block v1.0. |

## Explicitly out of scope for now

- **Tautological / assertionless-test detection** — high false-positive rate,
  breaks the "framework-agnostic" promise, and a wrong "worthless test" label
  undermines the "tell the truth" brand. Not planned.
- **Order-dependence culprit identification** — would require combinatorial
  bisection that isn't achievable while staying framework-agnostic. Detection
  only, no blame.

## Correctness note: flaky vs. order-dependence

Strictly separate passes — flaky = fixed order, repeat N times; order-dependence = shuffled
order, compare results. Never both in one pass; the reasoning lives in `DECISIONS.md`.

## Later — deferred until triggered

These are deliberately not part of the day-1 basis; each has a trigger that
brings it in scope:

- `cargo-deny` (license/CVE audit) — once there are more dependencies; starts
  non-blocking.
- `cargo-dist` + Homebrew tap + `install.sh` — at v1.0 launch (distribution,
  not milestone 1).
- 3-OS CI matrix — once the network-egress spike starts (the day-1 product is
  portable and Linux-only CI is sufficient until then).
- `CODE_OF_CONDUCT.md` / issue templates — once there is a first external
  contributor (`CONTRIBUTING.md`, the PR template and Dependabot already
  landed).
