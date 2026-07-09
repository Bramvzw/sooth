//! `sooth` — the truth about your tests.

mod cli;
mod runner;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

fn main() -> ExitCode {
    let parsed = Cli::parse();
    match parsed.command {
        Command::Run(args) => run(&args),
    }
}

/// Handle `sooth run`: execute the test command and report how each run went.
fn run(args: &cli::RunArgs) -> ExitCode {
    match runner::run(&args.command, args.runs) {
        Ok(outcomes) if report(&outcomes) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(err) => {
            let program = &args.command[0];
            eprintln!("sooth: failed to run `{program}`: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Print a per-run line for each outcome; return `true` iff every run succeeded.
fn report(outcomes: &[runner::RunOutcome]) -> bool {
    let total = outcomes.len();
    for (index, outcome) in outcomes.iter().enumerate() {
        let code = outcome
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
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
    use super::report;
    use crate::runner::RunOutcome;
    use std::time::Duration;

    fn outcome(success: bool) -> RunOutcome {
        RunOutcome {
            exit_code: Some(i32::from(!success)),
            success,
            duration: Duration::from_millis(1),
        }
    }

    #[test]
    fn reports_true_only_when_every_run_succeeded() {
        assert!(report(&[outcome(true), outcome(true)]));
    }

    #[test]
    fn reports_false_when_any_run_failed() {
        assert!(!report(&[outcome(true), outcome(false)]));
    }
}
