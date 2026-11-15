//! Presentation layer: the colored human report and the machine JSON.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "The one module that prints (AGENTS.md)."
)]

use std::borrow::Cow;
use std::time::Duration;

use crate::analyzers::history::PriorEvidence;
use crate::analyzers::{explain, flaky, history};

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
use crate::cli::ColorChoice;
use crate::gate;
use crate::json;
use crate::junit;
use crate::runner::RunOutcome;
use crate::verify;

/// A warning on stderr: sooth carries on and the exit code is unchanged.
pub fn warn(message: &str) {
    eprintln!("sooth: {message}");
}

/// Version of the `--json` shape. Fields are only added within a version;
/// this number is bumped when the shape changes incompatibly.
pub const JSON_SCHEMA_VERSION: u32 = 1;

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
        by_duration.sort_by_key(|case| std::cmp::Reverse(case.duration));

        Self {
            total: report.test_cases.len(),
            passed,
            failed,
            error,
            skipped,
            slowest: by_duration
                .into_iter()
                .take(slowest)
                .map(|case| (case.qualified_name(), case.duration))
                .collect(),
        }
    }
}

/// One line per run. The runner's own exit code is labeled `runner exit=` on
/// purpose: a bare `exit=2` reads as sooth's own exit-code contract, where 2
/// means "sooth itself failed" — two vocabularies that must stay distinct.
pub fn print_runs(outcomes: &[RunOutcome], style: Style) {
    for (index, outcome) in outcomes.iter().enumerate() {
        print_run(index + 1, outcomes.len(), outcome, style);
    }
}

/// One run's line: which run, how the runner exited, how long it took.
pub fn print_run(number: usize, total: usize, outcome: &RunOutcome, style: Style) {
    let status = outcome.status_label();
    let status = if outcome.success {
        style.green(&status)
    } else {
        style.red(&status)
    };
    println!("run {number}/{total}: {status} ({:.2?})", outcome.duration);
}

/// One quiet run's output, to be shown after all: which run, why this one,
/// and what it wrote.
pub struct Replay<'a> {
    pub number: usize,
    pub total: usize,
    /// Why this run's output is shown, as it reads after "output of run
    /// 2/20, ".
    pub why: &'a str,
    pub output: &'a [u8],
}

/// Show a quiet run's output between two lines that say whose it is. The
/// output is the runner's, byte for byte as it would have reached the
/// terminal — both of its streams, in the order it wrote them — closed
/// with a newline so the next line starts on its own. `to_stderr` when
/// stdout belongs to the JSON document.
pub fn print_replay(replay: &Replay<'_>, to_stderr: bool, style: Style) {
    use std::io::Write;
    let Replay {
        number,
        total,
        why,
        output,
    } = replay;
    let opening = style.dim(&format!("output of run {number}/{total}, {why}:"));
    let closing = style.dim(&format!("end of the output of run {number}/{total}"));
    let newline: &[u8] = if output.last().is_some_and(|last| *last != b'\n') {
        b"\n"
    } else {
        b""
    };
    let mut stream: Box<dyn Write> = if to_stderr {
        Box::new(std::io::stderr().lock())
    } else {
        Box::new(std::io::stdout().lock())
    };
    // A closed pipe is the reader's business; see `internal_error`.
    let _ = writeln!(stream, "{opening}");
    let _ = stream.write_all(output);
    let _ = stream.write_all(newline);
    let _ = writeln!(stream, "{closing}");
}

/// What the gate selected: the files about to be repeated, so "why is my
/// suite suddenly three tests" is answered before the first run.
pub fn print_gate(selection: &gate::Selection, style: Style) {
    println!(
        "gate: {} against {}",
        count(selection.files.len(), "changed test file"),
        selection.base
    );
    for file in &selection.files {
        println!("  {}", style.dim(&printable(file)));
    }
}

/// The test totals plus the slowest-tests list. With several `runs` the
/// totals are the final run's and say so: the failures listed below them
/// cover every run, and the two would otherwise seem to disagree. The
/// ranking lists measured durations only, and — unless `ranking_asked_for`
/// with `--slowest` — only when it leaves tests out: seven tests have no
/// "slowest ten".
pub fn print_summary(summary: &JunitSummary, runs: usize, ranking_asked_for: bool, style: Style) {
    let passed = style.green(&format!("{} passed", summary.passed));
    let failed = format!("{} failed", summary.failed);
    let failed = if summary.failed > 0 {
        style.red(&failed)
    } else {
        failed
    };
    let errors = count(summary.error, "error");
    let errors = if summary.error > 0 {
        style.red(&errors)
    } else {
        errors
    };
    let skipped = format!("{} skipped", summary.skipped);
    let skipped = if summary.skipped > 0 {
        style.yellow(&skipped)
    } else {
        skipped
    };
    let label = if runs > 1 {
        "tests (final run)"
    } else {
        "tests"
    };
    println!(
        "{label}: {} total — {passed}, {failed}, {errors}, {skipped}",
        summary.total
    );

    // `time="0"` is a runner that did not measure, not a test that took no
    // time: `0.00ns` in a ranking would be a number nobody observed.
    let measured: Vec<&(String, Duration)> = summary
        .slowest
        .iter()
        .filter(|(_, duration)| !duration.is_zero())
        .collect();
    let leaves_tests_out = summary.total > summary.slowest.len();
    if measured.is_empty() || !(ranking_asked_for || leaves_tests_out) {
        return;
    }
    println!("{}", style.dim("slowest tests:"));
    for (index, (name, duration)) in measured.into_iter().enumerate() {
        println!(
            "  {}. {} {}",
            index + 1,
            identity(name, style),
            style.dim(&format!("({duration:.2?})"))
        );
    }
}

/// The closing verdict line: sooth's suite-level judgement at a glance.
pub fn verdict_line(
    outcomes: &[RunOutcome],
    summary: Option<&JunitSummary>,
    report_failures: usize,
    suite_failed: bool,
    style: Style,
) -> String {
    let total: Duration = outcomes.iter().map(|outcome| outcome.duration).sum();
    let runs = outcomes.len();
    if suite_failed {
        let failed_runs = outcomes.iter().filter(|outcome| !outcome.success).count();
        let detail = if failed_runs > 0 {
            format!("{failed_runs} of {runs} runs failed")
        } else {
            // The runner claimed success but a report disagrees; the
            // stderr note carries the full story.
            format!(
                "the report shows {}",
                count(report_failures, "failing test")
            )
        };
        style.bold_red(&format!("result: ✗ FAILED — {detail} ({total:.2?} total)"))
    } else if summary.is_some_and(|summary| summary.total == 0) {
        // Runner and report agree nothing failed, so the exit stays 0 — but
        // a run of zero tests verified nothing, and a bold green would dress
        // the vacuum up as proof (see `DECISIONS.md`).
        style.yellow(&format!(
            "result: ✓ PASSED — but the report shows 0 tests, so this run proved nothing \
             ({total:.2?} total)"
        ))
    } else {
        let tests = summary.map_or_else(String::new, |summary| {
            format!(", {}", count(summary.total, "test"))
        });
        style.bold_green(&format!(
            "result: ✓ PASSED — {runs} of {runs} runs{tests} ({total:.2?} total)"
        ))
    }
}

/// What the history knows about tests that did *not* fail this run. The ones
/// that did carry their history verdict on their own line above; repeating
/// them here would print the same counts twice.
pub fn print_history(analyses: &Analyses<'_>, style: Style) {
    let Some(pass) = analyses.history else { return };
    if pass.is_empty() {
        return;
    }
    let explained: std::collections::BTreeSet<&str> = analyses
        .explanation
        .unwrap_or_default()
        .iter()
        .map(|explanation| explanation.id.as_str())
        .collect();
    let flaky: Vec<&history::HistoricFlake> = pass
        .flaky
        .iter()
        .filter(|test| !explained.contains(test.outcomes.id.as_str()))
        .collect();
    let failing_since: Vec<&history::FailingSince> = pass
        .failing_since
        .iter()
        .filter(|test| !explained.contains(test.id.as_str()))
        .collect();
    let never_passed: Vec<&history::NeverPassed> = pass
        .never_passed
        .iter()
        .filter(|test| !explained.contains(test.id.as_str()))
        .collect();

    if !flaky.is_empty() {
        let heading = if explained.is_empty() {
            "flaky per history (mixed outcomes on one commit):"
        } else {
            "also flaky per history (these did not fail this run):"
        };
        println!("{}", style.bold_red(heading));
        for (index, test) in flaky.iter().enumerate() {
            let confined = test
                .failures_confined_to
                .as_deref()
                .map_or_else(String::new, |environment| {
                    format!("; every failure in {}", printable(environment))
                });
            println!(
                "  {}. {} {} {}",
                index + 1,
                flaky_mark(style),
                identity(&test.outcomes.id, style),
                style.red(&format!(
                    "failed {} of {} observed runs ({}%{confined}){}",
                    test.outcomes.failed,
                    test.outcomes.observed(),
                    test.outcomes.failure_rate_percent(),
                    red_streak_phrase(test.red_since.as_ref())
                ))
            );
        }
    }
    if !failing_since.is_empty() {
        println!("{}", style.red("failing since a commit boundary:"));
        for test in failing_since {
            let short = short_commit(&test.commit);
            println!(
                "  {} {} {}",
                regression_mark(style),
                identity(&test.id, style),
                style.dim(&format!(
                    "(since {short}, failed the last {} observed runs)",
                    test.failed_runs
                ))
            );
        }
    }
    if !never_passed.is_empty() {
        println!("{}", style.red("never seen passing:"));
        for test in never_passed {
            println!(
                "  {} {} {}",
                style.bold_red("✗"),
                identity(&test.id, style),
                style.dim(&format!(
                    "(failed all {} observed runs, first on {})",
                    test.failed_runs,
                    short_commit(&test.first_commit)
                ))
            );
        }
    }
}

/// `, and every run since abc1234 (3 runs)` — the fact that keeps a known
/// flake from being waved through; empty without a streak.
fn red_streak_phrase(streak: Option<&history::RedStreak>) -> String {
    streak.map_or_else(String::new, |streak| {
        format!(
            ", and every run since {} ({} runs)",
            short_commit(&streak.commit),
            streak.failed_runs
        )
    })
}

/// The one-glance glyph opening a per-test line: what the reader should do
/// with it. Flake evidence and quarantine calm the line; everything else is
/// attention. Single-width characters, not emoji — every monospace font has
/// them and, unlike emoji, they carry the ANSI color that is the point.
/// With `--color never` the characters stay; only the paint drops.
fn glyph(explanation: &explain::Explanation, style: Style) -> String {
    // Quarantine calms a flake's line. A regression on the list keeps its
    // own mark: listed or not, it is not a flake.
    if explanation.quarantined && !explanation.listed_but_not_a_flake() {
        return style.yellow("⊘");
    }
    match &explanation.verdict {
        explain::Verdict::KnownFlake { .. } => flaky_mark(style),
        explain::Verdict::FailingSince { .. } => regression_mark(style),
        explain::Verdict::NeverPassed { .. } => style.bold_red("✗"),
        explain::Verdict::Unknown => match &explanation.observed {
            Some(
                explain::Observed::Flaky { .. }
                | explain::Observed::FlakyOrOrder
                | explain::Observed::MonotoneFlip { .. },
            ) => flaky_mark(style),
            _ => style.bold_red("✗"),
        },
    }
}

/// The marks that appear in more than one section: the per-test glyph and
/// the history lines must paint the same verdict the same way.
fn flaky_mark(style: Style) -> String {
    style.yellow("~")
}

fn regression_mark(style: Style) -> String {
    style.red("▼")
}

/// Text from outside sooth — a report, the history file, git — made safe to
/// print: every control character (an ANSI escape, a carriage return, a
/// newline) becomes U+FFFD. Without this a test name can erase and repaint
/// sooth's own lines, the verdict included, in any terminal or CI log that
/// renders ANSI. The `--json` document escapes them instead (`json::escape`).
fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    text.chars()
        .map(|ch| if ch.is_control() { '\u{fffd}' } else { ch })
        .collect()
}

/// The identity with its namespace prefix dimmed: every character stays —
/// grep and copy-paste keep working — the eye lands on `ClassTest::method`.
/// Control characters are the exception (see [`printable`]).
fn identity(id: &str, style: Style) -> String {
    let id = printable(id);
    let id = id.as_ref();
    let class = id.split_once("::").map_or(id, |(class, _)| class);
    let Some((namespace, _)) = class.rsplit_once('.') else {
        return id.to_owned();
    };
    let Some(rest) = id.strip_prefix(namespace).and_then(|r| r.strip_prefix('.')) else {
        return id.to_owned();
    };
    format!("{}{rest}", style.dim(&format!("{namespace}.")))
}

/// The run's failures, each labeled with what sooth already knew about it.
pub fn print_explanation(analyses: &Analyses<'_>, style: Style) {
    let Some(explanations) = analyses.explanation else {
        return;
    };
    if explanations.is_empty() {
        return;
    }
    // Without a history read, "new" and "nothing in history" would be
    // claims about a file nobody opened.
    let consulted = analyses.history_observations.is_some();
    let counts = explain::Counts::of(explanations);
    let headline = explanation_headline(&counts, consulted);
    println!(
        "{}",
        if counts.only_known_flakes() {
            style.yellow(&headline)
        } else {
            style.bold_red(&headline)
        }
    );
    let reordered = analyses
        .flaky
        .map_or(&[][..], |pass| pass.reordered_runs.as_slice());
    for explanation in explanations {
        // Two independent answers on one line: what this invocation saw, and
        // whether sooth or the team had seen it before.
        let mut parts = Vec::with_capacity(2);
        if let Some(observed) = &explanation.observed {
            parts.push(observed_phrase(observed, reordered, style));
        }
        parts.extend(familiarity_phrase(explanation, consulted, style));
        let known = if parts.is_empty() {
            String::new()
        } else {
            format!(" — {}", parts.join(", "))
        };
        println!(
            "  {} {}{known}",
            glyph(explanation, style),
            identity(&explanation.id, style)
        );
    }
    print_history_gap(analyses.history_observations, style);
}

/// What this invocation's own runs saw — never the history's opinion.
/// `reordered` names the runs that did not share run 1's order: without a
/// fixed order a mixed outcome is no longer proof of nondeterminism, so the
/// claim is weakened rather than repeated with a disclaimer (`DECISIONS.md`).
fn observed_phrase(observed: &explain::Observed, reordered: &[usize], style: Style) -> String {
    match observed {
        explain::Observed::Flaky {
            failed_runs,
            observed_runs,
        } if !reordered.is_empty() => style.yellow(&format!(
            "flaky or order-dependent ({failed_runs} of {observed_runs} runs now; {} did not share run 1's order)",
            runs_phrase(reordered)
        )),
        explain::Observed::Flaky {
            failed_runs,
            observed_runs,
        } => style.red(&format!(
            "flaky ({failed_runs} of {observed_runs} runs now)"
        )),
        explain::Observed::MonotoneFlip {
            flipped_after_run,
            started_green,
        } => {
            let (opening, closing) = if *started_green {
                ("green", "red")
            } else {
                ("red", "green")
            };
            style.yellow(&format!(
                "{opening} up to run {flipped_after_run}, then {closing} for every later \
                 run (the environment may have changed between runs)"
            ))
        }
        explain::Observed::LoneFailure { absent_runs } => style.red(&format!(
            "failed its only observed run (absent from the other {absent_runs} — a name \
             that changes per run hides flakiness)"
        )),
        explain::Observed::Broken { observed_runs } => style.red(&format!(
            "broken ({observed_runs} of {observed_runs} runs now)"
        )),
        explain::Observed::Real => style.bold_red("real (reproduced on re-run)"),
        explain::Observed::FailedDifferently { suite, isolation } => style.red(&format!(
            "failed differently on re-run ({} in the suite, {} in isolation — not a reproduction)",
            printable(suite),
            printable(isolation)
        )),
        explain::Observed::FlakyOrOrder => {
            style.yellow("flaky or order-dependent (passed on re-run in isolation)")
        }
        explain::Observed::Unverified => style.dim("unverified (the re-run did not cover it)"),
    }
}

/// `run 2` / `runs 2 and 3` / `runs 2, 3 and 5`.
fn runs_phrase(runs: &[usize]) -> String {
    let numbers: Vec<String> = runs.iter().map(usize::to_string).collect();
    match numbers.split_last() {
        None => String::new(),
        Some((last, [])) => format!("run {last}"),
        Some((last, rest)) => format!("runs {} and {last}", rest.join(", ")),
    }
}

/// Whether this failure was already known, and on whose authority. `None`
/// when there is nothing to go on: the history was not `consulted` and no
/// list names the test.
fn familiarity_phrase(
    explanation: &explain::Explanation,
    consulted: bool,
    style: Style,
) -> Option<String> {
    if !consulted && explanation.verdict == explain::Verdict::Unknown && !explanation.quarantined {
        return None;
    }
    Some(known_phrase(explanation, style))
}

/// [`familiarity_phrase`] for a failure there is something to say about.
fn known_phrase(explanation: &explain::Explanation, style: Style) -> String {
    let listed = if explanation.quarantined {
        format!(", quarantined in {}", crate::quarantine::FILE_NAME)
    } else {
        String::new()
    };
    match &explanation.verdict {
        explain::Verdict::KnownFlake {
            failed_runs,
            observed_runs,
            failures_confined_to,
            red_since,
        } => {
            // Where it breaks is the first thing to check, so it rides along
            // with the rate rather than waiting in a section below.
            let confined = failures_confined_to
                .as_deref()
                .map_or_else(String::new, |environment| {
                    format!("; every failure in {}", printable(environment))
                });
            let phrase = format!(
                "known flake ({failed_runs} of {observed_runs} in history, {}%{confined}){}{listed}",
                flaky::failure_rate_percent(*failed_runs, *observed_runs),
                red_streak_phrase(red_since.as_ref())
            );
            // A flake that stopped passing is attention, not calm.
            if red_since.is_some() {
                style.red(&phrase)
            } else {
                style.yellow(&phrase)
            }
        }
        explain::Verdict::FailingSince {
            commit,
            failed_runs,
        } => style.red(&format!(
            "failing since {} (the last {failed_runs} observed runs){listed}",
            short_commit(commit)
        )),
        explain::Verdict::NeverPassed {
            first_commit,
            failed_runs,
        } => style.red(&format!(
            "never seen passing (failed all {failed_runs} observed runs, first on {}){listed}",
            short_commit(first_commit)
        )),
        explain::Verdict::Unknown if explanation.quarantined => style.yellow(&format!(
            "quarantined (listed in {})",
            crate::quarantine::FILE_NAME
        )),
        explain::Verdict::Unknown => style.bold_red("new (nothing in history)"),
    }
}

/// `3 failures — 1 known flake, 2 new`; the all-clear sentence when nothing
/// in the run is new.
fn explanation_headline(counts: &explain::Counts, consulted: bool) -> String {
    let failures = count(counts.total(), "failure");
    if counts.only_known_flakes() {
        return format!("{failures} — all known flakes, nothing new");
    }
    // Unread, the history cannot have found a failure new.
    let new = if consulted { counts.new } else { 0 };
    let parts: Vec<String> = [
        (counts.known_flakes, "known flake", "known flakes"),
        (counts.failing_since, "regression", "regressions"),
        (
            counts.never_passed,
            "never seen passing",
            "never seen passing",
        ),
        (counts.quarantined, "quarantined", "quarantined"),
        (new, "new", "new"),
    ]
    .iter()
    .filter(|(amount, _, _)| *amount > 0)
    .map(|(amount, singular, plural)| {
        let noun = if *amount == 1 { singular } else { plural };
        format!("{amount} {noun}")
    })
    .collect();
    if parts.is_empty() {
        return failures;
    }
    format!("{failures} — {}", parts.join(", "))
}

/// The short form of a commit, as git itself abbreviates it. The history
/// file is the user's to edit, so it is printed like any outside text.
fn short_commit(commit: &str) -> Cow<'_, str> {
    printable(
        commit
            .char_indices()
            .nth(7)
            .map_or(commit, |(end, _)| commit.split_at(end).0),
    )
}

/// How much history the analysis above rests on. A file larger than one
/// load reads is said as such: the count is then the newest part, not all.
pub fn print_history_size(observations: usize, truncated: bool) {
    let observations = count(observations, "observation");
    if truncated {
        println!(
            "history: the newest {observations} were read; the file holds more than sooth \
             loads at once, so older ones are out of reach"
        );
    } else {
        println!("history now holds {observations}");
    }
}

/// What a "new" label rests on, when it rests on nothing: `None` is a
/// history that was not consulted, `Some(0)` an empty one.
pub fn print_history_gap(evidence: Option<PriorEvidence>, style: Style) {
    if let Some(note) = history_gap_note(evidence) {
        println!("{}", style.dim(&format!("note: {note}")));
    }
}

/// Why a "new" label may rest on nothing. Three ways that happens, and each
/// is fixed differently — an empty history fills up on its own, a dirty one
/// never will.
fn history_gap_note(evidence: Option<PriorEvidence>) -> Option<String> {
    let Some(prior) = evidence else {
        return Some(
            "the run history was not consulted: failures are labeled from the \
             quarantine list alone"
                .to_owned(),
        );
    };
    if prior.observations == 0 {
        return Some(
            "no observations from earlier runs yet: every failure reads as new until \
             they accumulate"
                .to_owned(),
        );
    }
    // Not empty but unusable, which reads the same to anyone looking at the
    // output and is why this says so explicitly.
    (prior.unusable == prior.observations).then(|| {
        format!(
            "all {} earlier observations were made on a dirty tree and cannot be \
             evidence — commit or stash to let sooth prove flakiness",
            prior.observations
        )
    })
}

/// Why a run whose failures are all known still exits 1: the pardon rests on
/// the committed list, never on sooth's own evidence (see `DECISIONS.md`).
pub fn print_pardon_gap(style: Style) {
    println!(
        "{}",
        style.yellow(&format!(
            "note: not every failure above is in {}, so --fail-on-flaky pardoned nothing — \
             add the ids to pardon them",
            crate::quarantine::FILE_NAME
        ))
    );
}

/// The verdict when `--fail-on-flaky` pardoned every failure: honest about
/// the failures, explicit about why the exit is 0.
/// `not_flakes` of the pardoned are regressions or never passed by the
/// history: the list pardons them all the same, but they are not called
/// flakes here.
pub fn pardoned_verdict(
    outcomes: &[RunOutcome],
    pardoned: usize,
    not_flakes: usize,
    style: Style,
) -> String {
    let total: Duration = outcomes.iter().map(|outcome| outcome.duration).sum();
    let pardoned = count(pardoned, "test");
    let what = if not_flakes == 0 {
        format!("only quarantined flakes failed ({pardoned} pardoned)")
    } else {
        format!(
            "only quarantined tests failed ({pardoned} pardoned, {not_flakes} of them not a \
             flake by the history)"
        )
    };
    style.yellow(&format!("result: ✓ PASSED — {what} ({total:.2?} total)"))
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

/// Hand-rolled JSON: the run outcomes plus the junit summary, versioned via
/// `schema_version`. Revisited when this story landed and deliberately kept
/// hand-rolled: the shape is still small and fixed, so `serde_json` is still
/// not worth a second dependency — see `DECISIONS.md`.
pub fn to_json(outcomes: &[RunOutcome], summary: &JunitSummary, analyses: &Analyses<'_>) -> String {
    let Analyses {
        flaky,
        history,
        verify,
        pardoned,
        explanation,
        gate,
        // Context for the human note only; the JSON carries the counts.
        history_observations: _,
    } = *analyses;
    let runs: Vec<String> = outcomes
        .iter()
        .map(|outcome| {
            let exit_code = outcome
                .exit_code
                .map_or_else(|| "null".to_owned(), |code| code.to_string());
            format!(
                r#"{{"exit_code":{exit_code},"success":{},"duration_seconds":{}}}"#,
                outcome.success,
                outcome.duration.as_secs_f64()
            )
        })
        .collect();

    let slowest: Vec<String> = summary
        .slowest
        .iter()
        .map(|(name, duration)| {
            let name = json::escape(name);
            format!(
                r#"{{"name":"{name}","duration_seconds":{}}}"#,
                duration.as_secs_f64()
            )
        })
        .collect();

    // Additive within schema_version 1: the analysis fields appear only
    // when a multi-run analysis ran.
    let active = flaky.map_or(String::new(), |pass| {
        let reordered: Vec<String> = pass.reordered_runs.iter().map(usize::to_string).collect();
        let monotone: Vec<String> = pass
            .monotone
            .iter()
            .map(|flip| {
                format!(
                    r#"{{"name":"{}","flipped_after_run":{},"started_green":{}}}"#,
                    json::escape(&flip.outcomes.id),
                    flip.flipped_after_run,
                    flip.started_green
                )
            })
            .collect();
        let lone: Vec<String> = pass
            .lone_failures
            .iter()
            .map(|test| {
                format!(
                    r#"{{"name":"{}","absent_runs":{}}}"#,
                    json::escape(&test.id),
                    test.absent_runs
                )
            })
            .collect();
        format!(
            r#","flaky":[{}],"broken":[{}],"monotone":[{}],"lone_failures":[{}],"reordered_runs":[{}]"#,
            outcome_entries(&pass.flaky),
            outcome_entries(&pass.broken),
            monotone.join(","),
            lone.join(","),
            reordered.join(",")
        )
    });

    // Additive within schema_version 1: present whenever the history pass ran.
    let history = history.map_or(String::new(), history_object);

    let verification = verify.map_or(String::new(), verification_object);

    let quarantine = pardoned.map_or(String::new(), |ids| {
        format!(r#","quarantine":{{"pardoned":[{}]}}"#, json_ids(ids))
    });

    let explanation = explanation.map_or(String::new(), |explanations| {
        format!(r#","explanation":{}"#, explanation_object(explanations))
    });

    // Additive within schema_version 1: present when `--changed` gated the run.
    let gate = gate.map_or(String::new(), |selection| {
        format!(
            r#","gate":{{"base":"{}","files":[{}]}}"#,
            json::escape(&selection.base),
            json_ids(&selection.files)
        )
    });

    format!(
        r#"{{"schema_version":{JSON_SCHEMA_VERSION},"sooth_version":"{}","runs":[{}],"junit":{{"total":{},"passed":{},"failed":{},"errors":{},"skipped":{},"slowest":[{}]}}{active}{history}{verification}{quarantine}{explanation}{gate}}}"#,
        env!("CARGO_PKG_VERSION"),
        runs.join(","),
        summary.total,
        summary.passed,
        summary.failed,
        summary.error,
        summary.skipped,
        slowest.join(","),
    )
}

/// The whole machine output of `sooth explain`: no runs and no totals, since
/// explain observes no run.
pub fn explanation_json(
    report_path: &std::path::Path,
    explanations: &[explain::Explanation],
) -> String {
    format!(
        r#"{{"schema_version":{JSON_SCHEMA_VERSION},"sooth_version":"{}","report":"{}","explanation":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        json::escape(&report_path.display().to_string()),
        explanation_object(explanations)
    )
}

/// The explanation object both output shapes share. `verdict` names the
/// category the failure was counted in, `quarantined` the label beside it.
fn explanation_object(explanations: &[explain::Explanation]) -> String {
    let entries: Vec<String> = explanations
        .iter()
        .map(|explanation| {
            let name = json::escape(&explanation.id);
            let quarantined = explanation.quarantined;
            match &explanation.verdict {
                explain::Verdict::KnownFlake {
                    failed_runs,
                    observed_runs,
                    // The environment belongs to the human line; the JSON
                    // carries it under `history`, where the pass owns it.
                    failures_confined_to: _,
                    red_since,
                } => format!(
                    r#"{{"name":"{name}","verdict":"known_flake","quarantined":{quarantined},"failed_runs":{failed_runs},"observed_runs":{observed_runs},"red_since":{}}}"#,
                    red_streak_json(red_since.as_ref())
                ),
                explain::Verdict::FailingSince {
                    commit,
                    failed_runs,
                } => format!(
                    r#"{{"name":"{name}","verdict":"failing_since","quarantined":{quarantined},"commit":"{}","failed_runs":{failed_runs}}}"#,
                    json::escape(commit)
                ),
                explain::Verdict::NeverPassed {
                    first_commit,
                    failed_runs,
                } => format!(
                    r#"{{"name":"{name}","verdict":"never_passed","quarantined":{quarantined},"first_commit":"{}","failed_runs":{failed_runs}}}"#,
                    json::escape(first_commit)
                ),
                explain::Verdict::Unknown => {
                    let verdict = if quarantined { "quarantined" } else { "new" };
                    format!(
                        r#"{{"name":"{name}","verdict":"{verdict}","quarantined":{quarantined}}}"#
                    )
                }
            }
        })
        .collect();
    let counts = explain::Counts::of(explanations);
    format!(
        r#"{{"failures":[{}],"counts":{{"known_flakes":{},"failing_since":{},"never_passed":{},"quarantined":{},"new":{}}},"only_known_flakes":{}}}"#,
        entries.join(","),
        counts.known_flakes,
        counts.failing_since,
        counts.never_passed,
        counts.quarantined,
        counts.new,
        counts.only_known_flakes()
    )
}

/// The verification pass as JSON: the three id lists plus, per failure that
/// failed differently, both signatures.
fn verification_object(verdict: &verify::Verdict) -> String {
    let different: Vec<String> = verdict
        .failed_differently
        .iter()
        .map(|failure| {
            format!(
                r#"{{"name":"{}","suite":"{}","isolation":"{}"}}"#,
                json::escape(&failure.id),
                json::escape(&failure.suite),
                json::escape(&failure.isolation)
            )
        })
        .collect();
    format!(
        r#","verification":{{"real":[{}],"flaky_or_order_dependent":[{}],"failed_differently":[{}],"unverified":[{}]}}"#,
        json_ids(&verdict.real),
        json_ids(&verdict.flaky_or_order),
        different.join(","),
        json_ids(&verdict.unverified)
    )
}

/// The history pass as JSON. `failures_confined_to` is the environment
/// half: `null` unless every failure came from one environment and another
/// environment observed the test too.
fn history_object(pass: &history::Analysis) -> String {
    let flaky_entries: Vec<String> = pass
        .flaky
        .iter()
        .map(|test| {
            let confined = test.failures_confined_to.as_deref().map_or_else(
                || "null".to_owned(),
                |environment| format!(r#""{}""#, json::escape(environment)),
            );
            format!(
                r#"{{"name":"{}","failed_runs":{},"observed_runs":{},"failures_confined_to":{confined},"red_since":{}}}"#,
                json::escape(&test.outcomes.id),
                test.outcomes.failed,
                test.outcomes.observed(),
                red_streak_json(test.red_since.as_ref())
            )
        })
        .collect();
    let never_entries: Vec<String> = pass
        .never_passed
        .iter()
        .map(|test| {
            format!(
                r#"{{"name":"{}","first_commit":"{}","failed_runs":{}}}"#,
                json::escape(&test.id),
                json::escape(&test.first_commit),
                test.failed_runs
            )
        })
        .collect();
    let since_entries: Vec<String> = pass
        .failing_since
        .iter()
        .map(|test| {
            format!(
                r#"{{"name":"{}","commit":"{}","failed_runs":{}}}"#,
                json::escape(&test.id),
                json::escape(&test.commit),
                test.failed_runs
            )
        })
        .collect();
    format!(
        r#","history":{{"flaky":[{}],"failing_since":[{}],"never_passed":[{}]}}"#,
        flaky_entries.join(","),
        since_entries.join(","),
        never_entries.join(",")
    )
}

/// `null`, or the streak a proven flake has been red for.
fn red_streak_json(streak: Option<&history::RedStreak>) -> String {
    streak.map_or_else(
        || "null".to_owned(),
        |streak| {
            format!(
                r#"{{"commit":"{}","failed_runs":{}}}"#,
                json::escape(&streak.commit),
                streak.failed_runs
            )
        },
    )
}

/// A comma-joined JSON array body of per-test outcome counts: the shape both
/// flaky rankings serialize to.
fn outcome_entries(tests: &[flaky::TestOutcomes]) -> String {
    tests
        .iter()
        .map(|test| {
            format!(
                r#"{{"name":"{}","failed_runs":{},"observed_runs":{}}}"#,
                json::escape(&test.id),
                test.failed,
                test.observed()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A comma-joined JSON array body of escaped id strings.
fn json_ids(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!(r#""{}""#, json::escape(id)))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::{to_json, verdict_line, Analyses, JunitSummary, Style};
    use crate::analyzers::{explain, flaky};
    use crate::cli::ColorChoice;
    use crate::junit::{JunitReport, TestCase, TestStatus};
    use crate::runner::RunOutcome;
    use std::time::Duration;

    fn plain() -> Style {
        Style::from_parts(ColorChoice::Never, false, false)
    }

    #[test]
    fn outside_text_loses_its_control_characters_and_nothing_else() {
        assert_eq!(
            super::printable("Tëst.Ünit::méthod → ok"),
            "Tëst.Ünit::méthod → ok"
        );
        assert_eq!(
            super::printable("a\u{1b}[31mb\rc\nd\u{9b}e"),
            "a\u{fffd}[31mb\u{fffd}c\u{fffd}d\u{fffd}e"
        );
        assert_eq!(super::short_commit("ab\u{1b}[2Kcdef0123"), "ab\u{fffd}[2Kc");
    }

    #[test]
    fn short_commit_is_the_first_seven_characters_not_bytes() {
        assert_eq!(super::short_commit("abdeb31c0ffee"), "abdeb31");
        assert_eq!(super::short_commit("abc"), "abc");
        // A history file is user-editable: a commit can hold anything.
        assert_eq!(super::short_commit("ééééééééé"), "ééééééé");
    }

    fn outcome(success: bool) -> RunOutcome {
        RunOutcome {
            exit_code: Some(i32::from(!success)),
            signal: None,
            success,
            duration: Duration::from_millis(1),
        }
    }

    fn test_case(name: &str, status: TestStatus, duration_seconds: f64) -> TestCase {
        TestCase {
            name: name.to_owned(),
            classname: None,
            duration: Duration::from_secs_f64(duration_seconds),
            status,
            failure_type: None,
        }
    }

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

    #[test]
    fn the_verdict_names_failed_runs() {
        let line = verdict_line(&[outcome(true), outcome(false)], None, 0, true, plain());
        assert!(line.contains("FAILED"));
        assert!(line.contains("1 of 2 runs failed"));
    }

    #[test]
    fn the_verdict_blames_the_report_when_runs_were_green() {
        let report = JunitReport {
            test_cases: vec![test_case("bad", TestStatus::Failed, 0.1)],
        };
        let summary = JunitSummary::from_report(&report, 0);
        let line = verdict_line(&[outcome(true)], Some(&summary), 1, true, plain());
        assert!(line.contains("FAILED"));
        assert!(line.contains("the report shows 1 failing test"));
        assert!(!line.contains("1 failing tests"));
    }

    #[test]
    fn the_verdict_counts_tests_on_success() {
        let report = JunitReport {
            test_cases: vec![test_case("ok", TestStatus::Passed, 0.1)],
        };
        let summary = JunitSummary::from_report(&report, 0);
        let line = verdict_line(&[outcome(true)], Some(&summary), 0, false, plain());
        assert!(line.contains("PASSED"));
        assert!(line.contains("1 of 1 runs, 1 test ("));
    }

    #[test]
    fn a_zero_test_pass_says_it_proved_nothing() {
        let line = verdict_line(
            &[outcome(true)],
            Some(&JunitSummary::empty()),
            0,
            false,
            plain(),
        );
        assert!(line.contains("✓ PASSED"), "the exit contract holds: {line}");
        assert!(
            line.contains("0 tests, so this run proved nothing"),
            "got: {line}"
        );
    }

    #[test]
    fn json_output_is_versioned_and_carries_runs_and_summary() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Passed, 0.25)],
            },
            10,
        );
        let json = to_json(&[outcome(true)], &summary, &Analyses::default());

        assert!(json.starts_with(r#"{"schema_version":1,"#));
        assert!(json.contains(&format!(
            r#""sooth_version":"{}""#,
            env!("CARGO_PKG_VERSION")
        )));
        assert!(json.contains(r#""success":true"#));
        assert!(json.contains(r#""total":1"#));
        // Renamed from "error" before v0.1.0 froze the schema: every other
        // count is plural and the human output says "errors".
        assert!(json.contains(r#""errors":0"#));
        assert!(json.contains(r#""name":"a""#));
    }

    #[test]
    fn each_pass_contributes_its_own_phrase_to_the_test_line() {
        // What this invocation saw, whichever pass saw it.
        for (observed, expected) in [
            (
                explain::Observed::Flaky {
                    failed_runs: 1,
                    observed_runs: 3,
                },
                "flaky (1 of 3 runs now)",
            ),
            (
                explain::Observed::Broken { observed_runs: 3 },
                "broken (3 of 3 runs now)",
            ),
            (explain::Observed::Real, "real (reproduced on re-run)"),
            (
                explain::Observed::FlakyOrOrder,
                "flaky or order-dependent (passed on re-run in isolation)",
            ),
            (
                explain::Observed::Unverified,
                "unverified (the re-run did not cover it)",
            ),
            (
                explain::Observed::MonotoneFlip {
                    flipped_after_run: 1,
                    started_green: true,
                },
                "green up to run 1, then red for every later run (the environment may \
                 have changed between runs)",
            ),
            (
                explain::Observed::MonotoneFlip {
                    flipped_after_run: 2,
                    started_green: false,
                },
                "red up to run 2, then green for every later run (the environment may \
                 have changed between runs)",
            ),
            (
                explain::Observed::FailedDifferently {
                    suite: "TypeError".to_owned(),
                    isolation: "RuntimeException".to_owned(),
                },
                "failed differently on re-run (TypeError in the suite, RuntimeException \
                 in isolation — not a reproduction)",
            ),
            (
                explain::Observed::LoneFailure { absent_runs: 19 },
                "failed its only observed run (absent from the other 19 — a name that \
                 changes per run hides flakiness)",
            ),
        ] {
            assert_eq!(super::observed_phrase(&observed, &[], plain()), expected);
        }
    }

    #[test]
    fn a_reordered_run_weakens_the_flaky_claim_instead_of_disclaiming_it() {
        let mixed = explain::Observed::Flaky {
            failed_runs: 2,
            observed_runs: 4,
        };
        assert_eq!(
            super::observed_phrase(&mixed, &[], plain()),
            "flaky (2 of 4 runs now)"
        );
        assert_eq!(
            super::observed_phrase(&mixed, &[2], plain()),
            "flaky or order-dependent (2 of 4 runs now; run 2 did not share run 1's order)"
        );
        assert_eq!(
            super::observed_phrase(&mixed, &[2, 3, 5], plain()),
            "flaky or order-dependent (2 of 4 runs now; runs 2, 3 and 5 did not share run 1's order)"
        );
        // Failing every run is broken whatever the order was.
        assert_eq!(
            super::observed_phrase(
                &explain::Observed::Broken { observed_runs: 4 },
                &[2],
                plain()
            ),
            "broken (4 of 4 runs now)"
        );
    }

    #[test]
    fn the_two_answers_stay_independent_on_one_line() {
        // Broken *and* never seen before is a coherent pair, not a conflict.
        let broken_and_new = explain::Explanation {
            id: "c::fresh".to_owned(),
            observed: Some(explain::Observed::Broken { observed_runs: 2 }),
            verdict: explain::Verdict::Unknown,
            quarantined: false,
        };
        assert_eq!(
            super::known_phrase(&broken_and_new, plain()),
            "new (nothing in history)"
        );

        let known_and_listed = explain::Explanation {
            id: "c::wob".to_owned(),
            observed: Some(explain::Observed::Flaky {
                failed_runs: 1,
                observed_runs: 2,
            }),
            verdict: explain::Verdict::KnownFlake {
                failed_runs: 2,
                observed_runs: 6,
                failures_confined_to: None,
                red_since: None,
            },
            quarantined: true,
        };
        assert_eq!(
            super::known_phrase(&known_and_listed, plain()),
            "known flake (2 of 6 in history, 33%), quarantined in .sooth-quarantine"
        );
    }

    #[test]
    fn a_history_of_nothing_but_dirty_runs_says_so_instead_of_staying_silent() {
        // Measured on a real repo: 9423 observations, every one of them
        // dirty, and every failure reading as "new" without a word about why.
        let all_dirty = super::PriorEvidence {
            observations: 9423,
            unusable: 9423,
        };
        assert_eq!(
            super::history_gap_note(Some(all_dirty)).as_deref(),
            Some(
                "all 9423 earlier observations were made on a dirty tree and cannot be \
                 evidence — commit or stash to let sooth prove flakiness"
            )
        );
    }

    #[test]
    fn some_usable_evidence_is_left_unremarked() {
        // One clean observation is enough for the history to speak for
        // itself; a note about the rest would be noise on every run.
        let mixed = super::PriorEvidence {
            observations: 10,
            unusable: 9,
        };
        assert_eq!(super::history_gap_note(Some(mixed)), None);
    }

    #[test]
    fn an_empty_history_keeps_its_own_note() {
        let empty = super::PriorEvidence {
            observations: 0,
            unusable: 0,
        };
        assert!(super::history_gap_note(Some(empty))
            .is_some_and(|note| note.starts_with("no observations from earlier runs")));
        assert!(super::history_gap_note(None)
            .is_some_and(|note| note.starts_with("the run history was not consulted")));
    }

    #[test]
    fn the_explanation_headline_names_every_non_empty_category() {
        let counts = explain::Counts {
            known_flakes: 2,
            failing_since: 1,
            quarantined: 0,
            new: 1,
            ..explain::Counts::default()
        };
        assert_eq!(
            super::explanation_headline(&counts, true),
            "4 failures — 2 known flakes, 1 regression, 1 new"
        );
    }

    #[test]
    fn a_run_with_nothing_new_says_so_in_one_sentence() {
        let counts = explain::Counts {
            known_flakes: 1,
            failing_since: 0,
            quarantined: 1,
            new: 0,
            ..explain::Counts::default()
        };
        assert_eq!(
            super::explanation_headline(&counts, true),
            "2 failures — all known flakes, nothing new"
        );
    }

    #[test]
    fn a_regression_is_never_folded_into_nothing_new() {
        let counts = explain::Counts {
            known_flakes: 0,
            failing_since: 1,
            quarantined: 0,
            new: 0,
            ..explain::Counts::default()
        };
        assert_eq!(
            super::explanation_headline(&counts, true),
            "1 failure — 1 regression"
        );
    }

    #[test]
    fn the_explanation_json_carries_the_verdict_the_counts_and_the_report() {
        let explanations = [
            explain::Explanation {
                observed: None,
                id: "c::wobbly".to_owned(),
                verdict: explain::Verdict::KnownFlake {
                    failed_runs: 4,
                    observed_runs: 50,
                    failures_confined_to: None,
                    red_since: None,
                },
                quarantined: true,
            },
            explain::Explanation {
                observed: None,
                id: "c::fresh".to_owned(),
                verdict: explain::Verdict::Unknown,
                quarantined: false,
            },
        ];
        let json = super::explanation_json(std::path::Path::new("out/report.xml"), &explanations);

        assert!(json.starts_with(r#"{"schema_version":1,"#));
        assert!(json.contains(r#""report":"out/report.xml""#));
        assert!(json.contains(
            r#"{"name":"c::wobbly","verdict":"known_flake","quarantined":true,"failed_runs":4,"observed_runs":50,"red_since":null}"#
        ));
        assert!(json.contains(r#"{"name":"c::fresh","verdict":"new","quarantined":false}"#));
        assert!(json
            .contains(r#""counts":{"known_flakes":1,"failing_since":0,"never_passed":0,"quarantined":0,"new":1}"#));
        assert!(json.contains(r#""only_known_flakes":false"#));
    }

    #[test]
    fn the_json_carries_which_runs_were_reordered() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = flaky::Analysis {
            reordered_runs: vec![2, 3],
            ..flaky::Analysis::default()
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&pass),
                ..Analyses::default()
            },
        );
        // A consumer must be able to see that the ranking rests on runs that
        // did not share one order.
        assert!(json.contains(r#""reordered_runs":[2,3]"#), "got: {json}");

        let stable = flaky::Analysis::default();
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&stable),
                ..Analyses::default()
            },
        );
        assert!(json.contains(r#""reordered_runs":[]"#), "got: {json}");
    }

    #[test]
    fn the_json_carries_the_flaky_and_broken_entries_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = flaky::Analysis {
            flaky: vec![flaky::TestOutcomes {
                id: "c::wob".to_owned(),
                passed: 1,
                failed: 2,
            }],
            broken: vec![flaky::TestOutcomes {
                id: "c::dead".to_owned(),
                passed: 0,
                failed: 3,
            }],
            ..flaky::Analysis::default()
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&pass),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(r#""flaky":[{"name":"c::wob","failed_runs":2,"observed_runs":3}]"#),
            "got: {json}"
        );
        assert!(
            json.contains(r#""broken":[{"name":"c::dead","failed_runs":3,"observed_runs":3}]"#),
            "got: {json}"
        );
    }

    #[test]
    fn the_json_carries_the_monotone_and_lone_entries_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = flaky::Analysis {
            monotone: vec![flaky::MonotoneFlip {
                outcomes: flaky::TestOutcomes {
                    id: "c::polluted".to_owned(),
                    passed: 1,
                    failed: 2,
                },
                flipped_after_run: 1,
                started_green: true,
            }],
            lone_failures: vec![flaky::LoneFailure {
                id: "c::drift".to_owned(),
                absent_runs: 2,
            }],
            ..flaky::Analysis::default()
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&pass),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(
                r#""monotone":[{"name":"c::polluted","flipped_after_run":1,"started_green":true}]"#
            ),
            "got: {json}"
        );
        assert!(
            json.contains(r#""lone_failures":[{"name":"c::drift","absent_runs":2}]"#),
            "got: {json}"
        );
    }

    #[test]
    fn the_json_carries_the_history_objects_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = crate::analyzers::history::Analysis {
            flaky: vec![crate::analyzers::history::HistoricFlake {
                outcomes: flaky::TestOutcomes {
                    id: "c::wob".to_owned(),
                    passed: 3,
                    failed: 1,
                },
                failures_confined_to: Some("ci".to_owned()),
                red_since: None,
            }],
            failing_since: vec![crate::analyzers::history::FailingSince {
                id: "c::reg".to_owned(),
                commit: "abc1234def".to_owned(),
                failed_runs: 3,
            }],
            never_passed: Vec::new(),
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                history: Some(&pass),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(
                r#""history":{"flaky":[{"name":"c::wob","failed_runs":1,"observed_runs":4,"failures_confined_to":"ci","red_since":null}],"failing_since":[{"name":"c::reg","commit":"abc1234def","failed_runs":3}],"never_passed":[]}"#
            ),
            "got: {json}"
        );
    }

    #[test]
    fn the_json_carries_the_verification_buckets_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let verdict = crate::verify::Verdict {
            real: vec!["c::dead".to_owned()],
            flaky_or_order: vec!["c::wob".to_owned()],
            failed_differently: vec![crate::verify::DifferentFailure {
                id: "c::boot".to_owned(),
                suite: "AssertionError".to_owned(),
                isolation: "RuntimeException".to_owned(),
            }],
            unverified: vec!["c::missed".to_owned()],
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                verify: Some(&verdict),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(
                r#""verification":{"real":["c::dead"],"flaky_or_order_dependent":["c::wob"],"failed_differently":[{"name":"c::boot","suite":"AssertionError","isolation":"RuntimeException"}],"unverified":["c::missed"]}"#
            ),
            "got: {json}"
        );
    }

    #[test]
    fn the_run_json_gains_the_explanation_only_when_the_pass_ran() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.25)],
            },
            0,
        );
        assert!(!to_json(&[outcome(false)], &summary, &Analyses::default())
            .contains(r#""explanation""#));

        let explanations = [explain::Explanation {
            observed: None,
            id: "a".to_owned(),
            verdict: explain::Verdict::FailingSince {
                commit: "abc1234".to_owned(),
                failed_runs: 3,
            },
            quarantined: false,
        }];
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                explanation: Some(&explanations),
                ..Analyses::default()
            },
        );
        assert!(json.contains(
            r#""explanation":{"failures":[{"name":"a","verdict":"failing_since","quarantined":false,"commit":"abc1234","failed_runs":3}]"#
        ));
    }

    #[test]
    fn the_json_carries_the_gate_selection_when_the_gate_ran() {
        let selection = crate::gate::Selection {
            base: "origin/main".to_owned(),
            files: vec!["a/BTest.php".to_owned()],
        };
        let json = to_json(
            &[],
            &JunitSummary::empty(),
            &Analyses {
                gate: Some(&selection),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(r#""gate":{"base":"origin/main","files":["a/BTest.php"]}"#),
            "got: {json}"
        );
        // The empty gate's document: zero runs is a result, not an omission.
        assert!(json.contains(r#""runs":[]"#), "got: {json}");
    }
}
