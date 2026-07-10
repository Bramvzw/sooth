//! `sooth` — the truth about your tests.
//!
//! Exit codes distinguish whose fault a failure is (grep-style):
//! `0` — every run passed; `1` — at least one run failed;
//! `2` — sooth itself failed (spawn error, flag not implemented yet).

mod cli;
mod runner;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

/// Exit code for "sooth itself failed", as opposed to "the tests failed" (1).
const EXIT_SOOTH_ERROR: u8 = 2;

fn main() -> ExitCode {
    let parsed = Cli::parse();
    match parsed.command {
        Command::Run(args) => run(&args),
    }
}

/// Handle `sooth run`: execute the test command and report how each run went.
fn run(args: &cli::RunArgs) -> ExitCode {
    if let Some(flag) = unimplemented_flag(args) {
        eprintln!("sooth: `{flag}` is not implemented yet (lands in v0.1)");
        return ExitCode::from(EXIT_SOOTH_ERROR);
    }
    match runner::run(&args.command, args.runs) {
        Ok(outcomes) if report(&outcomes) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(err) => {
            let program = &args.command[0];
            eprintln!("sooth: failed to run `{program}`: {err}");
            ExitCode::from(EXIT_SOOTH_ERROR)
        }
    }
}

/// The first flag that parses but is not wired up yet, if any. Rejecting them
/// loudly beats silently ignoring them — this tool's brand is the truth.
fn unimplemented_flag(args: &cli::RunArgs) -> Option<&'static str> {
    if args.preset.is_some() {
        Some("--preset")
    } else if args.json {
        Some("--json")
    } else if args.slowest.is_some() {
        Some("--slowest")
    } else {
        None
    }
}

/// Print a per-run line for each outcome; return `true` iff every run succeeded.
fn report(outcomes: &[runner::RunOutcome]) -> bool {
    let total = outcomes.len();
    for (index, outcome) in outcomes.iter().enumerate() {
        let code = match (outcome.exit_code, outcome.signal) {
            (Some(code), _) => code.to_string(),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "signal".to_owned(),
        };
        println!(
            "run {}/{total}: exit={code} ({:.2?})",
            index + 1,
            outcome.duration
        );
    }
    outcomes.iter().all(|outcome| outcome.success)
}

#[cfg(test)]
mod tests {
    use super::{report, unimplemented_flag};
    use crate::cli::{Cli, Command};
    use crate::runner::RunOutcome;
    use clap::Parser;
    use std::time::Duration;

    fn outcome(success: bool) -> RunOutcome {
        RunOutcome {
            exit_code: Some(i32::from(!success)),
            signal: None,
            success,
            duration: Duration::from_millis(1),
        }
    }

    fn parse_run_args(cmdline: &[&str]) -> crate::cli::RunArgs {
        let parsed = Cli::try_parse_from(cmdline).unwrap();
        let Command::Run(args) = parsed.command;
        args
    }

    #[test]
    fn reports_true_only_when_every_run_succeeded() {
        assert!(report(&[outcome(true), outcome(true)]));
    }

    #[test]
    fn reports_false_when_any_run_failed() {
        assert!(!report(&[outcome(true), outcome(false)]));
    }

    #[test]
    fn a_plain_run_uses_no_unimplemented_flags() {
        let args = parse_run_args(&["sooth", "run", "--runs", "3", "--", "true"]);
        assert_eq!(unimplemented_flag(&args), None);
    }

    #[test]
    fn flags_that_are_not_wired_up_yet_are_rejected() {
        for (cmdline, flag) in [
            (vec!["sooth", "run", "--json", "--", "true"], "--json"),
            (
                vec!["sooth", "run", "--preset", "pytest", "--", "true"],
                "--preset",
            ),
            (
                vec!["sooth", "run", "--slowest", "3", "--", "true"],
                "--slowest",
            ),
        ] {
            let args = parse_run_args(&cmdline);
            assert_eq!(unimplemented_flag(&args), Some(flag));
        }
    }
}
