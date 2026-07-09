//! Command-line interface definitions for `sooth`.

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
    /// Built-in preset for a known test runner (injects the right reporter flags).
    #[arg(long, value_enum)]
    pub preset: Option<Preset>,

    /// How many times to run the suite (fixed order; flaky detection lands in v0.2).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    pub runs: u32,

    /// Emit machine-readable JSON instead of a table.
    #[arg(long)]
    pub json: bool,

    /// How many of the slowest tests to show.
    #[arg(long, default_value_t = 10)]
    pub slowest: usize,

    /// The test command to run, given after `--` (e.g. `sooth run -- pytest`).
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

/// A built-in preset for a known test runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Preset {
    Pytest,
    Phpunit,
    Jest,
    Go,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, Preset};
    use clap::Parser;

    #[test]
    fn captures_the_command_after_double_dash() {
        let parsed = Cli::try_parse_from(["sooth", "run", "--", "pytest", "-k", "foo"]).unwrap();
        let Command::Run(args) = parsed.command;
        assert_eq!(args.command, ["pytest", "-k", "foo"].map(String::from));
        assert_eq!(args.runs, 1);
        assert_eq!(args.slowest, 10);
        assert_eq!(args.preset, None);
        assert!(!args.json);
    }

    #[test]
    fn parses_flags_and_preset() {
        let parsed = Cli::try_parse_from([
            "sooth",
            "run",
            "--preset",
            "pytest",
            "--runs",
            "5",
            "--json",
            "--slowest",
            "3",
            "--",
            "pytest",
        ])
        .unwrap();
        let Command::Run(args) = parsed.command;
        assert_eq!(args.preset, Some(Preset::Pytest));
        assert_eq!(args.runs, 5);
        assert_eq!(args.slowest, 3);
        assert!(args.json);
    }

    #[test]
    fn a_command_is_required() {
        assert!(Cli::try_parse_from(["sooth", "run"]).is_err());
    }

    #[test]
    fn rejects_runs_below_one() {
        // `--runs 0` would run nothing and report a vacuous success; reject it early.
        assert!(Cli::try_parse_from(["sooth", "run", "--runs", "0", "--", "true"]).is_err());
    }
}
