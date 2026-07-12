//! `sooth` — the truth about your tests.
//!
//! Exit codes distinguish whose fault a failure is (grep-style):
//! `0` — every run passed; `1` — at least one run failed (nonzero runner
//! exit *or* failures in the parsed report — both must agree for a `0`);
//! `2` — sooth itself failed (spawn error, unparsable report, bad flags).

mod cli;
mod junit;
mod preset;
mod report;
mod runner;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::report::JunitSummary;

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

/// Handle `sooth run`: execute the test command, then — when a report source
/// is given — parse the JUnit-XML report and extend the output with totals and
/// the slowest tests. `--junit` points at a report the command already writes;
/// `--preset` injects the right reporter flags and manages a temp report
/// itself. Without either, the output is the plain per-run report.
fn run(args: &cli::RunArgs) -> ExitCode {
    if let Some(reason) = rejected_flag(args) {
        eprintln!("sooth: {reason}");
        return ExitCode::from(EXIT_SOOTH_ERROR);
    }
    let style = report::Style::resolved(args.color);

    let (command, envs, report_source) = match args.preset {
        Some(chosen) => {
            let path = match preset::report_path() {
                Ok(path) => path,
                Err(err) => {
                    eprintln!("sooth: failed to create a temp directory for the report: {err}");
                    return ExitCode::from(EXIT_SOOTH_ERROR);
                }
            };
            let (command, envs) = preset::inject(&args.command, chosen, &path);
            (command, envs, Some(path))
        }
        None => (args.command.clone(), Vec::new(), args.junit.clone()),
    };

    let run_started = std::time::SystemTime::now();
    let outcomes = match runner::run(&command, args.runs, &envs) {
        Ok(outcomes) => outcomes,
        Err(err) => {
            let program = &command[0];
            eprintln!("sooth: failed to run `{program}`: {err}");
            return ExitCode::from(EXIT_SOOTH_ERROR);
        }
    };

    let junit_summary = match &report_source {
        Some(path) => {
            // A --junit report that predates the run is the classic silent
            // failure: the runner wrote nothing (wrong flag, crash) and sooth
            // would report yesterday's truth. Presets skip this: their report
            // lives in a directory created fresh for this invocation.
            if args.preset.is_none() && report_is_stale(path, run_started) {
                eprintln!(
                    "sooth: the JUnit-XML report at `{}` was not touched by this run — it \
                     predates the test command. Is the runner writing its report to this path?",
                    path.display()
                );
                return ExitCode::from(EXIT_SOOTH_ERROR);
            }
            let preset_program = args.preset.map(|_| command[0].as_str());
            let loaded = load_summary(
                path,
                preset_program,
                args.slowest.unwrap_or(DEFAULT_SLOWEST),
            );
            if args.preset.is_some() {
                // Best-effort cleanup of the preset's private report dir —
                // the parse result is already in memory. A user's own --junit
                // file is never removed.
                cleanup_preset_report(path);
            }
            match loaded {
                Ok(summary) => Some(summary),
                Err(message) => {
                    eprintln!("sooth: {message}");
                    return ExitCode::from(EXIT_SOOTH_ERROR);
                }
            }
        }
        None => None,
    };

    let failed = suite_failed(&outcomes, junit_summary.as_ref());
    let report_failures = junit_summary
        .as_ref()
        .map_or(0, |summary| summary.failed + summary.error);
    let runs_failed = outcomes.iter().any(|outcome| !outcome.success);
    if report_failures > 0 && !runs_failed {
        eprintln!(
            "sooth: the runner exited 0 but the report shows {report_failures} failing \
             test(s) — exiting 1 (the runner and its report must agree for a 0)"
        );
    }

    if let Some(exit) = emit_output(args, &outcomes, junit_summary.as_ref(), failed, style) {
        return exit;
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Print the run's output in the shape the flags ask for. Returns an exit
/// code only when emitting itself failed (the JSON file could not be
/// written).
fn emit_output(
    args: &cli::RunArgs,
    outcomes: &[runner::RunOutcome],
    junit_summary: Option<&JunitSummary>,
    failed: bool,
    style: report::Style,
) -> Option<ExitCode> {
    match (junit_summary, &args.json) {
        // Bare --json: sooth's own stdout output is exactly one line of
        // JSON, printed after the wrapped command finished (last-line
        // contract — the child's output still shares the stream).
        (Some(summary), Some(None)) => println!("{}", report::to_json(outcomes, summary)),
        // --json=PATH: the machine output goes to a file, the human report
        // stays on stdout.
        (Some(summary), Some(Some(path))) => {
            report::print_runs(outcomes, style);
            report::print_summary(summary, style);
            let json = report::to_json(outcomes, summary);
            if let Err(err) = std::fs::write(path, json + "\n") {
                eprintln!(
                    "sooth: failed to write JSON report `{}`: {err}",
                    path.display()
                );
                return Some(ExitCode::from(EXIT_SOOTH_ERROR));
            }
            println!(
                "{}",
                report::verdict_line(outcomes, junit_summary, failed, style)
            );
        }
        (Some(summary), None) => {
            report::print_runs(outcomes, style);
            report::print_summary(summary, style);
            println!(
                "{}",
                report::verdict_line(outcomes, junit_summary, failed, style)
            );
        }
        (None, _) => {
            // Unreachable with json set: rejected_flag exits earlier when
            // --json has no report source. Assert that invariant locally so
            // a weakened guard fails loudly instead of dropping output.
            debug_assert!(args.json.is_none());
            report::print_runs(outcomes, style);
            println!("{}", report::verdict_line(outcomes, None, failed, style));
        }
    }
    None
}

/// A flag `sooth` cannot honor, if any. Rejecting loudly beats silently
/// ignoring — this tool's brand is the truth. `--json` and `--slowest` only
/// mean something once there is a report to summarize: `--junit` brings your
/// own, `--preset` has the runner write one.
fn rejected_flag(args: &cli::RunArgs) -> Option<&'static str> {
    if args.junit.is_none() && args.preset.is_none() {
        if args.json.is_some() {
            return Some("`--json` requires a report: `--junit <PATH>` or `--preset <RUNNER>`");
        }
        if args.slowest.is_some() {
            return Some("`--slowest` requires a report: `--junit <PATH>` or `--preset <RUNNER>`");
        }
    }
    None
}

/// Whether the suite failed, combining both signals: a nonzero runner exit
/// *or* failures/errors in the parsed report. A failure is never upgraded to
/// success — sooth exits 0 only when the runner and its report agree that
/// everything passed (see `DECISIONS.md`).
fn suite_failed(outcomes: &[runner::RunOutcome], summary: Option<&JunitSummary>) -> bool {
    outcomes.iter().any(|outcome| !outcome.success)
        || summary.is_some_and(|summary| summary.failed + summary.error > 0)
}

/// Load and summarize the JUnit-XML report at `path`. `preset_program` is the
/// wrapped program name when the report is preset-managed — used to turn "no
/// report was written" into an actionable message instead of a parse error
/// about a temp path the user never chose.
fn load_summary(
    path: &std::path::Path,
    preset_program: Option<&str>,
    slowest: usize,
) -> Result<JunitSummary, String> {
    if let Some(program) = preset_program {
        if !path.exists() {
            return Err(format!(
                "the test command wrote no JUnit-XML report — is the reporter available, \
                 and is `{program}` the test runner itself rather than a wrapper like \
                 `python -m`, `npm` or `php artisan`?"
            ));
        }
    }
    match junit::parse_file(path) {
        Ok(parsed) => Ok(JunitSummary::from_report(&parsed, slowest)),
        Err(err) => Err(format!(
            "failed to parse JUnit-XML report `{}`: {err}",
            path.display()
        )),
    }
}

/// Tolerance before a report counts as stale: wide enough to absorb coarse
/// filesystem timestamps *and* modest clock skew between sooth and a network
/// filesystem's server. The real failure mode this guards against — the
/// runner wrote nothing and the file is from an earlier run — is minutes to
/// days old, so generosity here costs almost nothing while a false "stale"
/// on a fresh report would be its own lie.
const STALE_TOLERANCE: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the report predates this run. A missing file or a filesystem
/// without mtimes skips the check (the parse step reports missing files with
/// its own, better message).
fn report_is_stale(path: &std::path::Path, run_started: std::time::SystemTime) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    match run_started.duration_since(modified) {
        Ok(age_before_start) => age_before_start > STALE_TOLERANCE,
        // Modified after the run started: fresh.
        Err(_) => false,
    }
}

/// Best-effort removal of a preset's private report directory (file + dir).
fn cleanup_preset_report(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::{rejected_flag, suite_failed};
    use crate::cli::{Cli, Command};
    use crate::junit::{JunitReport, TestCase, TestStatus};
    use crate::report::JunitSummary;
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

    fn test_case(name: &str, status: TestStatus, duration_seconds: f64) -> TestCase {
        TestCase {
            name: name.to_owned(),
            classname: None,
            duration: Duration::from_secs_f64(duration_seconds),
            status,
        }
    }

    fn summary_of(status: TestStatus) -> JunitSummary {
        let report = JunitReport {
            test_cases: vec![test_case("case", status, 0.1)],
        };
        JunitSummary::from_report(&report, 10)
    }

    #[test]
    fn the_suite_fails_when_the_report_shows_failures_even_if_the_runner_exited_zero() {
        let summary = summary_of(TestStatus::Failed);
        assert!(suite_failed(&[outcome(true)], Some(&summary)));
    }

    #[test]
    fn an_erroring_test_in_the_report_also_fails_the_suite() {
        let summary = summary_of(TestStatus::Error);
        assert!(suite_failed(&[outcome(true)], Some(&summary)));
    }

    #[test]
    fn the_suite_fails_on_a_nonzero_runner_even_with_a_clean_report() {
        let summary = summary_of(TestStatus::Passed);
        assert!(suite_failed(&[outcome(false)], Some(&summary)));
    }

    #[test]
    fn the_suite_passes_when_runner_and_report_agree() {
        let summary = summary_of(TestStatus::Skipped);
        assert!(!suite_failed(&[outcome(true)], Some(&summary)));
        assert!(!suite_failed(&[outcome(true)], None));
    }

    #[test]
    fn a_preset_run_that_writes_no_report_gets_an_actionable_error() {
        let missing = std::env::temp_dir().join("sooth-test-no-such-report.xml");
        let message = super::load_summary(&missing, Some("pytest"), 10)
            .err()
            .expect("a missing preset report should error");
        assert!(message.contains("wrote no JUnit-XML report"));
        assert!(message.contains("pytest"));
    }

    #[test]
    fn a_missing_user_junit_file_reports_the_path() {
        let missing = std::env::temp_dir().join("sooth-test-no-such-report.xml");
        let message = super::load_summary(&missing, None, 10)
            .err()
            .expect("a missing --junit file should error");
        assert!(message.contains("failed to parse"));
        assert!(message.contains("sooth-test-no-such-report.xml"));
    }

    #[test]
    fn a_report_older_than_the_run_start_is_stale() {
        let path =
            std::env::temp_dir().join(format!("sooth-stale-test-{}.xml", std::process::id()));
        std::fs::write(&path, "x").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        // Pin the tolerance boundary itself, not just far-away cases: just
        // inside stays fresh, just past it turns stale.
        let just_inside =
            modified + super::STALE_TOLERANCE.saturating_sub(std::time::Duration::from_secs(5));
        assert!(!super::report_is_stale(&path, just_inside));
        let just_past = modified + super::STALE_TOLERANCE + std::time::Duration::from_secs(5);
        assert!(super::report_is_stale(&path, just_past));
        let before_write = modified - std::time::Duration::from_secs(60);
        assert!(!super::report_is_stale(&path, before_write));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_report_is_not_stale() {
        // Missing files take the parse-error path, which has a better message.
        let missing = std::env::temp_dir().join("sooth-stale-test-missing.xml");
        assert!(!super::report_is_stale(
            &missing,
            std::time::SystemTime::now()
        ));
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
    fn json_and_slowest_are_accepted_with_a_preset() {
        let args = parse_run_args(&[
            "sooth",
            "run",
            "--preset",
            "pytest",
            "--json",
            "--slowest",
            "3",
            "--",
            "pytest",
        ]);
        assert_eq!(rejected_flag(&args), None);
    }

    #[test]
    fn reportless_json_and_slowest_are_rejected() {
        for (cmdline, fragment) in [
            (vec!["sooth", "run", "--json", "--", "true"], "--json"),
            (
                vec!["sooth", "run", "--json=out.json", "--", "true"],
                "--json",
            ),
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
