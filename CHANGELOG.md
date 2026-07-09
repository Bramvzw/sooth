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
