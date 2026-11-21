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
    inferred_preset, record_history, run_failed, unusable_report, write_json_file, HistoryPass,
    Session,
};
use super::{load_history, Outcome, SoothError};
use crate::preset::{Plan, Shuffles};
use crate::reported::{ReportSource, Run};
use crate::{analyzers, cli, history, interrupt, junit, report, reported, runner, verify};

/// How many order-dependent tests are run alone at most. Each one costs the
/// runner's start-up twice, and a suite where everything depends on one
/// bootstrap test would otherwise be started hundreds of times.
const ALONE_AT_MOST: usize = 20;

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

impl Pass<'_> {
    /// Ask the runner for the one test `name` of `classname`. `Ok(None)`
    /// when the run has nothing to say — the runner could not be pointed
    /// at the test, did not start, or left no report: the question stays
    /// open, as it does for `--verify`. Whether the runner then ran that
    /// test and nothing else is for whoever reads the report to check. Its
    /// output is not shown, and how it ended is not the pass's exit.
    fn alone(
        &mut self,
        classname: Option<&str>,
        name: &str,
    ) -> Result<Option<junit::JunitReport>, Stopped> {
        if self.interrupt.requested() {
            return Err(Stopped);
        }
        let Ok(source) = ReportSource::temp() else {
            return Ok(None);
        };
        let Some(spawn) = self.plan.alone(source.path(), classname, name) else {
            return Ok(None);
        };
        let Ok(hidden) = self.capture.open() else {
            return Ok(None);
        };
        match reported::run(&spawn, Some(&source), Some(hidden), self.interrupt) {
            Ok(Run::Finished(ran)) => Ok(ran.report.and_then(Result::ok)),
            Ok(Run::CutShort) => Err(Stopped),
            Err(_) => Ok(None),
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
    if let Some(reason) = plan
        .order_refusal()
        .or_else(|| plan.composer_refusal(std::path::Path::new(".")))
    {
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
    let (seed, shuffled, comparison) = shuffle(&mut pass, &given, args.seed)?;
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

    let proven_flaky = proven_flaky(history.as_ref(), !args.no_history);
    let mut analysis = analyzers::order::analyze(
        comparison,
        again
            .as_ref()
            .map(|(given, shuffled)| analyzers::order::Again { given, shuffled }),
        &proven_flaky,
    );
    // Which way each dependency runs: the tests it found, one by one.
    let alone = isolate(&mut pass, &mut analysis, (&given, &shuffled));
    pass.capture
        .replay_first_failure(pass.json_owns_stdout, style);
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
        shuffles: plan.shuffles(),
        alone,
    };
    match &args.json {
        None => report::print_order(&ended, style),
        Some(None) => report::print_json(&report::order_json(&ended)),
        Some(Some(path)) => {
            report::print_order(&ended, style);
            write_json_file(path, report::order_json(&ended))?;
        }
    }
    // The pass was made and reported; what was cut short is the last part
    // of what was asked, so the exit is 2 all the same.
    if alone.interrupted {
        return Err(SoothError::new(
            "interrupted while the tests were run alone — the report says which way a \
             dependency runs only for the tests that were",
        ));
    }
    Ok(if pass.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The tests the history has proved flaky. They come with the pass that
/// recorded the first run; when that run could not be written, the history
/// is read all the same — a failed write is no reason to forget what the
/// file holds. `consult` is false under `--no-history`.
fn proven_flaky(recorded: Option<&HistoryPass>, consult: bool) -> BTreeSet<String> {
    let ids = |analysis: &analyzers::history::Analysis| -> BTreeSet<String> {
        analysis
            .flaky
            .iter()
            .map(|flake| flake.outcomes.id.clone())
            .collect()
    };
    match recorded {
        Some(recorded) => ids(&recorded.analysis),
        None if consult => {
            let loaded = load_history(std::path::Path::new(history::HISTORY_PATH));
            ids(&analyzers::history::analyze(&loaded.observations))
        }
        None => BTreeSet::new(),
    }
}

/// Run each order-dependent test by itself and let the analysis hear it:
/// that says which way its dependency runs, and takes a test back out that
/// does not behave the same alone twice.
///
/// The failure a test showed in the suite is taken from the run it failed
/// in — the shuffled one when it passed in the given order, else the given
/// one — together with the class and name the runner selects it by.
fn isolate(
    pass: &mut Pass<'_>,
    analysis: &mut analyzers::order::Analysis,
    (given, shuffled): (&junit::JunitReport, &junit::JunitReport),
) -> report::AloneRuns {
    use analyzers::order::PassedIn;
    let failed_in = |passed_in| match passed_in {
        PassedIn::Given => verify::failed_tests(shuffled),
        PassedIn::Shuffled => verify::failed_tests(given),
    };
    let failed_in_shuffled = failed_in(PassedIn::Given);
    let failed_in_given = failed_in(PassedIn::Shuffled);
    let candidates: Vec<&verify::FailedTest> = analysis
        .order_dependent
        .iter()
        .take(ALONE_AT_MOST)
        .filter_map(|difference| {
            let failed = match difference.passed_in {
                PassedIn::Given => &failed_in_shuffled,
                PassedIn::Shuffled => &failed_in_given,
            };
            failed.iter().find(|test| test.id == difference.id)
        })
        .collect();
    let mut alone = report::AloneRuns {
        tests: candidates.len(),
        // Counted now: a test that does not hold alone leaves the list.
        not_run: analysis.order_dependent.len() - candidates.len(),
        interrupted: false,
    };
    if candidates.is_empty() {
        return alone;
    }
    if !pass.json_owns_stdout {
        report::print_order_alone(candidates.len(), verify::VERIFY_RUNS as usize);
    }
    let mut stopped = false;
    let mut heard: Vec<Vec<junit::JunitReport>> = Vec::with_capacity(candidates.len());
    for test in &candidates {
        // The id is the class or file, `::`, and the name; the name alone
        // may itself contain `::`, so it is taken off the end.
        let classname = test
            .id
            .strip_suffix(&test.name)
            .and_then(|class| class.strip_suffix("::"));
        let mut runs = Vec::new();
        for _ in 0..verify::VERIFY_RUNS {
            match pass.alone(classname, &test.name) {
                Ok(Some(report)) => runs.push(report),
                // A run that says nothing leaves the question open, and
                // one more would not close it.
                Ok(None) => {
                    runs.clear();
                    break;
                }
                Err(Stopped) => {
                    stopped = true;
                    runs.clear();
                    break;
                }
            }
        }
        heard.push(runs);
        if stopped {
            break;
        }
    }
    let lone: Vec<analyzers::order::Lone<'_>> = candidates
        .iter()
        .zip(&heard)
        .map(|(test, runs)| analyzers::order::Lone {
            id: &test.id,
            suite_failure: &test.signature,
            runs,
        })
        .collect();
    analyzers::order::direct(analysis, &lone);
    alone.interrupted = stopped;
    alone
}

/// Sooth was asked to stop while a test ran alone.
struct Stopped;

/// Run the suite shuffled and lay it beside the run in its own order. A
/// seed that leaves the tests in the order they already ran in compared
/// nothing, so — unless the seed was asked for — the next one is tried.
/// When the shuffle had nothing to show whatever the seed, that is said
/// instead, and at once.
fn shuffle(
    pass: &mut Pass<'_>,
    given: &junit::JunitReport,
    asked: Option<u32>,
) -> Result<(u32, junit::JunitReport, analyzers::order::Comparison), SoothError> {
    let mut seed = asked.unwrap_or_else(fresh_seed);
    // What an unchanged report shows, as far as the shuffle reaches: files
    // that finish in another order move every test in the report, and no
    // test inside its file.
    let unmoved = match pass.plan.shuffles() {
        Shuffles::TheSuite => {
            "lists the tests that ran in both runs in the order the first run had them"
        }
        Shuffles::EachFile => {
            "lists the tests that ran in both runs in the order the first run had them inside \
             every file"
        }
    };
    for attempt in 1..=SEEDS_TRIED {
        let shuffled = pass.make(Order::Shuffled(seed))?;
        let comparison = analyzers::order::compare(given, &shuffled, pass.plan.shuffles());
        // Before anything else: fewer than two tests have no order, however
        // the reports happen to list them.
        if comparison.compared < 2 {
            return Err(SoothError::new(format!(
                "the order pass needs at least two tests that ran in both runs, and found \
                 {} — one test has no order to depend on",
                comparison.compared
            )));
        }
        if !comparison.shufflable {
            return Err(SoothError::new(nothing_to_show(comparison.namesakes)));
        }
        if comparison.reordered {
            return Ok((seed, shuffled, comparison));
        }
        if asked.is_some() || attempt == SEEDS_TRIED {
            break;
        }
        report::note(&format!("seed {seed} {unmoved} — trying another"));
        seed = seed % 999_999 + 1;
    }
    // What the report lists, not what ran: cases that share a name may
    // have changed places where no report shows it.
    Err(SoothError::new(format!(
        "the shuffled run (seed {seed}) {unmoved}, so the order pass compared nothing. An \
         order set in the runner's configuration or inside a script outranks the flag sooth \
         adds: is one set there?"
    )))
}

/// The refusal for a shuffle no seed could have shown: the runner shuffles
/// inside each file, and no file has two tests a report tells apart.
/// `namesakes` are the tests that stand for several cases under one name,
/// which is the one cause the user can do something about.
fn nothing_to_show(namesakes: usize) -> String {
    let rows = match namesakes {
        0 => String::new(),
        1 => " 1 test stands".to_owned(),
        several => format!(" {several} tests stand"),
    };
    let advice = if namesakes == 0 {
        ""
    } else {
        " for several cases under one name — the rows of a `test.each` with one title, for \
         one — and no report shows those changing places: give them names of their own."
    };
    format!(
        "the order pass has no order to compare: this runner shuffles the tests inside each \
         file, and no file has two tests with names of their own that ran in both \
         runs.{rows}{advice}"
    )
}

/// A seed for this pass. Any will do, as long as it is said: six digits at
/// most, because it is typed back to replay an order.
fn fresh_seed() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    (nanos ^ std::process::id().rotate_left(16)) % 999_999 + 1
}

/// The command line that runs this pass again under the same seed, safe to
/// paste into a shell: a word is left bare only when it is made of
/// characters no shell reads anything into.
fn replay_command(args: &cli::OrderArgs, seed: u32) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./=:,@%+-".contains(c);
    let preset = args
        .preset
        .map_or_else(String::new, |preset| format!(" --preset {}", preset.name()));
    let no_history = if args.no_history { " --no-history" } else { "" };
    let command: Vec<String> = args
        .command
        .iter()
        .map(|word| {
            // `=` is safe everywhere but in zsh, which reads `=word` as
            // the path of a command: at the start of a word, and under
            // `magic_equal_subst` behind another `=` as well.
            let zsh_reads = word.starts_with('=') || word.contains("==");
            if !word.is_empty() && !zsh_reads && word.chars().all(plain) {
                word.clone()
            } else {
                format!("'{}'", word.replace('\'', r"'\''"))
            }
        })
        .collect();
    format!(
        "sooth order --seed {seed}{preset}{no_history} -- {}",
        command.join(" ")
    )
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
        // Anything a shell would read something into is quoted, and a flag
        // of the pass itself comes along.
        let args = order_args(&[
            "sooth",
            "order",
            "--no-history",
            "--",
            "./phpunit",
            "--group",
            "a|b;$HOME",
            "",
            "=ls",
            "--x==ls",
        ]);
        assert_eq!(
            replay_command(&args, 7),
            "sooth order --seed 7 --no-history -- ./phpunit --group 'a|b;$HOME' '' '=ls' '--x==ls'"
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
