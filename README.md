# sooth

**The truth about your tests.**

`sooth` is a local, framework-agnostic CLI that runs your existing test command and tells you
what your tests *actually do* — which ones are flaky (and why), which are slow, which depend on
run order, which assert nothing, and which secretly hit the real network instead of a mock.

Zero setup. Single binary. No AI, no API keys. The sanity-check for your (AI-generated) test suite.

```
sooth run -- pytest
```

> 🚧 Work in progress. Reserving the name while the first version is built.

## Status

- [ ] v0.1 — run the suite N times: flaky ranking + slowest tests (from JUnit XML)
- [ ] order-dependence detection (shuffled runs, culprit identification)
- [ ] assertionless / tautological test detection
- [ ] **network egress detection** — flag tests that hit the real network (flagship)

## License

Licensed under either of MIT or Apache-2.0 at your option.
