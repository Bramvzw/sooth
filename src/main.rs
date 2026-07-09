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
        Ok(outcomes) => report(&outcomes),
        Err(err) => {
            let program = &args.command[0];
            eprintln!("sooth: failed to run `{program}`: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Print a per-run line and return success only if every run exited cleanly.
fn report(outcomes: &[runner::RunOutcome]) -> ExitCode {
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

    if outcomes.iter().all(|outcome| outcome.success) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
