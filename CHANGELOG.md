# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Project governance basis: licensing, CI, commit-message linting, `AGENTS.md`,
  `ROADMAP.md`, `DECISIONS.md`, `SECURITY.md`.
- `sooth run` CLI skeleton (clap): `--preset`, `--runs`, `--json`, `--slowest`,
  and the test command given after `--`.
- `sooth run` executes the test command (`--runs` times, fixed order) with
  inherited stdio and reports each run's exit code and wall-time.
- `junit` module: tolerant JUnit-XML parser (`parse_str`/`parse_file`) that
  accepts either a `<testsuites>` or a bare `<testsuite>` root, ignores
  unknown attributes/elements, and never panics on malformed input.
- `sooth run --junit <PATH>` parses that report after the run and extends
  the output with total/passed/failed/error/skipped counts and the slowest
  `--slowest` tests; `--json` emits this as JSON alongside the run outcomes.
- On Unix, a run terminated by a signal reports the signal number instead of a
  bare "signal".
- Presets are wired up: `--preset pytest|phpunit|jest|go` injects the right
  reporter flags (pytest `--junit-xml`, PHPUnit `--log-junit`, gotestsum
  `--junitfile`, Jest `--reporters` + `JEST_JUNIT_OUTPUT_FILE`), has the
  runner write into a fresh private temp directory, parses the report after
  the run, and cleans it up. A preset run that produces no report fails with
  an actionable hint (reporter missing, or the command is a wrapper instead
  of the runner itself). `--preset` conflicts with `--junit`.

### Changed

- Exit codes now distinguish outcomes: `0` all runs passed, `1` at least one
  run failed, `2` sooth itself failed (spawn error, unparsable report, bad
  flags).
- Flags sooth cannot honor fail loudly instead of being silently ignored:
  `--json` and `--slowest` require a report source (`--junit` or `--preset`).
- With a report source, exit 0 now requires the runner *and* its report to
  agree: a runner that exits 0 while the report contains failures or errors
  makes sooth exit 1, with a loud stderr note about the mismatch.
- The crate description no longer advertises cut or post-v1 features
  (assertionless-test detection, network egress).

### Fixed

- CI now actually uses the toolchain pinned in `rust-toolchain.toml`: the
  toolchain action exported `RUSTUP_TOOLCHAIN`, which silently overrode the
  pin with rolling `stable`.
- The release workflow verifies the tag matches the crate version and
  publishes via crates.io Trusted Publishing (OIDC) instead of a long-lived
  `CARGO_REGISTRY_TOKEN` secret.
