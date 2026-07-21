//! Command-line interface definitions for `sooth`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Parsed top-level command line.
#[derive(Debug, Parser)]
#[command(name = "sooth", version, about = "The truth about your tests")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands `sooth` understands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a test command and report what your tests actually do.
    Run(RunArgs),
}

/// Arguments for `sooth run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Built-in preset for a known test runner: injects the right reporter
    /// flags and reads the report it writes (mutually exclusive with --junit).
    /// The command after `--` must be the runner itself (pytest, phpunit,
    /// jest, gotestsum) — not a wrapper like `python -m pytest` or `npm test`.
    #[arg(long, value_enum, conflicts_with = "junit")]
    pub preset: Option<Preset>,

    /// How many times to run the suite (fixed order). With a report source
    /// and more than one run, mixed outcomes are reported as flaky; the
    /// summary table itself reflects the final run.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    pub runs: u32,

    /// Emit machine-readable JSON (requires --junit or --preset). Bare
    /// `--json` prints it as sooth's final stdout line; `--json=PATH` writes
    /// it to a file and keeps the human report on stdout (note the `=`;
    /// `--json PATH` with a space is rejected).
    // Option<Option<_>> is clap's canonical shape for a flag with an
    // optional value: None = absent, Some(None) = bare, Some(Some(p)) = path.
    #[allow(clippy::option_option)]
    #[arg(long, value_name = "PATH", num_args = 0..=1, require_equals = true)]
    pub json: Option<Option<PathBuf>>,

    /// Do not record this run into `.sooth/history.jsonl` and do not report
    /// from the accumulated history (recording needs a report source; without
    /// one nothing is recorded either way).
    #[arg(long)]
    pub no_history: bool,

    /// After a failing run, re-run only the failed tests and classify them
    /// as real or flaky/order-dependent (needs --preset, single run)
    #[arg(long)]
    pub verify: bool,

    /// Fail only on new flakiness: failures of tests listed in
    /// .sooth-quarantine are pardoned — reported loudly, but the run exits 0
    /// when nothing else failed (requires a report source)
    #[arg(long)]
    pub fail_on_flaky: bool,

    /// When to color the report: auto respects `NO_COLOR` and whether stdout
    /// is a terminal.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorChoice,

    /// How many of the slowest tests to show (default 10; requires --junit or
    /// --preset).
    #[arg(long, value_parser = positive_usize)]
    pub slowest: Option<usize>,

    /// Path to the JUnit-XML report the test command writes during the run.
    /// sooth parses it afterwards and reports totals, status counts, and the
    /// slowest tests; a file that predates the run is rejected as stale.
    #[arg(long, value_name = "PATH")]
    pub junit: Option<PathBuf>,

    /// The test command to run, given after `--` (e.g. `sooth run -- pytest`).
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

/// Parse a strictly positive count: `--slowest 0` would silently hide the
/// ranking, and silent is not this tool's style (mirrors the `--runs` guard).
fn positive_usize(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(0) => Err("must be at least 1".to_owned()),
        Ok(parsed) => Ok(parsed),
        Err(err) => Err(err.to_string()),
    }
}

/// A built-in preset for a known test runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Preset {
    Pytest,
    Phpunit,
    Jest,
    Go,
}

/// When to color the human report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[cfg(test)]
mod tests {
    use super::{Cli, ColorChoice, Command, Preset};
    use clap::Parser;

    fn parse_run_args(cmdline: &[&str]) -> super::RunArgs {
        let parsed = Cli::try_parse_from(cmdline).unwrap();
        let Command::Run(args) = parsed.command;
        args
    }

    #[test]
    fn captures_the_command_after_double_dash() {
        let args = parse_run_args(&["sooth", "run", "--", "pytest", "-k", "foo"]);
        assert_eq!(args.command, ["pytest", "-k", "foo"].map(String::from));
        assert_eq!(args.runs, 1);
        assert_eq!(args.slowest, None);
        assert_eq!(args.preset, None);
        assert_eq!(args.json, None);
        assert_eq!(args.junit, None);
        assert_eq!(args.color, ColorChoice::Auto);
    }

    #[test]
    fn parses_flags_and_preset() {
        let args = parse_run_args(&[
            "sooth",
            "run",
            "--preset",
            "pytest",
            "--runs",
            "5",
            "--json",
            "--slowest",
            "3",
            "--color",
            "never",
            "--",
            "pytest",
        ]);
        assert_eq!(args.preset, Some(Preset::Pytest));
        assert_eq!(args.runs, 5);
        assert_eq!(args.slowest, Some(3));
        assert_eq!(args.json, Some(None));
        assert_eq!(args.color, ColorChoice::Never);
    }

    #[test]
    fn json_takes_an_optional_file_path_with_equals() {
        let args = parse_run_args(&["sooth", "run", "--json=out.json", "--", "true"]);
        assert_eq!(args.json, Some(Some(std::path::PathBuf::from("out.json"))));
    }

    #[test]
    fn a_command_is_required() {
        assert!(Cli::try_parse_from(["sooth", "run"]).is_err());
    }

    #[test]
    fn rejects_slowest_zero() {
        // Mirrors the --runs guard: a silent empty ranking is a surprise,
        // not a feature.
        assert!(Cli::try_parse_from([
            "sooth",
            "run",
            "--junit",
            "r.xml",
            "--slowest",
            "0",
            "--",
            "true",
        ])
        .is_err());
    }

    #[test]
    fn rejects_runs_below_one() {
        // `--runs 0` would run nothing and report a vacuous success; reject it early.
        assert!(Cli::try_parse_from(["sooth", "run", "--runs", "0", "--", "true"]).is_err());
    }

    #[test]
    fn preset_and_junit_are_mutually_exclusive() {
        // A preset manages its own report; pointing sooth at another file at
        // the same time is contradictory input. clap exits 2 on usage errors,
        // matching the "sooth itself failed" exit code.
        assert!(Cli::try_parse_from([
            "sooth", "run", "--preset", "pytest", "--junit", "r.xml", "--", "pytest",
        ])
        .is_err());
    }

    #[test]
    fn parses_the_junit_path() {
        let parsed =
            Cli::try_parse_from(["sooth", "run", "--junit", "target/report.xml", "--", "true"])
                .unwrap();
        let Command::Run(args) = parsed.command;
        assert_eq!(
            args.junit,
            Some(std::path::PathBuf::from("target/report.xml"))
        );
    }
}
