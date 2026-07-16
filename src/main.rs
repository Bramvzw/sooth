//! `sooth` — the truth about your tests.
//!
//! Exit codes distinguish whose fault a failure is (grep-style):
//! `0` — every run passed; `1` — at least one run failed (nonzero runner
//! exit *or* failures in the parsed report — both must agree for a `0`);
//! `2` — sooth itself failed (spawn error, unparsable report, bad flags).

mod analyzers;
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
            (command, envs, Some(ReportSource::Preset(path)))
        }
        None => (
            args.command.clone(),
            Vec::new(),
            args.junit.clone().map(ReportSource::User),
        ),
    };

    // Repetition lives here, not in the runner: the report is parsed per
    // run, and per-test outcomes across runs feed the flaky pass. Fixed
    // order on purpose — see DECISIONS.md.
    let mut outcomes = Vec::with_capacity(args.runs as usize);
    let mut reports: Vec<junit::JunitReport> = Vec::with_capacity(args.runs as usize);
    for _ in 0..args.runs {
        let report_before = match &report_source {
            // Delete the preset's report before every run: a runner that
            // stops writing must fail loudly instead of silently re-serving
            // the previous run's truth.
            Some(ReportSource::Preset(path)) => {
                let _ = std::fs::remove_file(path);
                None
            }
            // A user's file is never deleted; remember its state instead so
            // "did this run touch it" is a fact, not a clock comparison.
            Some(ReportSource::User(path)) => file_state(path),
            None => None,
        };
        match runner::run_once(&command, &envs) {
            Ok(outcome) => outcomes.push(outcome),
            Err(err) => {
                let program = &command[0];
                eprintln!("sooth: failed to run `{program}`: {err}");
                if let Some(ReportSource::Preset(path)) = &report_source {
                    cleanup_preset_report(path);
                }
                return ExitCode::from(EXIT_SOOTH_ERROR);
            }
        }
        if let Some(source) = &report_source {
            match load_run_report(source, &command[0], report_before, &outcomes) {
                Ok(report) => reports.push(report),
                Err(exit) => return exit,
            }
        }
    }
    if let Some(ReportSource::Preset(path)) = &report_source {
        // Best-effort cleanup of the preset's private report dir — every
        // run's parse result is already in memory. A user's own --junit file
        // is never removed.
        cleanup_preset_report(path);
    }

    let junit_summary = reports
        .last()
        .map(|report| JunitSummary::from_report(report, args.slowest.unwrap_or(DEFAULT_SLOWEST)));
    let flaky_analysis = if args.runs > 1 && !reports.is_empty() {
        Some(analyzers::flaky::analyze(&reports))
    } else {
        None
    };

    let failed = suite_failed(&outcomes, &reports);
    // The worst run's count, over all reports: the mismatch note and the
    // verdict must not claim "0 failing" because the *last* run was green.
    let report_failures = reports
        .iter()
        .map(junit::JunitReport::failing_count)
        .max()
        .unwrap_or(0);
    let runs_failed = outcomes.iter().any(|outcome| !outcome.success);
    if report_failures > 0 && !runs_failed {
        eprintln!(
            "sooth: the runner exited 0 but the report shows {report_failures} failing \
             test(s) — exiting 1 (the runner and its report must agree for a 0)"
        );
    }

    if let Some(exit) = emit_output(
        args,
        &outcomes,
        junit_summary.as_ref(),
        flaky_analysis.as_ref(),
        report_failures,
        failed,
        style,
    ) {
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
    flaky: Option<&analyzers::flaky::Analysis>,
    report_failures: usize,
    failed: bool,
    style: report::Style,
) -> Option<ExitCode> {
    match (junit_summary, &args.json) {
        // Bare --json: sooth's own stdout output is exactly one line of
        // JSON, printed after the wrapped command finished (last-line
        // contract — the child's output still shares the stream).
        (Some(summary), Some(None)) => println!("{}", report::to_json(outcomes, summary, flaky)),
        // --json=PATH: the machine output goes to a file, the human report
        // stays on stdout.
        (Some(summary), Some(Some(path))) => {
            report::print_runs(outcomes, style);
            report::print_summary(summary, style);
            report::print_flaky(flaky, style);
            let json = report::to_json(outcomes, summary, flaky);
            if let Err(err) = std::fs::write(path, json + "\n") {
                eprintln!(
                    "sooth: failed to write JSON report `{}`: {err}",
                    path.display()
                );
                return Some(ExitCode::from(EXIT_SOOTH_ERROR));
            }
            println!(
                "{}",
                report::verdict_line(outcomes, junit_summary, report_failures, failed, style)
            );
        }
        (Some(summary), None) => {
            report::print_runs(outcomes, style);
            report::print_summary(summary, style);
            report::print_flaky(flaky, style);
            println!(
                "{}",
                report::verdict_line(outcomes, junit_summary, report_failures, failed, style)
            );
        }
        (None, _) => {
            // Unreachable with json set: rejected_flag exits earlier when
            // --json has no report source. Assert that invariant locally so
            // a weakened guard fails loudly instead of dropping output.
            debug_assert!(args.json.is_none());
            report::print_runs(outcomes, style);
            println!(
                "{}",
                report::verdict_line(outcomes, None, report_failures, failed, style)
            );
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
fn suite_failed(outcomes: &[runner::RunOutcome], reports: &[junit::JunitReport]) -> bool {
    outcomes.iter().any(|outcome| !outcome.success)
        || reports.iter().any(junit::JunitReport::has_failures)
}

/// Where the report comes from — the user's own file (`--junit`, freshness
/// checked, never deleted) or a preset-managed temp file (fresh by
/// construction, cleaned up afterwards). One value carries what three
/// separate `args.preset` checks used to coordinate.
enum ReportSource {
    User(std::path::PathBuf),
    Preset(std::path::PathBuf),
}

impl ReportSource {
    fn path(&self) -> &std::path::Path {
        match self {
            Self::User(path) | Self::Preset(path) => path,
        }
    }
}

/// One run's report: freshness-checked (user files only), loaded, and — on
/// the error path — annotated with crash context and cleaned up (presets).
fn load_run_report(
    source: &ReportSource,
    program: &str,
    report_before: Option<(std::time::SystemTime, u64)>,
    outcomes: &[runner::RunOutcome],
) -> Result<junit::JunitReport, ExitCode> {
    if let ReportSource::User(path) = source {
        if report_before.is_some() && file_state(path) == report_before {
            eprintln!(
                "sooth: the JUnit-XML report at `{}` was not touched by this run — it \
                 predates the test command. Is the runner writing its report to this path?",
                path.display()
            );
            return Err(ExitCode::from(EXIT_SOOTH_ERROR));
        }
    }
    let preset_program = match source {
        ReportSource::Preset(_) => Some(program),
        ReportSource::User(_) => None,
    };
    load_report(source.path(), preset_program).map_err(|message| {
        eprintln!("sooth: {message}");
        if let Some(context) = crash_context(outcomes) {
            eprintln!("sooth: {context}");
        }
        if let ReportSource::Preset(path) = source {
            cleanup_preset_report(path);
        }
        ExitCode::from(EXIT_SOOTH_ERROR)
    })
}

/// Load the JUnit-XML report at `path`. `preset_program` is the wrapped
/// program name when the report is preset-managed — used to turn "no report
/// was written" into an actionable message instead of a parse error about a
/// temp path the user never chose.
fn load_report(
    path: &std::path::Path,
    preset_program: Option<&str>,
) -> Result<junit::JunitReport, String> {
    if let Some(program) = preset_program {
        if !path.exists() {
            return Err(format!(
                "the test command wrote no JUnit-XML report — is the reporter available, \
                 and is `{program}` the test runner itself rather than a wrapper like \
                 `python -m`, `npm` or `php artisan`?"
            ));
        }
    }
    junit::parse_file(path).map_err(|err| {
        format!(
            "failed to parse JUnit-XML report `{}`: {err}",
            path.display()
        )
    })
}

/// A file's identity-for-freshness: mtime plus size. Comparing states
/// before and after a run answers "did this run touch the report" as an
/// observed fact — no wall clock, no tolerance window, immune to clock skew
/// between sooth and a network filesystem. `None` when the file is missing
/// or the filesystem has no mtimes (the parse step owns the missing-file
/// message).
fn file_state(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// When the report is unusable and the runner itself failed, keep the run
/// facts sooth already measured instead of discarding them with the report:
/// after a long run, "the crash output above is the real story" beats a bare
/// parse error about a temp path. `None` when every run succeeded.
fn crash_context(outcomes: &[runner::RunOutcome]) -> Option<String> {
    let (index, outcome) = outcomes
        .iter()
        .enumerate()
        .rev()
        .find(|(_, outcome)| !outcome.success)?;
    Some(format!(
        "run {} of {} failed ({}, {:.2?}) — the runner's own output above likely \
         explains the unusable report",
        index + 1,
        outcomes.len(),
        outcome.status_label(),
        outcome.duration
    ))
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

    fn report_of(status: TestStatus) -> JunitReport {
        JunitReport {
            test_cases: vec![test_case("case", status, 0.1)],
        }
    }

    #[test]
    fn the_suite_fails_when_the_report_shows_failures_even_if_the_runner_exited_zero() {
        assert!(suite_failed(
            &[outcome(true)],
            &[report_of(TestStatus::Failed)]
        ));
    }

    #[test]
    fn an_erroring_test_in_the_report_also_fails_the_suite() {
        assert!(suite_failed(
            &[outcome(true)],
            &[report_of(TestStatus::Error)]
        ));
    }

    #[test]
    fn any_runs_report_failing_fails_the_suite_not_just_the_last() {
        // With --runs N every run's report counts: a failure in run 1 is not
        // forgiven by a green run 2.
        let reports = [report_of(TestStatus::Failed), report_of(TestStatus::Passed)];
        assert!(suite_failed(&[outcome(true), outcome(true)], &reports));
    }

    #[test]
    fn the_suite_fails_on_a_nonzero_runner_even_with_a_clean_report() {
        assert!(suite_failed(
            &[outcome(false)],
            &[report_of(TestStatus::Passed)]
        ));
    }

    #[test]
    fn the_suite_passes_when_runner_and_report_agree() {
        assert!(!suite_failed(
            &[outcome(true)],
            &[report_of(TestStatus::Skipped)]
        ));
        assert!(!suite_failed(&[outcome(true)], &[]));
    }

    #[test]
    fn a_preset_run_that_writes_no_report_gets_an_actionable_error() {
        let missing = std::env::temp_dir().join("sooth-test-no-such-report.xml");
        let message = super::load_report(&missing, Some("pytest"))
            .expect_err("a missing preset report should error");
        assert!(message.contains("wrote no JUnit-XML report"));
        assert!(message.contains("pytest"));
    }

    #[test]
    fn a_missing_user_junit_file_reports_the_path() {
        let missing = std::env::temp_dir().join("sooth-test-no-such-report.xml");
        let message =
            super::load_report(&missing, None).expect_err("a missing --junit file should error");
        assert!(message.contains("failed to parse"));
        assert!(message.contains("sooth-test-no-such-report.xml"));
    }

    #[test]
    fn crash_context_names_the_failed_run_and_its_status() {
        let outcomes = [outcome(true), outcome(false)];
        let context = super::crash_context(&outcomes).expect("a failed run should give context");
        assert!(context.contains("run 2 of 2"));
        assert!(context.contains("runner exit=1"));
        assert!(context.contains("output above"));
    }

    #[test]
    fn crash_context_is_silent_when_every_run_succeeded() {
        assert_eq!(super::crash_context(&[outcome(true)]), None);
    }

    #[test]
    fn file_state_changes_when_the_file_is_rewritten() {
        let path =
            std::env::temp_dir().join(format!("sooth-state-test-{}.xml", std::process::id()));
        std::fs::write(&path, "one").unwrap();
        let before = super::file_state(&path);
        assert!(before.is_some());
        // Same state when untouched — the "runner wrote nothing" signal.
        assert_eq!(super::file_state(&path), before);
        std::fs::write(&path, "two-longer").unwrap();
        assert_ne!(super::file_state(&path), before);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_has_no_state() {
        // Missing files take the parse-error path, which has a better message.
        let missing = std::env::temp_dir().join("sooth-state-test-missing.xml");
        assert_eq!(super::file_state(&missing), None);
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
