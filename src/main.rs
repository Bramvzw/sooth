//! `sooth` — the truth about your tests.
//!
//! Exit codes distinguish whose fault a failure is (grep-style):
//! `0` — every run passed; `1` — at least one run failed;
//! `2` — sooth itself failed (spawn error, unparsable report, bad flags).

mod cli;
mod junit;
mod runner;

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use crate::cli::{Cli, Command};

/// Exit code for "sooth itself failed", as opposed to "the tests failed" (1).
const EXIT_SOOTH_ERROR: u8 = 2;

/// How many of the slowest tests the summary shows when `--slowest` is not
/// given.
const DEFAULT_SLOWEST: usize = 10;

fn main() -> ExitCode {
    let parsed = Cli::parse();
    match parsed.command {
        Command::Run(args) => run(&args),
    }
}

/// Handle `sooth run`: execute the test command, then — if `--junit` was
/// given — parse the report it produced and extend the output with totals
/// and the slowest tests. Without `--junit` the output is unchanged from the
/// plain per-run report.
fn run(args: &cli::RunArgs) -> ExitCode {
    if let Some(reason) = rejected_flag(args) {
        eprintln!("sooth: {reason}");
        return ExitCode::from(EXIT_SOOTH_ERROR);
    }

    let outcomes = match runner::run(&args.command, args.runs) {
        Ok(outcomes) => outcomes,
        Err(err) => {
            let program = &args.command[0];
            eprintln!("sooth: failed to run `{program}`: {err}");
            return ExitCode::from(EXIT_SOOTH_ERROR);
        }
    };

    let junit_summary = match &args.junit {
        Some(path) => match junit::parse_file(path) {
            Ok(parsed) => Some(JunitSummary::from_report(
                &parsed,
                args.slowest.unwrap_or(DEFAULT_SLOWEST),
            )),
            Err(err) => {
                eprintln!(
                    "sooth: failed to parse JUnit-XML report `{}`: {err}",
                    path.display()
                );
                return ExitCode::from(EXIT_SOOTH_ERROR);
            }
        },
        None => None,
    };

    match &junit_summary {
        Some(summary) if args.json => println!("{}", to_json(&outcomes, summary)),
        Some(summary) => {
            report(&outcomes);
            print_junit_summary(summary);
        }
        None => report(&outcomes),
    }

    if outcomes.iter().all(|outcome| outcome.success) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// A flag `sooth` cannot honor, if any. Rejecting loudly beats silently
/// ignoring — this tool's brand is the truth. `--preset` is not implemented
/// yet; `--json` and `--slowest` only mean something once there is a report
/// to summarize, so they require `--junit` until presets locate the report
/// automatically.
fn rejected_flag(args: &cli::RunArgs) -> Option<&'static str> {
    if args.preset.is_some() {
        return Some("`--preset` is not implemented yet (lands in v0.1)");
    }
    if args.junit.is_none() {
        if args.json {
            return Some("`--json` requires `--junit <PATH>` (presets locate the report in v0.1)");
        }
        if args.slowest.is_some() {
            return Some(
                "`--slowest` requires `--junit <PATH>` (presets locate the report in v0.1)",
            );
        }
    }
    None
}

/// Print a per-run line for each outcome.
fn report(outcomes: &[runner::RunOutcome]) {
    let total = outcomes.len();
    for (index, outcome) in outcomes.iter().enumerate() {
        let code = match (outcome.exit_code, outcome.signal) {
            (Some(code), _) => code.to_string(),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "signal".to_owned(),
        };
        println!(
            "run {}/{total}: exit={code} ({:.2?})",
            index + 1,
            outcome.duration
        );
    }
}

/// Totals, status counts, and the slowest tests from a parsed JUnit-XML
/// report — the presentation-layer summary `sooth run --junit` prints.
struct JunitSummary {
    total: usize,
    passed: usize,
    failed: usize,
    error: usize,
    skipped: usize,
    slowest: Vec<(String, Duration)>,
}

impl JunitSummary {
    fn from_report(report: &junit::JunitReport, slowest: usize) -> Self {
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
                .map(|case| (case.name.clone(), case.duration))
                .collect(),
        }
    }
}

/// Print the junit summary as plain text: totals, then the slowest tests.
fn print_junit_summary(summary: &JunitSummary) {
    println!(
        "junit: {} total, {} passed, {} failed, {} error, {} skipped",
        summary.total, summary.passed, summary.failed, summary.error, summary.skipped
    );
    if summary.slowest.is_empty() {
        return;
    }
    println!("slowest tests:");
    for (index, (name, duration)) in summary.slowest.iter().enumerate() {
        println!("  {}. {name} ({duration:.2?})", index + 1);
    }
}

/// Hand-rolled JSON: the run outcomes plus the junit summary. There is no
/// other JSON surface yet (the general `--json` report lands in a later
/// story), and the shape here is small and fixed, so `serde_json` is not
/// worth a second dependency for this one story — see `DECISIONS.md`.
fn to_json(outcomes: &[runner::RunOutcome], summary: &JunitSummary) -> String {
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

    format!(
        r#"{{"runs":[{}],"junit":{{"total":{},"passed":{},"failed":{},"error":{},"skipped":{},"slowest":[{}]}}}}"#,
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
fn json_escape(value: &str) -> String {
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
    use super::{json_escape, rejected_flag, report, to_json, JunitSummary};
    use crate::cli::{Cli, Command};
    use crate::junit::{JunitReport, TestCase, TestStatus};
    use crate::runner::RunOutcome;
    use clap::Parser;
    use std::time::Duration;

    fn outcome(success: bool) -> RunOutcome {
        RunOutcome {
            exit_code: Some(i32::from(!success)),
            signal: None,
            success,
            duration: Duration::from_millis(1),
        }
    }

    fn parse_run_args(cmdline: &[&str]) -> crate::cli::RunArgs {
        let parsed = Cli::try_parse_from(cmdline).unwrap();
        let Command::Run(args) = parsed.command;
        args
    }

    #[test]
    fn report_prints_without_panicking_regardless_of_outcome() {
        report(&[outcome(true), outcome(false)]);
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
    fn json_output_includes_runs_and_the_junit_summary() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Passed, 0.25)],
            },
            10,
        );
        let json = to_json(&[outcome(true)], &summary);

        assert!(json.contains(r#""success":true"#));
        assert!(json.contains(r#""total":1"#));
        assert!(json.contains(r#""passed":1"#));
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

    #[test]
    fn a_plain_run_uses_no_rejected_flags() {
        let args = parse_run_args(&["sooth", "run", "--runs", "3", "--", "true"]);
        assert_eq!(rejected_flag(&args), None);
    }

    #[test]
    fn json_and_slowest_are_accepted_together_with_junit() {
        let args = parse_run_args(&[
            "sooth",
            "run",
            "--junit",
            "r.xml",
            "--json",
            "--slowest",
            "3",
            "--",
            "true",
        ]);
        assert_eq!(rejected_flag(&args), None);
    }

    #[test]
    fn preset_and_reportless_flags_are_rejected() {
        for (cmdline, fragment) in [
            (
                vec!["sooth", "run", "--preset", "pytest", "--", "true"],
                "--preset",
            ),
            (vec!["sooth", "run", "--json", "--", "true"], "--json"),
            (
                vec!["sooth", "run", "--slowest", "3", "--", "true"],
                "--slowest",
            ),
        ] {
            let args = parse_run_args(&cmdline);
            let reason = rejected_flag(&args).expect("flag should be rejected");
            assert!(reason.contains(fragment), "{reason} should name {fragment}");
        }
    }
}
