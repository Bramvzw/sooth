//! Presentation layer: the colored human report and the machine JSON.

use std::fmt::Write as _;
use std::time::Duration;

use crate::analyzers::{flaky, history};
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
/// nothing to say.
pub fn print_history(analysis: Option<&history::Analysis>, style: Style) {
    let Some(analysis) = analysis else { return };
    if analysis.is_empty() {
        return;
    }
    if !analysis.flaky.is_empty() {
        println!(
            "{}",
            style.bold_red("flaky per history (mixed outcomes on one commit):")
        );
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
    if !analysis.failing_since.is_empty() {
        println!("{}", style.red("failing since a commit boundary:"));
        for test in &analysis.failing_since {
            let short = &test.commit[..test.commit.len().min(7)];
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

/// `1 error`, `2 errors` — a count with a correctly pluralized noun.
fn count(amount: usize, noun: &str) -> String {
    if amount == 1 {
        format!("{amount} {noun}")
    } else {
        format!("{amount} {noun}s")
    }
}

/// Hand-rolled JSON: the run outcomes plus the junit summary, versioned via
/// `schema_version`. Revisited when this story landed and deliberately kept
/// hand-rolled: the shape is still small and fixed, so `serde_json` is still
/// not worth a second dependency — see `DECISIONS.md`.
pub fn to_json(
    outcomes: &[RunOutcome],
    summary: &JunitSummary,
    flaky: Option<&flaky::Analysis>,
    history: Option<&history::Analysis>,
    verify: Option<&verify::Verdict>,
) -> String {
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
    let analysis = flaky.map_or(String::new(), |analysis| {
        let entry = |test: &flaky::TestOutcomes| {
            format!(
                r#"{{"name":"{}","failed_runs":{},"observed_runs":{}}}"#,
                json_escape(&test.id),
                test.failed,
                test.observed()
            )
        };
        let flaky_entries: Vec<String> = analysis.flaky.iter().map(entry).collect();
        let broken_entries: Vec<String> = analysis.broken.iter().map(entry).collect();
        format!(
            r#","flaky":[{}],"broken":[{}]"#,
            flaky_entries.join(","),
            broken_entries.join(",")
        )
    });

    // Additive within schema_version 1: present whenever the history pass ran.
    let history = history.map_or(String::new(), |analysis| {
        let flaky_entries: Vec<String> = analysis
            .flaky
            .iter()
            .map(|test| {
                format!(
                    r#"{{"name":"{}","failed_runs":{},"observed_runs":{}}}"#,
                    json_escape(&test.id),
                    test.failed,
                    test.observed()
                )
            })
            .collect();
        let since_entries: Vec<String> = analysis
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
            flaky_entries.join(","),
            since_entries.join(",")
        )
    });

    let verification = verify.map_or(String::new(), |verdict| {
        let names = |ids: &[String]| {
            ids.iter()
                .map(|id| format!(r#""{}""#, json_escape(id)))
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            r#","verification":{{"real":[{}],"flaky_or_order_dependent":[{}],"unverified":[{}]}}"#,
            names(&verdict.real),
            names(&verdict.flaky_or_order),
            names(&verdict.unverified)
        )
    });

    format!(
        r#"{{"schema_version":{JSON_SCHEMA_VERSION},"sooth_version":"{}","runs":[{}],"junit":{{"total":{},"passed":{},"failed":{},"errors":{},"skipped":{},"slowest":[{}]}}{analysis}{history}{verification}}}"#,
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
    use super::{json_escape, to_json, verdict_line, JunitSummary, Style};
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
        let json = to_json(&[outcome(true)], &summary, None, None, None);
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
        let json = to_json(&[outcome(true)], &summary, None, None, None);

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
    fn json_escape_handles_quotes_backslashes_and_control_characters() {
        assert_eq!(
            json_escape(r#"quote " backslash \ "#),
            r#"quote \" backslash \\ "#
        );
        assert_eq!(json_escape("tab\tnewline\n"), "tab\\tnewline\\n");
        assert_eq!(json_escape("bell\u{7}"), "bell\\u0007");
    }
}
