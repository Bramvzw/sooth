# sooth

**The truth about your tests.**

`sooth` runs your existing test command, reads the report it produces, and tells you what your
tests *actually do* — which ones are flaky, which are slow, and which depend on run order. Local,
instant, single binary. No server, no dashboard, no AI, no accounts.

```
sooth run --preset pytest -- pytest
```

> 🚧 Work in progress. Reserving the name and building the first version. See `ROADMAP.md` for the
> full plan and `DECISIONS.md` for why it's built this way.

## Status

- [ ] **v0.1** — `sooth run -- <cmd>` runs your suite once, parses the JUnit XML it produced, and
      prints a summary (total, pass/fail/skip, slowest N) + `--json`.
- [ ] **v0.2** — flaky detection: fixed order, N repeats, failure-rate ranking.
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
nothing leaves your machine. See `SECURITY.md`.

## Install

Not published yet. Once `v0.1` ships:

```
cargo install sooth
```

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
