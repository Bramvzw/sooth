# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-15

### Added

- `sooth run -- <command>`: run any test command (`--runs N` times, fixed
  order) with inherited stdio, per-run `runner exit=`/`runner signal` lines,
  and a closing `result: PASSED/FAILED` verdict.
- Report sources: `--preset pytest|phpunit|jest|go` injects the right
  reporter flags and manages a private temp report; `--junit <PATH>` reads
  the report your command writes during the run (a file that predates the
  run is rejected as stale).
- Tolerant JUnit-XML parser: accepts a `<testsuites>` or bare `<testsuite>`
  root, ignores unknown attributes and elements, and never panics on
  malformed input.
- Totals and a slowest-N ranking with classname-qualified test names,
  colored terminal output (`--color auto|always|never`, `NO_COLOR`
  respected), and machine JSON via bare `--json` (sooth's final stdout
  line) or `--json=PATH` (a clean file), versioned with `schema_version`.
- An exit-code contract: `0` — the runner and its report agree everything
  passed; `1` — the suite failed (either signal); `2` — sooth itself failed.
  Runner/report mismatches and unusable flag combinations fail loudly.
- When the report is unusable and the runner itself failed (a crashed
  worker, an OOM), sooth keeps the run facts it measured: a second stderr
  line names the failed run, its exit status and duration, and points at
  the runner's own output as the likely story.
