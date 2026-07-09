//! `sooth` — the truth about your tests.

mod cli;

use clap::Parser;

use crate::cli::{Cli, Command};

fn main() {
    let parsed = Cli::parse();
    match parsed.command {
        Command::Run(args) => run_placeholder(&args),
    }
}

/// Placeholder for the `run` subcommand until the runner lands (story #2).
///
/// Echoes the parsed plan so the CLI surface is exercisable and testable now,
/// without yet spawning any test process.
fn run_placeholder(args: &cli::RunArgs) {
    let preset = match args.preset {
        Some(preset) => format!("{preset:?}"),
        None => "none".to_owned(),
    };
    println!(
        "sooth run (skeleton) — preset={preset}, runs={}, json={}, slowest={}, command={:?}",
        args.runs, args.json, args.slowest, args.command
    );
}
