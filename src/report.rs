//! Presentation layer: the colored human report and the machine JSON.
//!
//! One file per layer. This one holds what both renderers read — the color
//! style, the view of a run and of the passes that ran on top of it — and
//! the plain lines sooth says outside the report. `human.rs` words the
//! printed report. `json.rs` serialises the machine one and prints nothing:
//! the lint that keeps printing out of every other module holds there too.

mod human;
mod json;

pub use human::{
    print_empty_gate, print_empty_history, print_explanation, print_gate, print_github_import,
    print_history, print_history_beyond_one_load, print_history_gap, print_history_size,
    print_imported, print_nothing_stands_out, print_nothing_to_explain, print_order,
    print_order_alone, print_order_run, print_pardon_gap, print_pruned, print_replay, print_run,
    print_runs, print_summary, print_verdict,
};
pub use json::{explanation_json, order_json, to_json};

use std::time::Duration;

use crate::analyzers::history::PriorEvidence;
use crate::analyzers::{explain, flaky, history, order};
use crate::cli::ColorChoice;
use crate::gate;
use crate::junit;
use crate::runner::RunOutcome;
use crate::verify;

/// The panic hook. A panic is a bug in sooth and is said as one. A reader
/// that closed sooth's stdout or stderr (`sooth history | head`) is not: the
/// failed print panics, and nobody is left to read a message about it.
pub fn internal_error(panic: &std::panic::PanicHookInfo) {
    use std::io::Write as _;

    let payload = panic.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    if message.starts_with("failed printing to std") {
        return;
    }
    let location = panic
        .location()
        .map(|at| format!(" at {}:{}", at.file(), at.line()))
        .unwrap_or_default();
    // Not `eprintln!`: with stderr closed it would panic inside the hook.
    let _ = writeln!(
        std::io::stderr(),
        "sooth: internal error{location}: {message}\n\
         sooth: this is a bug — please report it at https://github.com/Bramvzw/sooth/issues"
    );
}
/// A warning on stderr: sooth carries on and the exit code is unchanged.
#[expect(clippy::print_stderr, reason = "Presentation prints (AGENTS.md).")]
pub fn warn(message: &str) {
    eprintln!("sooth: {message}");
}

/// Something sooth did or assumed on the user's behalf, said on stderr so
/// it stays out of the report.
#[expect(clippy::print_stderr, reason = "Presentation prints (AGENTS.md).")]
pub fn note(message: &str) {
    eprintln!("sooth: {message}");
}

/// Why sooth could not do what it was asked, a line each on stderr. The
/// exit code that goes with it is the caller's.
#[expect(clippy::print_stderr, reason = "Presentation prints (AGENTS.md).")]
pub fn error(lines: &[String]) {
    for line in lines {
        eprintln!("sooth: {line}");
    }
}

/// The machine output: one JSON document, one line of stdout.
#[expect(clippy::print_stdout, reason = "Presentation prints (AGENTS.md).")]
pub fn print_json(document: &str) {
    println!("{document}");
}

/// Whether to emit ANSI colors, resolved once from flag, environment and
/// terminal.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    colored: bool,
}

impl Style {
    /// Resolve from the `--color` flag, `NO_COLOR`, and whether stdout is a
    /// terminal.
    pub fn resolved(choice: ColorChoice) -> Self {
        use std::io::IsTerminal;
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Self::from_parts(choice, no_color, std::io::stdout().is_terminal())
    }

    /// Precedence: an explicit `--color always|never` wins over `NO_COLOR`,
    /// which wins over terminal detection.
    fn from_parts(choice: ColorChoice, no_color: bool, terminal: bool) -> Self {
        let colored = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => !no_color && terminal,
        };
        Self { colored }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.colored {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    fn green(self, text: &str) -> String {
        self.paint("32", text)
    }

    fn red(self, text: &str) -> String {
        self.paint("31", text)
    }

    fn yellow(self, text: &str) -> String {
        self.paint("33", text)
    }

    fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }

    fn bold_green(self, text: &str) -> String {
        self.paint("1;32", text)
    }

    fn bold_red(self, text: &str) -> String {
        self.paint("1;31", text)
    }
}

/// Totals, status counts, and the slowest tests from a parsed JUnit-XML
/// report — the summary the report prints and the JSON serializes.
pub struct JunitSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub error: usize,
    pub skipped: usize,
    pub slowest: Vec<(String, Duration)>,
}

impl JunitSummary {
    /// The summary of a run that never happened: the empty gate still emits
    /// the JSON document, with zero runs and zero tests.
    pub fn empty() -> Self {
        Self {
            total: 0,
            passed: 0,
            failed: 0,
            error: 0,
            skipped: 0,
            slowest: Vec::new(),
        }
    }

    pub fn from_report(report: &junit::JunitReport, slowest: usize) -> Self {
        let mut passed = 0;
        let mut failed = 0;
        let mut error = 0;
        let mut skipped = 0;
        for case in &report.test_cases {
            match case.status {
                junit::TestStatus::Passed => passed += 1,
                junit::TestStatus::Failed => failed += 1,
                junit::TestStatus::Error => error += 1,
                junit::TestStatus::Skipped => skipped += 1,
            }
        }

        let mut by_duration: Vec<&junit::TestCase> = report.test_cases.iter().collect();
        by_duration.sort_by_key(|case| std::cmp::Reverse(case.duration.unwrap_or_default()));

        Self {
            total: report.test_cases.len(),
            passed,
            failed,
            error,
            skipped,
            slowest: by_duration
                .into_iter()
                .take(slowest)
                .map(|case| (case.qualified_name(), case.duration.unwrap_or_default()))
                .collect(),
        }
    }
}

/// One quiet run's output, to be shown after all: which run, why this one,
/// and what it wrote.
pub struct Replay<'a> {
    /// The run by the name its own line gave it: `run 2/20`.
    pub run: &'a str,
    /// Why this run's output is shown, as it reads after "output of run
    /// 2/20, ".
    pub why: &'a str,
    pub output: &'a [u8],
}

/// Everything one `sooth run` ends with: its runs, the passes that ran on
/// top of them, and the verdict the command settled on.
pub struct RunReport<'a> {
    pub outcomes: &'a [RunOutcome],
    /// The final run's totals, when the runs had a report.
    pub summary: Option<&'a JunitSummary>,
    pub analyses: &'a Analyses<'a>,
    /// The worst run's count of failing tests, over every run's report.
    pub report_failures: usize,
    /// How many of the runs failed: by their exit or by their report, the
    /// rule the suite's verdict rests on.
    pub failed_runs: usize,
    /// Whether the suite failed, a pardon taken into account.
    pub failed: bool,
}

/// Everything one `sooth order` ends with.
pub struct OrderReport<'a> {
    pub analysis: &'a order::Analysis,
    /// The seed of the shuffled order.
    pub seed: u32,
    /// The pass's runs in the order they were made: the suite's own order
    /// and the shuffled one, and both again when a test differed.
    pub runs: &'a [OrderRun],
    /// Whether any of those runs failed, by its exit or by its report.
    pub failed: bool,
    /// The command line that makes this pass again under the same seed.
    pub replay: &'a str,
    /// How far the runner's shuffle reached.
    pub shuffles: crate::preset::Shuffles,
    /// The runs of the order-dependent tests by themselves.
    pub alone: AloneRuns,
}

/// How the runs alone went, as numbers: what stands behind the directions
/// the report gives, and behind the ones it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AloneRuns {
    /// How many tests the runner was asked to run alone.
    pub tests: usize,
    /// How many order-dependent tests it was not asked for: more were
    /// found than sooth runs one by one.
    pub not_run: usize,
    /// Whether sooth was asked to stop before all of them had run.
    pub interrupted: bool,
}

/// One run of an order pass.
pub struct OrderRun {
    /// The seed it was shuffled under; `None` for the suite's own order.
    pub seed: Option<u32>,
    pub outcome: RunOutcome,
}

/// What `sooth import --github` read and made of it.
#[derive(Debug, Default)]
pub struct GithubImport {
    pub repo: String,
    pub since: String,
    pub dry_run: bool,
    /// Runs with more than one attempt in the window.
    pub reruns: usize,
    /// Of those, started by something other than a push: left for later.
    pub other_events: usize,
    /// Of those, not finished yet.
    pub unfinished: usize,
    /// Red attempts with a green rerun, recorded now.
    pub recorded: Vec<RerunPair>,
    /// Rerun pairs recorded by an earlier import.
    pub already: usize,
    /// Rerun pairs whose failure the log does not name.
    pub unnamed: Vec<UnnamedPair>,
    /// Red and green attempts that are no pair sooth vouches for: why, and
    /// how many.
    pub not_pairs: std::collections::BTreeMap<String, usize>,
    /// Attempts whose log GitHub keeps no longer.
    pub gone: usize,
}

/// A job that failed and passed on its rerun, with the tests it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerunPair {
    pub job: String,
    pub commit: String,
    pub tests: Vec<String>,
}

/// A rerun pair whose failure the red log does not name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnnamedPair {
    pub job: String,
    pub commit: String,
    pub causes: Vec<crate::ci_log::Cause>,
    /// Why the log could not name it, as the reader said.
    pub why: String,
}

/// One file handed to `sooth import`, and what became of it.
pub struct ImportedFile<'a> {
    pub path: &'a std::path::Path,
    pub fate: ImportFate,
}

/// What became of a file handed to `sooth import`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFate {
    /// The same content as an earlier file of this import: one file.
    SameContent,
    /// Imported before, and `--replace` was not asked for.
    AlreadyImported,
    /// A console log of a green run, which names no tests.
    NothingToRecord,
    /// In the history now, in place of its earlier import or beside the
    /// rest.
    Recorded { observations: usize, replaced: bool },
}

/// `1 error`, `2 errors` — a count with a correctly pluralized noun.
pub(crate) fn count(amount: usize, noun: &str) -> String {
    if amount == 1 {
        format!("{amount} {noun}")
    } else {
        format!("{amount} {noun}s")
    }
}

/// The passes that ran on top of the plain run report. `None` is "this pass
/// did not run"; the printer and the serializer must agree on that.
#[derive(Default)]
pub struct Analyses<'a> {
    /// The active pass (`--runs N`).
    pub flaky: Option<&'a flaky::Analysis>,
    /// The passive pass (the accumulated history).
    pub history: Option<&'a history::Analysis>,
    /// What earlier runs left behind; `None` when the history pass did not
    /// run at all, which is not the same as an empty history.
    pub history_observations: Option<PriorEvidence>,
    pub verify: Option<&'a verify::Verdict>,
    pub pardoned: Option<&'a [String]>,
    pub explanation: Option<&'a [explain::Explanation]>,
    /// The gate's selection when `--changed` gated this run.
    pub gate: Option<&'a gate::Selection>,
    /// Set when sooth was asked to stop before it had done what it was
    /// asked.
    pub interrupted: Option<Interrupted>,
}

/// Sooth was asked to stop before it had done what it was asked: what it
/// was doing then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupted {
    /// Making the runs: `requested` were asked for and fewer finished. The
    /// report covers the ones that finished.
    Runs { requested: usize },
    /// Re-running the failures (`--verify`), after every run had finished:
    /// the report is the suite's own, its failures unverified.
    Verification,
}

impl<'a> Analyses<'a> {
    /// The passes a failure is explained from — the same three, handed to
    /// the explainer under the same names, so a pass is wired once.
    pub fn passes(&self) -> explain::Passes<'a> {
        explain::Passes {
            flaky: self.flaky,
            verify: self.verify,
            history: self.history,
        }
    }
}

#[cfg(test)]
mod fixtures {
    //! Values the tests of the three files build on.

    use super::Style;
    use crate::cli::ColorChoice;
    use crate::junit::{TestCase, TestStatus};
    use crate::runner::RunOutcome;
    use std::time::Duration;

    pub fn plain() -> Style {
        Style::from_parts(ColorChoice::Never, false, false)
    }

    pub fn outcome(success: bool) -> RunOutcome {
        RunOutcome {
            exit_code: Some(i32::from(!success)),
            signal: None,
            success,
            duration: Duration::from_millis(1),
        }
    }

    pub fn test_case(name: &str, status: TestStatus, duration_seconds: f64) -> TestCase {
        TestCase {
            name: name.to_owned(),
            classname: None,
            duration: Some(Duration::from_secs_f64(duration_seconds)),
            status,
            failure_type: None,
            file: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{outcome, plain, test_case};
    use super::{to_json, Analyses, JunitSummary, Style};
    use crate::cli::ColorChoice;
    use crate::junit::{JunitReport, TestStatus};
    use std::time::Duration;

    #[test]
    fn color_resolution_precedence() {
        // --color always wins over NO_COLOR; never wins over a terminal;
        // auto respects NO_COLOR first, then terminal detection.
        assert!(Style::from_parts(ColorChoice::Always, true, false).colored);
        assert!(!Style::from_parts(ColorChoice::Never, false, true).colored);
        assert!(!Style::from_parts(ColorChoice::Auto, true, true).colored);
        assert!(Style::from_parts(ColorChoice::Auto, false, true).colored);
        assert!(!Style::from_parts(ColorChoice::Auto, false, false).colored);
    }

    #[test]
    fn painting_is_a_no_op_without_color() {
        assert_eq!(plain().red("boom"), "boom");
        let colored = Style::from_parts(ColorChoice::Always, false, false);
        assert_eq!(colored.red("boom"), "\x1b[31mboom\x1b[0m");
    }

    #[test]
    fn summarizes_counts_and_ranks_the_slowest_tests() {
        let report = JunitReport {
            test_cases: vec![
                test_case("fast", TestStatus::Passed, 0.1),
                test_case("slow", TestStatus::Failed, 2.0),
                test_case("medium", TestStatus::Skipped, 1.0),
                test_case("erroring", TestStatus::Error, 0.5),
            ],
        };

        let summary = JunitSummary::from_report(&report, 2);

        assert_eq!(summary.total, 4);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.error, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(
            summary.slowest,
            vec![
                ("slow".to_owned(), Duration::from_secs_f64(2.0)),
                ("medium".to_owned(), Duration::from_secs_f64(1.0)),
            ]
        );
    }

    #[test]
    fn the_slowest_ranking_qualifies_names_with_their_classname() {
        let mut with_class = test_case("test_create", TestStatus::Passed, 1.0);
        with_class.classname = Some("Modules.Order.OrderTest".to_owned());
        let report = JunitReport {
            test_cases: vec![with_class, test_case("bare", TestStatus::Passed, 0.5)],
        };

        let summary = JunitSummary::from_report(&report, 10);

        assert_eq!(summary.slowest[0].0, "Modules.Order.OrderTest::test_create");
        assert_eq!(summary.slowest[1].0, "bare");

        // The qualified name deliberately rides into the frozen JSON
        // `name` field.
        let json = to_json(&[outcome(true)], &summary, &Analyses::default());
        assert!(json.contains(r#""name":"Modules.Order.OrderTest::test_create""#));
    }
}
