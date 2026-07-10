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

### Fixed

- CI now actually uses the toolchain pinned in `rust-toolchain.toml`: the
  toolchain action exported `RUSTUP_TOOLCHAIN`, which silently overrode the
  pin with rolling `stable`.
