# sooth

**The truth about your tests.**

`sooth` runs your existing test command, reads the report it produces, and tells you what your
tests *actually do* — which ones are flaky, which are slow, and which depend on run order. Local,
instant, single binary. No server, no dashboard, no AI, no accounts.

```
sooth run --preset pytest -- pytest
```

> **v0.1.0 is out** — `cargo install sooth`. Flaky detection (v0.2) is in active development.
> See `ROADMAP.md` for the full plan and `DECISIONS.md` for why it's built this way.

## Usage

```
# a known runner: the preset injects the reporter flags and reads the report
sooth run --preset pytest -- pytest
sooth run --preset phpunit -- vendor/bin/phpunit

# any other runner: point sooth at the JUnit-XML report your command writes
sooth run --junit report.xml -- ./run-tests.sh

# machine-readable JSON, straight to a file
sooth run --preset pytest --json=sooth.json -- pytest
```

The command after `--` must be the test runner itself — not a wrapper like
`python -m pytest` or `npm test`. Laravel's `php artisan test --parallel` is
such a wrapper; invoke paratest directly instead (it merges the per-worker
reports itself):

```
LARAVEL_PARALLEL_TESTING=1 sooth run --preset phpunit -- \
  vendor/bin/paratest '--runner=\Illuminate\Testing\ParallelRunner' [phpunit args...]
```

Exit codes: `0` — the runner and its report agree everything passed; `1` —
the suite failed; `2` — sooth itself failed (your CI can tell a red suite
from a broken invocation).

## Run history

Every run with a report source appends one observation per test to
`.sooth/history.jsonl` in the directory you run from — add `.sooth/` to your
`.gitignore`. Flaky evidence then accumulates from runs you make anyway:

- **flaky per history** — the same test both passed and failed on one clean
  commit: proven nondeterminism, ranked by failure rate.
- **failing since `<commit>`** — green until a commit, red ever since: a
  regression pointer, deliberately *not* called flaky. Start at `git show
  <commit>`.

Runs on a dirty working tree count in the totals but are never used as
evidence — sooth does not draw conclusions from code it can no longer
identify. Opt out per run with `--no-history`; delete or trim the file
whenever you like, it is yours. In CI, cache `.sooth/` between runs (or pass
it as an artifact) and twenty pipeline runs a day become twenty observations
a day.

## Verify failures

`--verify` removes the `--runs N` cost from daily use. When a run fails,
sooth re-runs *only the failed tests* twice — seconds instead of N× the
suite — and splits the failures:

- **real** — reproduced on every re-run: fix the test or the code.
- **flaky or order-dependent** — passed on re-run in isolation.
- **unverified** — the re-run did not cover them; sooth does not guess.

The suite verdict and exit code are unchanged: sooth classifies failures, it
never absorbs them the way retry plugins do. Requires `--preset` (sooth must
re-invoke your runner on a subset) and a single run; not supported for the
go preset yet.

```bash
sooth run --verify --preset phpunit -- vendor/bin/phpunit
```

## Status

- [x] **v0.1** — `sooth run -- <cmd>` runs your suite once, parses the JUnit XML it produced, and
      prints a summary (total, pass/fail/skip, slowest N) + `--json`. Released.
- [ ] **v0.2** — flaky detection: failure-rate ranking fed by fixed-order repeats *and* a local
      run history that accumulates observations from runs you make anyway (zero extra wall-time).
- [ ] **v0.3** — refined slow-test analysis + order-dependence *detection* (no culprit bisection).
- [ ] **v1.0** — polish, distribution, launch.
- [ ] **spike** (post-v1, timeboxed) — network-egress detection: flag tests that hit the real
      network instead of a mock.

## Framework support

`sooth` is not a "works with anything, zero setup" tool — there is no real JUnit-XML standard, and
producing that report isn't free for every runner. Instead:

- **Built-in presets** (inject the right reporter flags automatically): pytest, PHPUnit, Jest, Go.
- **Bring your own JUnit XML** for everything else — point `sooth` at the report file your runner
  already produces.

## The no-telemetry promise

`sooth` makes zero network calls of its own: no telemetry, no update checks, no crash reporting,
no API keys, no accounts. It reads a file your test command wrote to disk and prints a report —
nothing leaves your machine. Any run history `sooth` keeps (v0.2) is a plain local file in your
repo that you own and move yourself. See `SECURITY.md`.

## Install

```
cargo install sooth
```

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
