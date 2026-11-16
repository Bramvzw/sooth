//! `sooth order`: does the order the tests run in decide what they say?
//!
//! Its own command because it is its own pass (see `DECISIONS.md`): `run
//! --runs N` repeats one order to find what flakes, this compares two
//! orders to find what depends on one. The suite runs once the way it
//! normally runs and once shuffled under a seed; when a test's outcome
//! differs, both orders run a second time, because a flaky test differs
//! too and only an order can be replayed.

use std::collections::BTreeSet;
use std::process::ExitCode;

use super::capture::Capture;
use super::run::{
    inferred_preset, record_history, run_failed, unusable_report, write_json_file, Session,
};
use super::{Outcome, SoothError};
use crate::preset::Plan;
use crate::reported::{ReportSource, Run};
use crate::{analyzers, cli, history, interrupt, junit, report, reported, runner};

/// How many seeds are tried before giving up on a shuffle that leaves the
/// tests in the order they already ran in. A suite of three tests keeps
/// its order under one seed in six; a real one never does.
const SEEDS_TRIED: u32 = 5;

/// Which order one of the pass's runs ran in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// The order the command runs its tests in by itself.
    Given,
    /// Shuffled under this seed.
    Shuffled(u32),
    /// The shuffled order of this seed, a second time.
    ShuffledAgain(u32),
    /// The command's own order, a second time.
    GivenAgain,
}

impl Order {
    /// What the run's own line calls it.
    fn line(self) -> String {
        match self {
            Self::Given => "your order".to_owned(),
            Self::Shuffled(seed) => format!("shuffled, seed {seed}"),
            Self::ShuffledAgain(seed) => format!("shuffled again, seed {seed}"),
            Self::GivenAgain => "your order again".to_owned(),
        }
    }

    /// The seed of a shuffled run; `None` for a run in the command's own
    /// order.
    fn seed(self) -> Option<u32> {
        match self {
            Self::Given | Self::GivenAgain => None,
            Self::Shuffled(seed) | Self::ShuffledAgain(seed) => Some(seed),
        }
    }

    /// The run as a sentence refers to it.
    fn name(self) -> &'static str {
        match self {
            Self::Given => "the run in your order",
            Self::Shuffled(_) => "the shuffled run",
            Self::ShuffledAgain(_) => "the second shuffled run",
            Self::GivenAgain => "the second run in your order",
        }
    }
}

/// The runs of one pass: each quiet, each into a report of its own.
struct Pass<'a> {
    plan: &'a Plan,
    interrupt: &'a interrupt::Interrupt,
    capture: Capture,
    /// Bare `--json` owns stdout: the run lines stay away, and what a quiet
    /// run wrote is shown on stderr.
    json_owns_stdout: bool,
    style: report::Style,
    /// How every run so far ended, in the order they were made.
    outcomes: Vec<runner::RunOutcome>,
    /// Which order each of them ran in.
    orders: Vec<Order>,
    /// Whether any of them failed, by its exit or by its report.
    failed: bool,
}

impl Pass<'_> {
    /// Make one run in `order` and come back with its report. Anything
    /// short of a whole run with a readable report ends the pass: it
    /// compares runs, and half a run compares to nothing.
    fn make(&mut self, order: Order) -> Result<junit::JunitReport, SoothError> {
        let stopped = || {
            SoothError::new(format!(
                "interrupted during {} — the order pass compares whole runs, so there is \
                 nothing to report",
                order.name()
            ))
        };
        if self.interrupt.requested() {
            return Err(stopped());
        }
        let source = ReportSource::temp().map_err(|err| {
            SoothError::new(format!(
                "failed to create a temp directory for the report: {err}"
            ))
        })?;
        let spawn = match order.seed() {
            None => self.plan.run(source.path(), &[]),
            Some(seed) => self
                .plan
                .shuffled(source.path(), seed)
                .expect("a preset that cannot shuffle was refused before the first run"),
        };
        let hidden = self.capture.open().map_err(|err| {
            SoothError::new(format!(
                "failed to create a file for the run's output: {err}"
            ))
        })?;
        let ran = reported::run(&spawn, Some(&source), Some(hidden), self.interrupt)
            .map_err(|err| SoothError::new(format!("failed to run `{}`: {err}", spawn.0[0])))?;
        let Run::Finished(ran) = ran else {
            self.capture
                .replay_cut_short(order.name(), self.json_owns_stdout, self.style);
            return Err(stopped());
        };
        self.outcomes.push(ran.outcome);
        self.orders.push(order);
        let outcome = &self.outcomes[self.outcomes.len() - 1];
        if !self.json_owns_stdout {
            report::print_order_run(&order.line(), outcome, self.style);
        }
        match ran.report {
            Some(Ok(report)) => {
                if run_failed(outcome, Some(&report)) {
                    self.failed = true;
                    self.capture.note_failure(order.name());
                }
                Ok(report)
            }
            Some(Err(error)) => {
                self.capture.replay_before_stopping(
                    order.name(),
                    self.json_owns_stdout,
                    self.style,
                );
                let session = Session {
                    spawn: &spawn,
                    report_source: Some(&source),
                    plan: Some(self.plan),
                    gated_files: 0,
                    interrupt: self.interrupt,
                };
                let made = u32::try_from(self.outcomes.len()).unwrap_or(u32::MAX);
                Err(SoothError {
                    lines: unusable_report(&session, &error, (&self.outcomes, made)),
                })
            }
            None => Err(SoothError::new(
                "the run came back without a report, though it was given a place to write one",
            )),
        }
    }
}

/// Handle `sooth order`. Exit 0 when every run passed, 1 when one failed —
/// a test that fails in another order fails a run — and 2 when the pass
/// could not be made: a runner sooth cannot shuffle, a run cut short, or a
/// shuffle that changed nothing.
pub fn run(args: &cli::OrderArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let preset = args
        .preset
        .or_else(|| inferred_preset(&args.command))
        .ok_or_else(|| {
            SoothError::new(
                "`sooth order` asks the runner itself for a shuffled order, so it has to \
                 know the runner: pass `--preset <RUNNER>` (phpunit, pest or jest) when \
                 the command is that runner",
            )
        })?;
    let plan = Plan::resolve(&args.command, preset);
    if let Some(reason) = plan.order_refusal() {
        return Err(SoothError::new(reason));
    }
    // Read before the first run, as for `sooth run`: what a run writes is
    // not a change to the code it ran.
    let identity =
        (!args.no_history).then(|| history::code_identity(std::path::Path::new("."), None));

    let interrupt = interrupt::Interrupt::listen();
    let mut pass = Pass {
        plan: &plan,
        interrupt: &interrupt,
        capture: Capture::start().map_err(|err| {
            SoothError::new(format!(
                "failed to create a temp directory for the runs' output: {err}"
            ))
        })?,
        json_owns_stdout: matches!(args.json, Some(None)),
        style,
        outcomes: Vec::new(),
        orders: Vec::new(),
        failed: false,
    };

    let given = pass.make(Order::Given)?;
    // The run in the suite's own order is an ordinary run, and recorded as
    // one. The shuffled runs are not: a test that fails only in another
    // order would read as a flake to the history.
    let history = record_history(identity, std::slice::from_ref(&given));
    let (seed, comparison) = shuffle(&mut pass, &given, args.seed)?;
    // A difference between two runs is what a flaky test produces too. An
    // order can be run again and a coincidence cannot, so both are: the
    // shuffled one under its seed, and the suite's own.
    let again = if comparison.differences.is_empty() {
        None
    } else {
        let shuffled = pass.make(Order::ShuffledAgain(seed))?;
        let given = pass.make(Order::GivenAgain)?;
        Some((given, shuffled))
    };
    pass.capture
        .replay_first_failure(pass.json_owns_stdout, style);

    let proven_flaky: BTreeSet<String> = history
        .iter()
        .flat_map(|history| &history.analysis.flaky)
        .map(|flake| flake.outcomes.id.clone())
        .collect();
    let analysis = analyzers::order::analyze(
        comparison,
        again
            .as_ref()
            .map(|(given, shuffled)| analyzers::order::Again { given, shuffled }),
        &proven_flaky,
    );
    let runs: Vec<report::OrderRun> = pass
        .orders
        .iter()
        .zip(&pass.outcomes)
        .map(|(order, outcome)| report::OrderRun {
            seed: order.seed(),
            outcome: outcome.clone(),
        })
        .collect();
    let ended = report::OrderReport {
        analysis: &analysis,
        seed,
        runs: &runs,
        failed: pass.failed,
        replay: &replay_command(args, seed),
        scope: plan.shuffle_scope(),
    };
    match &args.json {
        None => report::print_order(&ended, style),
        Some(None) => report::print_json(&report::order_json(&ended)),
        Some(Some(path)) => {
            report::print_order(&ended, style);
            write_json_file(path, report::order_json(&ended))?;
        }
    }
    Ok(if pass.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Run the suite shuffled and lay it beside the run in its own order. A
/// seed that leaves the tests in the order they already ran in compared
/// nothing, so — unless the seed was asked for — the next one is tried.
fn shuffle(
    pass: &mut Pass<'_>,
    given: &junit::JunitReport,
    asked: Option<u32>,
) -> Result<(u32, analyzers::order::Comparison), SoothError> {
    let mut seed = asked.unwrap_or_else(fresh_seed);
    for attempt in 1..=SEEDS_TRIED {
        let shuffled = pass.make(Order::Shuffled(seed))?;
        let comparison = analyzers::order::compare(given, &shuffled);
        if comparison.reordered {
            return Ok((seed, comparison));
        }
        if comparison.compared < 2 {
            return Err(SoothError::new(format!(
                "the order pass needs at least two tests that ran in both runs, and found \
                 {} — one test has no order to depend on",
                comparison.compared
            )));
        }
        if asked.is_some() || attempt == SEEDS_TRIED {
            break;
        }
        report::note(&format!(
            "seed {seed} left the tests in the order they already ran in — trying another"
        ));
        seed = seed % 999_999 + 1;
    }
    Err(SoothError::new(format!(
        "the shuffled run (seed {seed}) ran the tests in the order they already ran in, so \
         the order pass compared nothing. An order set in the runner's configuration or \
         inside a script outranks the flag sooth adds: is one set there?"
    )))
}

/// A seed for this pass. Any will do, as long as it is said: six digits at
/// most, because it is typed back to replay an order.
fn fresh_seed() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    (nanos ^ std::process::id().rotate_left(16)) % 999_999 + 1
}

/// The command line that runs this pass again under the same seed.
fn replay_command(args: &cli::OrderArgs, seed: u32) -> String {
    let preset = args
        .preset
        .map_or_else(String::new, |preset| format!(" --preset {}", preset.name()));
    let command: Vec<String> = args
        .command
        .iter()
        .map(|word| {
            if word.is_empty() || word.contains(|c: char| c.is_whitespace() || c == '\'') {
                format!("'{}'", word.replace('\'', r"'\''"))
            } else {
                word.clone()
            }
        })
        .collect();
    format!("sooth order --seed {seed}{preset} -- {}", command.join(" "))
}

#[cfg(test)]
mod tests {
    use super::{fresh_seed, replay_command, Order};
    use crate::cli::{Cli, Command, OrderArgs};
    use clap::Parser;

    fn order_args(cmdline: &[&str]) -> OrderArgs {
        let Command::Order(args) = Cli::try_parse_from(cmdline).unwrap().command else {
            panic!("expected `order`");
        };
        args
    }

    #[test]
    fn a_fresh_seed_is_one_a_runner_takes_and_a_person_can_type() {
        for _ in 0..1000 {
            let seed = fresh_seed();
            assert!((1..=999_999).contains(&seed), "{seed}");
        }
    }

    #[test]
    fn the_replay_command_is_the_pass_again_under_its_seed() {
        let args = order_args(&["sooth", "order", "--", "php", "artisan", "test"]);
        assert_eq!(
            replay_command(&args, 48213),
            "sooth order --seed 48213 -- php artisan test"
        );
        // An explicit preset is part of the command; an argument with a
        // space in it is quoted so the line can be pasted.
        let args = order_args(&[
            "sooth",
            "order",
            "--preset",
            "pest",
            "--",
            "vendor/bin/pest",
            "--filter",
            "it's a b",
        ]);
        assert_eq!(
            replay_command(&args, 7),
            r"sooth order --seed 7 --preset phpunit -- vendor/bin/pest --filter 'it'\''s a b'"
        );
    }

    #[test]
    fn each_run_is_named_on_its_line_and_in_a_sentence() {
        assert_eq!(Order::Given.line(), "your order");
        assert_eq!(Order::Shuffled(42).line(), "shuffled, seed 42");
        assert_eq!(Order::ShuffledAgain(42).line(), "shuffled again, seed 42");
        assert_eq!(Order::GivenAgain.line(), "your order again");
        assert_eq!(Order::ShuffledAgain(42).name(), "the second shuffled run");
        assert_eq!(Order::GivenAgain.seed(), None);
        assert_eq!(Order::Shuffled(42).seed(), Some(42));
    }
}
