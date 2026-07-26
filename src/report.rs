//! Presentation layer: the colored human report and the machine JSON.

use std::fmt::Write as _;
use std::time::Duration;

use crate::analyzers::{explain, flaky, history};
use crate::cli::ColorChoice;
use crate::junit;
use crate::runner::RunOutcome;
use crate::verify;

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
    let total = outcomes.len();
    for (index, outcome) in outcomes.iter().enumerate() {
        let status = outcome.status_label();
        let status = if outcome.success {
            style.green(&status)
        } else {
            style.red(&status)
        };
        println!(
            "run {}/{total}: {status} ({:.2?})",
            index + 1,
            outcome.duration
        );
    }
}

/// The test totals plus the slowest-tests list.
pub fn print_summary(summary: &JunitSummary, style: Style) {
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
    println!(
        "tests: {} total — {passed}, {failed}, {errors}, {skipped}",
        summary.total
    );

    if summary.slowest.is_empty() {
        return;
    }
    println!("{}", style.dim("slowest tests:"));
    for (index, (name, duration)) in summary.slowest.iter().enumerate() {
        println!(
            "  {}. {name} {}",
            index + 1,
            style.dim(&format!("({duration:.2?})"))
        );
    }
}

/// The flaky pass, when one ran: mixed outcomes ranked by failure rate,
/// then the always-failing tests — labeled broken, never flaky (see
/// `DECISIONS.md`). Prints nothing when there is nothing to say: the healthy
/// majority is not news.
pub fn print_flaky(analysis: Option<&flaky::Analysis>, style: Style) {
    let Some(analysis) = analysis else { return };
    if analysis.is_empty() {
        return;
    }
    if !analysis.flaky.is_empty() {
        println!("{}", style.bold_red("flaky tests (mixed outcomes):"));
        for (index, test) in analysis.flaky.iter().enumerate() {
            println!(
                "  {}. {} {}",
                index + 1,
                test.id,
                style.red(&format!(
                    "failed {} of {} observed runs ({}%)",
                    test.failed,
                    test.observed(),
                    test.failure_rate_percent()
                ))
            );
        }
    }
    if !analysis.broken.is_empty() {
        println!("{}", style.red("consistently failing (broken, not flaky):"));
        for test in &analysis.broken {
            println!(
                "  - {} {}",
                test.id,
                style.dim(&format!("(failed all {} observed runs)", test.observed()))
            );
        }
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
        style.bold_red(&format!("result: FAILED — {detail} ({total:.2?} total)"))
    } else {
        let tests = summary.map_or_else(String::new, |summary| {
            format!(", {}", count(summary.total, "test"))
        });
        style.bold_green(&format!(
            "result: PASSED — {runs} of {runs} runs{tests} ({total:.2?} total)"
        ))
    }
}

/// The history pass's findings: proven flakes over the accumulated
/// observations, then regression pointers. Prints nothing when there is
/// nothing to say. Tests that failed this run are left out — the explanation
/// below carries their history verdict, and printing the same counts twice
/// buries the run's own news.
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
    let flaky: Vec<&flaky::TestOutcomes> = pass
        .flaky
        .iter()
        .filter(|test| !explained.contains(test.id.as_str()))
        .collect();
    let failing_since: Vec<&history::FailingSince> = pass
        .failing_since
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
            println!(
                "  {}. {} {}",
                index + 1,
                test.id,
                style.red(&format!(
                    "failed {} of {} observed runs ({}%)",
                    test.failed,
                    test.observed(),
                    test.failure_rate_percent()
                ))
            );
        }
    }
    if !failing_since.is_empty() {
        println!("{}", style.red("failing since a commit boundary:"));
        for test in failing_since {
            let short = short_commit(&test.commit);
            println!(
                "  - {} {}",
                test.id,
                style.dim(&format!(
                    "(since {short}, failed the last {} observed runs)",
                    test.failed_runs
                ))
            );
        }
    }
}

/// Verification's verdict, when it ran. The suite verdict is unchanged.
pub fn print_verification(verdict: Option<&verify::Verdict>, style: Style) {
    let Some(verdict) = verdict else { return };
    if verdict.is_empty() {
        return;
    }
    if !verdict.real.is_empty() {
        println!(
            "{}",
            style.bold_red("real failures (reproduced on re-run):")
        );
        for id in &verdict.real {
            println!("  - {id}");
        }
    }
    if !verdict.flaky_or_order.is_empty() {
        println!(
            "{}",
            style.yellow("flaky or order-dependent (passed on re-run in isolation):")
        );
        for id in &verdict.flaky_or_order {
            println!("  - {id}");
        }
    }
    if !verdict.unverified.is_empty() {
        println!(
            "{}",
            style.dim("unverified (the re-run did not cover these):")
        );
        for id in &verdict.unverified {
            println!("  - {id}");
        }
    }
}

/// The run's failures, each labeled with what sooth already knew about it.
pub fn print_explanation(analyses: &Analyses<'_>, style: Style) {
    let Some(explanations) = analyses.explanation else {
        return;
    };
    if explanations.is_empty() {
        return;
    }
    let counts = explain::Counts::of(explanations);
    let headline = explanation_headline(&counts);
    println!(
        "{}",
        if counts.only_known_flakes() {
            style.yellow(&headline)
        } else {
            style.bold_red(&headline)
        }
    );
    for explanation in explanations {
        let detail = match &explanation.verdict {
            explain::Verdict::KnownFlake {
                failed_runs,
                observed_runs,
            } => style.yellow(&format!(
                "known flake (failed {failed_runs} of {observed_runs} observed runs, {}%)",
                flaky::failure_rate_percent(*failed_runs, *observed_runs)
            )),
            explain::Verdict::FailingSince {
                commit,
                failed_runs,
            } => style.red(&format!(
                "failing since {} (the last {failed_runs} observed runs)",
                short_commit(commit)
            )),
            explain::Verdict::Unknown if explanation.quarantined => style.yellow(&format!(
                "quarantined (listed in {})",
                crate::quarantine::FILE_NAME
            )),
            explain::Verdict::Unknown => style.bold_red("new (the history holds no evidence)"),
        };
        // A quarantined-only failure already says so in its verdict.
        let listed = if explanation.quarantined && explanation.verdict != explain::Verdict::Unknown
        {
            style.dim(", quarantined")
        } else {
            String::new()
        };
        println!("  - {} — {detail}{listed}", explanation.id);
    }
    print_history_gap(analyses.history_observations, style);
}

/// `3 failures — 1 known flake, 2 new`; the all-clear sentence when nothing
/// in the run is new.
fn explanation_headline(counts: &explain::Counts) -> String {
    let failures = count(counts.total(), "failure");
    if counts.only_known_flakes() {
        return format!("{failures} — all known flakes, nothing new");
    }
    let parts: Vec<String> = [
        (counts.known_flakes, "known flake", "known flakes"),
        (counts.failing_since, "regression", "regressions"),
        (counts.quarantined, "quarantined", "quarantined"),
        (counts.new, "new", "new"),
    ]
    .iter()
    .filter(|(amount, _, _)| *amount > 0)
    .map(|(amount, singular, plural)| {
        let noun = if *amount == 1 { singular } else { plural };
        format!("{amount} {noun}")
    })
    .collect();
    format!("{failures} — {}", parts.join(", "))
}

/// The short form of a commit, as git itself abbreviates it.
fn short_commit(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// What a "new" label rests on, when it rests on nothing: `None` is a
/// history that was not consulted, `Some(0)` an empty one.
fn print_history_gap(observations: Option<usize>, style: Style) {
    let note = match observations {
        None => {
            "the run history was not consulted: failures are labeled from the \
             quarantine list alone"
        }
        Some(0) => {
            "the run history is empty: every failure reads as new until observations \
             accumulate"
        }
        Some(_) => return,
    };
    println!("{}", style.dim(&format!("note: {note}")));
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

/// The failures `--fail-on-flaky` pardoned via the quarantine file.
pub fn print_pardoned(pardoned: Option<&[String]>, style: Style) {
    let Some(pardoned) = pardoned else { return };
    println!(
        "{}",
        style.yellow(&format!(
            "quarantined failures (pardoned by {}):",
            crate::quarantine::FILE_NAME
        ))
    );
    for id in pardoned {
        println!("  - {id}");
    }
}

/// The verdict when `--fail-on-flaky` pardoned every failure: honest about
/// the failures, explicit about why the exit is 0.
pub fn pardoned_verdict(outcomes: &[RunOutcome], pardoned: usize, style: Style) -> String {
    let total: Duration = outcomes.iter().map(|outcome| outcome.duration).sum();
    style.yellow(&format!(
        "result: PASSED — only quarantined flakes failed ({} pardoned) ({total:.2?} total)",
        count(pardoned, "test")
    ))
}

/// `1 error`, `2 errors` — a count with a correctly pluralized noun.
fn count(amount: usize, noun: &str) -> String {
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
    /// How many observations backed the history pass.
    pub history_observations: Option<usize>,
    pub verify: Option<&'a verify::Verdict>,
    pub pardoned: Option<&'a [String]>,
    pub explanation: Option<&'a [explain::Explanation]>,
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
            let name = json_escape(name);
            format!(
                r#"{{"name":"{name}","duration_seconds":{}}}"#,
                duration.as_secs_f64()
            )
        })
        .collect();

    // Additive within schema_version 1: the flaky/broken fields appear only
    // when a multi-run analysis ran.
    let active = flaky.map_or(String::new(), |pass| {
        format!(
            r#","flaky":[{}],"broken":[{}]"#,
            outcome_entries(&pass.flaky),
            outcome_entries(&pass.broken)
        )
    });

    // Additive within schema_version 1: present whenever the history pass ran.
    let history = history.map_or(String::new(), |pass| {
        let since_entries: Vec<String> = pass
            .failing_since
            .iter()
            .map(|test| {
                format!(
                    r#"{{"name":"{}","commit":"{}","failed_runs":{}}}"#,
                    json_escape(&test.id),
                    json_escape(&test.commit),
                    test.failed_runs
                )
            })
            .collect();
        format!(
            r#","history":{{"flaky":[{}],"failing_since":[{}]}}"#,
            outcome_entries(&pass.flaky),
            since_entries.join(",")
        )
    });

    let verification = verify.map_or(String::new(), |verdict| {
        format!(
            r#","verification":{{"real":[{}],"flaky_or_order_dependent":[{}],"unverified":[{}]}}"#,
            json_ids(&verdict.real),
            json_ids(&verdict.flaky_or_order),
            json_ids(&verdict.unverified)
        )
    });

    let quarantine = pardoned.map_or(String::new(), |ids| {
        format!(r#","quarantine":{{"pardoned":[{}]}}"#, json_ids(ids))
    });

    let explanation = explanation.map_or(String::new(), |explanations| {
        format!(r#","explanation":{}"#, explanation_object(explanations))
    });

    format!(
        r#"{{"schema_version":{JSON_SCHEMA_VERSION},"sooth_version":"{}","runs":[{}],"junit":{{"total":{},"passed":{},"failed":{},"errors":{},"skipped":{},"slowest":[{}]}}{active}{history}{verification}{quarantine}{explanation}}}"#,
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
        json_escape(&report_path.display().to_string()),
        explanation_object(explanations)
    )
}

/// The explanation object both output shapes share. `verdict` names the
/// category the failure was counted in, `quarantined` the label beside it.
fn explanation_object(explanations: &[explain::Explanation]) -> String {
    let entries: Vec<String> = explanations
        .iter()
        .map(|explanation| {
            let name = json_escape(&explanation.id);
            let quarantined = explanation.quarantined;
            match &explanation.verdict {
                explain::Verdict::KnownFlake {
                    failed_runs,
                    observed_runs,
                } => format!(
                    r#"{{"name":"{name}","verdict":"known_flake","quarantined":{quarantined},"failed_runs":{failed_runs},"observed_runs":{observed_runs}}}"#
                ),
                explain::Verdict::FailingSince {
                    commit,
                    failed_runs,
                } => format!(
                    r#"{{"name":"{name}","verdict":"failing_since","quarantined":{quarantined},"commit":"{}","failed_runs":{failed_runs}}}"#,
                    json_escape(commit)
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
        r#"{{"failures":[{}],"counts":{{"known_flakes":{},"failing_since":{},"quarantined":{},"new":{}}},"only_known_flakes":{}}}"#,
        entries.join(","),
        counts.known_flakes,
        counts.failing_since,
        counts.quarantined,
        counts.new,
        counts.only_known_flakes()
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
                json_escape(&test.id),
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
        .map(|id| format!(r#""{}""#, json_escape(id)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Escape a string for inclusion in a hand-rolled JSON string literal.
pub(crate) fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control.is_control() => {
                // `escaped` is a plain `String`; `write!` never fails for it.
                let _ = write!(escaped, "\\u{:04x}", control as u32);
            }
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::{json_escape, to_json, verdict_line, Analyses, JunitSummary, Style};
    use crate::analyzers::explain;
    use crate::cli::ColorChoice;
    use crate::junit::{JunitReport, TestCase, TestStatus};
    use crate::runner::RunOutcome;
    use std::time::Duration;

    fn plain() -> Style {
        Style::from_parts(ColorChoice::Never, false, false)
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
    fn the_explanation_headline_names_every_non_empty_category() {
        let counts = explain::Counts {
            known_flakes: 2,
            failing_since: 1,
            quarantined: 0,
            new: 1,
        };
        assert_eq!(
            super::explanation_headline(&counts),
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
        };
        assert_eq!(
            super::explanation_headline(&counts),
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
        };
        assert_eq!(
            super::explanation_headline(&counts),
            "1 failure — 1 regression"
        );
    }

    #[test]
    fn the_explanation_json_carries_the_verdict_the_counts_and_the_report() {
        let explanations = [
            explain::Explanation {
                id: "c::wobbly".to_owned(),
                verdict: explain::Verdict::KnownFlake {
                    failed_runs: 4,
                    observed_runs: 50,
                },
                quarantined: true,
            },
            explain::Explanation {
                id: "c::fresh".to_owned(),
                verdict: explain::Verdict::Unknown,
                quarantined: false,
            },
        ];
        let json = super::explanation_json(std::path::Path::new("out/report.xml"), &explanations);

        assert!(json.starts_with(r#"{"schema_version":1,"#));
        assert!(json.contains(r#""report":"out/report.xml""#));
        assert!(json.contains(
            r#"{"name":"c::wobbly","verdict":"known_flake","quarantined":true,"failed_runs":4,"observed_runs":50}"#
        ));
        assert!(json.contains(r#"{"name":"c::fresh","verdict":"new","quarantined":false}"#));
        assert!(json
            .contains(r#""counts":{"known_flakes":1,"failing_since":0,"quarantined":0,"new":1}"#));
        assert!(json.contains(r#""only_known_flakes":false"#));
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
    fn json_escape_handles_quotes_backslashes_and_control_characters() {
        assert_eq!(
            json_escape(r#"quote " backslash \ "#),
            r#"quote \" backslash \\ "#
        );
        assert_eq!(json_escape("tab\tnewline\n"), "tab\\tnewline\\n");
        assert_eq!(json_escape("bell\u{7}"), "bell\\u0007");
    }
}
