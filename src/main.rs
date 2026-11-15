//! `sooth` — the truth about your tests.
//!
//! Exit codes distinguish whose fault a failure is (grep-style):
//! `0` — every run passed; `1` — at least one run failed (nonzero runner
//! exit *or* failures in the parsed report — both must agree for a `0`);
//! `2` — sooth itself failed (spawn error, unparsable report, bad flags).

mod analyzers;
mod cli;
mod commands;
mod gate;
mod history;
mod interrupt;
mod json;
mod junit;
mod phpunit_log;
mod preset;
mod quarantine;
mod report;
mod reported;
mod runner;
mod verify;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

/// Exit code for "sooth itself failed", as opposed to "the tests failed" (1).
const EXIT_SOOTH_ERROR: u8 = 2;

fn main() -> ExitCode {
    // A panic is sooth failing, never the tests: exit 2, not Rust's 101
    // (see DECISIONS.md, exit codes).
    std::panic::set_hook(Box::new(report::internal_error));
    std::panic::catch_unwind(dispatch).unwrap_or(ExitCode::from(EXIT_SOOTH_ERROR))
}

/// Parse the command line, run the command, and turn its outcome into the
/// process exit — the one place exit 2 is decided for a command that ran.
fn dispatch() -> ExitCode {
    // No input is known to make sooth panic, and what a panic must look
    // like (exit 2, said as a bug) is tested against the binary: a debug
    // build panics on request. A release build has no such switch.
    #[cfg(debug_assertions)]
    assert!(
        std::env::var_os("SOOTH_TEST_PANIC").is_none(),
        "asked to panic by SOOTH_TEST_PANIC"
    );
    let parsed = match Cli::try_parse() {
        Ok(parsed) => parsed,
        // clap prints and exits: 2 for a usage error, 0 for --help.
        Err(error) => {
            // Lossily: `args()` panics on an argument that is not UTF-8,
            // and the tip only compares words clap itself could read.
            let typed: Vec<String> = std::env::args_os()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            cli::with_tip(error, &typed).exit()
        }
    };
    let outcome = match parsed.command {
        Command::Run(args) => commands::run::run(args),
        Command::Explain(args) => commands::explain::run(&args),
        Command::Import(args) => commands::import::run(&args),
        Command::History(args) => commands::history::run(&args),
    };
    outcome.unwrap_or_else(|error| {
        report::error(&error.lines);
        ExitCode::from(EXIT_SOOTH_ERROR)
    })
}
