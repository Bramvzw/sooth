//! `sooth` — the truth about your tests.
//!
//! Exit codes distinguish whose fault a failure is (grep-style):
//! `0` — every run passed; `1` — at least one run failed (nonzero runner
//! exit *or* failures in the parsed report — both must agree for a `0`);
//! `2` — sooth itself failed (spawn error, unparsable report, bad flags).

mod analyzers;
mod cli;
mod gate;
mod history;
mod interrupt;
mod json;
mod junit;
mod phpunit_log;
mod preset;
mod quarantine;
mod report;
mod reported;
mod runner;
mod verify;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::report::{Analyses, JunitSummary};
use crate::reported::{file_state, ReportError, ReportSource};

/// Exit code for "sooth itself failed", as opposed to "the tests failed" (1).
const EXIT_SOOTH_ERROR: u8 = 2;

/// How many of the slowest tests the summary shows when `--slowest` is not
/// given.
const DEFAULT_SLOWEST: usize = 10;

/// Sooth could not do what it was asked — not the tests failing, sooth
/// itself. The lines say why, in the words of the place that knows; `main`
/// prints each as `sooth: …` on stderr and exits 2. Every command returns
/// this instead of exiting, so the exit-code contract is decided in one
/// place and whatever a command holds (a temp report, a capture directory)
/// is dropped, and so cleaned up, on the way out.
#[derive(Debug)]
struct SoothError {
    lines: Vec<String>,
}

impl SoothError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            lines: vec![message.into()],
        }
    }
}

/// What a command comes back with: the suite's exit (0 or 1), or why sooth
/// could not get that far.
type Outcome = Result<ExitCode, SoothError>;

fn main() -> ExitCode {
    // A panic is sooth failing, never the tests: exit 2, not Rust's 101
    // (see DECISIONS.md, exit codes).
    std::panic::set_hook(Box::new(report::internal_error));
    std::panic::catch_unwind(dispatch).unwrap_or(ExitCode::from(EXIT_SOOTH_ERROR))
}

/// Parse the command line, run the command, and turn its outcome into the
/// process exit — the one place exit 2 is decided for a command that ran.
#[expect(clippy::print_stderr)]
fn dispatch() -> ExitCode {
    // No input is known to make sooth panic, and what a panic must look
    // like (exit 2, said as a bug) is tested against the binary: a debug
    // build panics on request. A release build has no such switch.
    #[cfg(debug_assertions)]
    assert!(
        std::env::var_os("SOOTH_TEST_PANIC").is_none(),
        "asked to panic by SOOTH_TEST_PANIC"
    );
    let parsed = match Cli::try_parse() {
        Ok(parsed) => parsed,
        // clap prints and exits: 2 for a usage error, 0 for --help.
        Err(error) => {
            // Lossily: `args()` panics on an argument that is not UTF-8,
            // and the tip only compares words clap itself could read.
            let typed: Vec<String> = std::env::args_os()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();
            cli::with_tip(error, &typed).exit()
        }
    };
    let outcome = match parsed.command {
        Command::Run(mut args) => {
            infer_preset(&mut args);
            run(&args)
        }
        Command::Explain(args) => explain(&args),
        Command::Import(args) => import(&args),
        Command::History(args) => history_command(&args),
    };
    outcome.unwrap_or_else(|error| {
        for line in &error.lines {
            eprintln!("sooth: {line}");
        }
        ExitCode::from(EXIT_SOOTH_ERROR)
    })
}

/// Without `--preset` or `--junit`, take the preset the command itself
/// names — and say so, because the report that follows depends on it. An
/// explicit flag is never second-guessed.
#[expect(clippy::print_stderr)]
fn infer_preset(args: &mut cli::RunArgs) {
    if args.preset.is_some() || args.junit.is_some() {
        return;
    }
    if let Some((preset, words)) = preset::infer(&args.command) {
        eprintln!(
            "sooth: using `--preset {}`, inferred from `{words}`",
            preset.name()
        );
        args.preset = Some(preset);
    }
}

/// The closing line of a run that had no report to read: what it could not
/// see, and the flag that changes that.
#[expect(clippy::print_stderr)]
fn print_report_advice(command: &[String]) {
    eprintln!(
        "sooth: only the exit code was read — for per-test results, flaky detection and \
         history, {}",
        preset::report_advice(command)
    );
}

/// Handle `sooth run`: execute the test command, then — when a report source
/// is given — parse the JUnit-XML report and extend the output with totals and
/// the slowest tests. `--junit` points at a report the command already writes;
/// `--preset` injects the right reporter flags and manages a temp report
/// itself. Without either, the output is the plain per-run report.
fn run(args: &cli::RunArgs) -> Outcome {
    if let Some(reason) = rejected_flag(args) {
        return Err(SoothError::new(reason));
    }
    let style = report::Style::resolved(args.color);

    let plan = args
        .preset
        .map(|preset| preset::Plan::resolve(&args.command, preset));
    let selection = match gate_selection(args, plan.as_ref(), style)? {
        Gate::Off => None,
        Gate::Files(selection) => Some(selection),
        Gate::Nothing => return Ok(ExitCode::SUCCESS),
    };
    let files = selection.as_ref().map_or(&[][..], |gated| &gated.files);
    let ((command, envs), report_source) = prepared_command(args, plan.as_ref(), files)?;

    let identity = identity_before_run(args, report_source.as_ref());

    // From here a Ctrl-C or a SIGTERM is a request, not the end: the runs
    // that finished are still reported, and nothing is left behind.
    let interrupt = interrupt::Interrupt::listen();
    let session = Session {
        spawn: &(command, envs),
        report_source: report_source.as_ref(),
        plan: plan.as_ref(),
        gated_files: files.len(),
        interrupt: &interrupt,
    };
    let Executed {
        outcomes,
        reports,
        stopped,
    } = execute_runs(args, &session, style)?;
    let mut interrupted = stopped.then(|| report::Interrupted::Runs {
        requested: args.runs as usize,
        failed: failed_runs(&outcomes, &reports),
    });
    if stopped && outcomes.is_empty() {
        return Err(nothing_finished(args.runs));
    }

    let (junit_summary, flaky_analysis) = summarize_reports(args, &reports);
    let history_pass = record_history(identity, &reports);
    let history_analysis = history_pass.as_ref().map(|pass| &pass.analysis);
    // Asked to stop: no further runs are started, verification included.
    let verify_verdict = if interrupted.is_some() {
        None
    } else {
        verify_failures(args, plan.as_ref(), reports.last(), &interrupt).unwrap_or_else(|stop| {
            interrupted = Some(stop);
            None
        })
    };

    let suite_red = suite_failed(&outcomes, &reports);
    // The list labels known flakes on any red run; only --fail-on-flaky
    // lets it steer the exit.
    let quarantine = if suite_red {
        load_quarantine()
    } else {
        std::collections::BTreeSet::new()
    };
    // Built once: the passes that explain a failure are the ones the report
    // prints, and the explanation is added to them when it exists.
    let mut analyses = Analyses {
        flaky: flaky_analysis.as_ref(),
        history: history_analysis,
        history_observations: history_pass.as_ref().map(|pass| pass.prior),
        verify: verify_verdict.as_ref(),
        gate: selection.as_ref(),
        interrupted,
        ..Analyses::default()
    };
    let explanation = explain_failures(suite_red, &reports, analyses.passes(), &quarantine);
    let pardoned = pardoned_failures(args, suite_red, &quarantine, &outcomes, &reports);
    let failed = suite_red && pardoned.is_none();
    analyses.explanation = explanation.as_deref();
    analyses.pardoned = pardoned.as_deref();
    // The worst run's count, over all reports: the mismatch note and the
    // verdict must not claim "0 failing" because the *last* run was green.
    let report_failures = reports
        .iter()
        .map(junit::JunitReport::failing_count)
        .max()
        .unwrap_or(0);
    let runs_failed = outcomes.iter().any(|outcome| !outcome.success);
    // A pardoned run exits 0 and an interrupted one 2; the note's "exiting
    // 1" would then be a lie.
    if failed && interrupted.is_none() {
        print_disagreement(runs_failed, report_failures, !reports.is_empty());
    }

    emit_output(
        args,
        &outcomes,
        junit_summary.as_ref(),
        &analyses,
        report_failures,
        failed,
        style,
    )?;

    // Not 0 and not 1: sooth did not do what it was asked. The report above
    // is what it has.
    if let Some(stop) = interrupted {
        return Err(stopped_short(stop, outcomes.len()));
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Asked to stop before a single run had finished: there is no report, so
/// this line is all sooth says.
fn nothing_finished(requested_runs: u32) -> SoothError {
    SoothError::new(if requested_runs == 1 {
        "interrupted before the run finished — there is nothing to report".to_owned()
    } else {
        format!(
            "interrupted before the first of {requested_runs} runs finished — there is \
             nothing to report"
        )
    })
}

/// Asked to stop with a report to show for it: what that report covers.
fn stopped_short(stop: report::Interrupted, finished_runs: usize) -> SoothError {
    SoothError::new(match stop {
        report::Interrupted::Runs { requested, .. } => format!(
            "interrupted after {finished_runs} of {} — the report covers the runs that finished",
            report::count(requested, "run")
        ),
        report::Interrupted::Verification => {
            "interrupted before the failures were verified — the report covers the run, \
             and its failures stay unverified"
                .to_owned()
        }
    })
}

/// What every run of one invocation shares: the command as it is spawned,
/// where its report comes from, and what the gate handed it.
struct Session<'a> {
    spawn: &'a preset::Spawn,
    report_source: Option<&'a ReportSource>,
    plan: Option<&'a preset::Plan>,
    gated_files: usize,
    interrupt: &'a interrupt::Interrupt,
}

/// What the runs loop came back with: the runs that finished, and whether
/// sooth was asked to stop before every run had been made.
struct Executed {
    outcomes: Vec<runner::RunOutcome>,
    reports: Vec<junit::JunitReport>,
    stopped: bool,
}

/// Whether the runner is kept quiet: N copies of the same output bury the
/// verdict, so repeated runs print one line each and the output of the
/// first that failed (see `DECISIONS.md`). One run shows its tests as if
/// you had run them yourself.
fn quiet_repeats(args: &cli::RunArgs) -> bool {
    args.runs > 1 && !args.show_output
}

/// The output of quiet runs, kept off the screen: every run writes to one
/// file in a private directory, and the first failing run's is set aside.
/// Dropping it removes the directory, on every way out of the runs loop.
struct Capture {
    dir: std::path::PathBuf,
    /// The first run that failed, whether or not its output could be kept.
    first_failure: Option<usize>,
    kept: bool,
}

impl Capture {
    fn start() -> std::io::Result<Self> {
        Ok(Self {
            dir: preset::private_dir()?,
            first_failure: None,
            kept: false,
        })
    }

    fn current_path(&self) -> std::path::PathBuf {
        self.dir.join("run.log")
    }

    fn kept_path(&self) -> std::path::PathBuf {
        self.dir.join("first-failure.log")
    }

    /// An empty file for the run about to start.
    fn open(&self) -> std::io::Result<std::fs::File> {
        std::fs::File::create(self.current_path())
    }

    /// What the run that just ended wrote.
    fn current(&self) -> Vec<u8> {
        std::fs::read(self.current_path()).unwrap_or_default()
    }

    /// Run `number` failed: set its output aside if it is the first.
    fn note_failure(&mut self, number: usize) {
        if self.first_failure.is_none() {
            self.first_failure = Some(number);
            self.kept = std::fs::rename(self.current_path(), self.kept_path()).is_ok();
        }
    }

    /// Show the first failing run's output, if one failed and it was kept.
    fn replay_first_failure(&self, total: usize, to_stderr: bool, style: report::Style) {
        let Some(number) = self.first_failure.filter(|_| self.kept) else {
            return;
        };
        let replay = report::Replay {
            number,
            total,
            why: "the first that failed",
            output: &std::fs::read(self.kept_path()).unwrap_or_default(),
        };
        report::print_replay(&replay, to_stderr, style);
    }

    /// Show the output of run `number`, the one sooth stops at: it is what
    /// the error that follows points to. An earlier failure goes first.
    fn replay_before_stopping(
        &self,
        (number, total): (usize, usize),
        to_stderr: bool,
        style: report::Style,
    ) {
        self.replay_first_failure(total, to_stderr, style);
        let replay = report::Replay {
            number,
            total,
            why: "where sooth stopped",
            output: &self.current(),
        };
        report::print_replay(&replay, to_stderr, style);
    }

    /// Show what run `number` had written when it was cut short. For a run
    /// that hung, this is the only trace of where: the run itself gets no
    /// line and no place in the report.
    fn replay_cut_short(
        &self,
        (number, total): (usize, usize),
        to_stderr: bool,
        style: report::Style,
    ) {
        let output = self.current();
        if output.is_empty() {
            return;
        }
        let replay = report::Replay {
            number,
            total,
            why: "the one that was cut short",
            output: &output,
        };
        report::print_replay(&replay, to_stderr, style);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.current_path());
        let _ = std::fs::remove_file(self.kept_path());
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Run the command `--runs` times and load each run's report.
///
/// Repetition lives here, not in the runner: the report is parsed per run,
/// and per-test outcomes across runs feed the flaky pass. Fixed order on
/// purpose — see DECISIONS.md.
fn execute_runs(
    args: &cli::RunArgs,
    session: &Session<'_>,
    style: report::Style,
) -> Result<Executed, SoothError> {
    let total = args.runs as usize;
    let (command, _) = session.spawn;
    let report_source = session.report_source;
    let mut capture = if quiet_repeats(args) {
        Some(Capture::start().map_err(|err| {
            SoothError::new(format!(
                "failed to create a temp directory for the runs' output: {err}"
            ))
        })?)
    } else {
        None
    };
    // Bare --json owns sooth's stdout: the runs ride in the document, and
    // what a quiet run wrote is replayed on stderr.
    let json_owns_stdout = matches!(args.json, Some(None));
    let mut outcomes = Vec::with_capacity(total);
    let mut reports: Vec<junit::JunitReport> = Vec::with_capacity(total);
    let mut stopped = false;
    let mut cut_short = None;
    for number in 1..=total {
        // Asked to stop between two runs: the next one is not started.
        if session.interrupt.requested() {
            stopped = true;
            break;
        }
        let ran = capture
            .as_ref()
            .map(Capture::open)
            .transpose()
            .map_err(|err| format!("failed to create a file for the run's output: {err}"))
            .and_then(|hidden| {
                reported::run(session.spawn, report_source, hidden, session.interrupt)
                    .map_err(|err| format!("failed to run `{}`: {err}", command[0]))
            });
        let ran = match ran {
            Ok(reported::Run::Finished(ran)) => ran,
            // A run that was cut short is not a run that failed: whatever
            // it exited with and wrote, it does not count.
            Ok(reported::Run::CutShort) => {
                stopped = true;
                cut_short = Some(number);
                break;
            }
            Err(message) => {
                // An earlier run's failure is not lost with this one.
                if let Some(capture) = &capture {
                    capture.replay_first_failure(total, json_owns_stdout, style);
                }
                return Err(SoothError::new(message));
            }
        };
        outcomes.push(ran.outcome);
        let outcome = &outcomes[number - 1];
        match ran.report {
            Some(Ok(report)) => reports.push(report),
            Some(Err(error)) => {
                if let Some(capture) = &capture {
                    report::print_run(number, total, outcome, style);
                    capture.replay_before_stopping((number, total), json_owns_stdout, style);
                }
                return Err(SoothError {
                    lines: unusable_report(session, &error, (&outcomes, args.runs)),
                });
            }
            None => {}
        }
        let failed = run_failed(outcome, reports.get(number - 1));
        if let Some(capture) = capture.as_mut() {
            if failed {
                capture.note_failure(number);
            }
            if !json_owns_stdout {
                report::print_run(number, total, outcome, style);
            }
        }
    }
    if let Some(capture) = &capture {
        capture.replay_first_failure(total, json_owns_stdout, style);
        if let Some(number) = cut_short {
            capture.replay_cut_short((number, total), json_owns_stdout, style);
        }
    }
    Ok(Executed {
        outcomes,
        reports,
        stopped,
    })
}

/// The command to spawn, its environment, and where its report comes from:
/// the preset injects flags and manages a temp report, `--junit` points at
/// the user's own file. `files` is the gate's selection, empty without one.
fn prepared_command(
    args: &cli::RunArgs,
    plan: Option<&preset::Plan>,
    files: &[String],
) -> Result<(preset::Spawn, Option<ReportSource>), SoothError> {
    match plan {
        Some(plan) => {
            let source = ReportSource::temp().map_err(|err| {
                SoothError::new(format!(
                    "failed to create a temp directory for the report: {err}"
                ))
            })?;
            let spawn = plan.run(source.path(), files);
            Ok((spawn, Some(source)))
        }
        None => Ok((
            (args.command.clone(), Vec::new()),
            args.junit.clone().map(ReportSource::User),
        )),
    }
}

/// The last run's totals, and the active flaky pass when several runs
/// produced reports to compare.
fn summarize_reports(
    args: &cli::RunArgs,
    reports: &[junit::JunitReport],
) -> (Option<JunitSummary>, Option<analyzers::flaky::Analysis>) {
    let summary = reports
        .last()
        .map(|report| JunitSummary::from_report(report, args.slowest.unwrap_or(DEFAULT_SLOWEST)));
    let flaky = if args.runs > 1 && !reports.is_empty() {
        Some(analyzers::flaky::analyze(reports))
    } else {
        None
    };
    (summary, flaky)
}

/// What `--changed` made of this run.
enum Gate {
    /// No `--changed`: the command runs as given.
    Off,
    /// The happy fast path: nothing changed, nothing to prove, nothing
    /// spawned — its output is already emitted.
    Nothing,
    /// The test files that run, and nothing else.
    Files(gate::Selection),
}

/// Resolve `--changed` into the files to run, refusing what the runner
/// cannot be handed.
#[expect(clippy::print_stderr)]
fn gate_selection(
    args: &cli::RunArgs,
    plan: Option<&preset::Plan>,
    style: report::Style,
) -> Result<Gate, SoothError> {
    let Some(base) = &args.changed else {
        return Ok(Gate::Off);
    };
    let plan = plan.expect("rejected_flag guarantees a preset");
    let preset = plan.preset();
    // Before the selection is even resolved: a hook that works until the
    // day two test files change fails at the worst moment.
    if let Some(reason) = plan.gate_refusal() {
        return Err(SoothError::new(reason));
    }
    if let Some(selection) = plan.own_selection(std::path::Path::new(".")) {
        return Err(SoothError::new(format!(
            "the gate's command already selects `{selection}` — the runner would \
             run that and the changed test files, `--runs` times over, while the gate \
             names only the changed ones. Give the gate the bare runner: it adds the \
             files itself"
        )));
    }
    let selection = gate::resolve(base.as_deref(), preset, std::path::Path::new("."))
        .map_err(SoothError::new)?;
    if selection.files.is_empty() {
        return empty_gate(args, &selection).map(|()| Gate::Nothing);
    }
    // PHPUnit before 10 runs only the first positional path: a multi-file
    // selection would gate one file while claiming them all (#139). Probed
    // only when that can happen; too old refuses, unknown warns.
    if preset == cli::Preset::Phpunit && selection.files.len() > 1 {
        match plan.phpunit_major() {
            Some(major) if major < 10 => {
                return Err(SoothError::new(format!(
                    "the gate selected {} test files, but PHPUnit {major} runs only \
                     the first path it is given — the rest would silently not be gated. \
                     Upgrade to PHPUnit 10+ to gate several files at once",
                    selection.files.len()
                )));
            }
            None => eprintln!(
                "sooth: could not read a PHPUnit version from `{} --version` — if it is \
                 older than 10, only the first of the {} selected files will run",
                plan.version_probe().display(),
                selection.files.len()
            ),
            Some(_) => {}
        }
    }
    // Bare --json owns sooth's own stdout (one line of JSON); the selection
    // rides in the JSON's `gate` object instead.
    if !matches!(args.json, Some(None)) {
        report::print_gate(&selection, style);
    }
    Ok(Gate::Files(selection))
}

/// The empty gate's output, honoring every output mode's contract: bare
/// `--json` emits the document (zero runs), `--json=PATH` writes it and
/// keeps the human line on stdout.
#[expect(clippy::print_stdout)]
fn empty_gate(args: &cli::RunArgs, selection: &gate::Selection) -> Result<(), SoothError> {
    let analyses = Analyses {
        gate: Some(selection),
        ..Analyses::default()
    };
    let human = format!(
        "gate: no new or changed tests against {} — nothing to prove",
        selection.base
    );
    match &args.json {
        None => println!("{human}"),
        Some(None) => println!(
            "{}",
            report::to_json(&[], &JunitSummary::empty(), &analyses)
        ),
        Some(Some(path)) => {
            println!("{human}");
            write_json_file(
                path,
                report::to_json(&[], &JunitSummary::empty(), &analyses),
            )?;
        }
    }
    Ok(())
}

/// Write the JSON document to `path`; a failure is sooth's own (exit 2).
fn write_json_file(path: &std::path::Path, json: String) -> Result<(), SoothError> {
    std::fs::write(path, json + "\n").map_err(|err| {
        SoothError::new(format!(
            "failed to write JSON report `{}`: {err}",
            path.display()
        ))
    })
}

/// The failures pardoned by the quarantine file: `Some` — and the run exits
/// 0 — only when `--fail-on-flaky` is set, the suite failed, and every
/// failure in every run's report is quarantined. A failed run its report
/// cannot explain is never pardoned.
fn pardoned_failures(
    args: &cli::RunArgs,
    suite_red: bool,
    quarantine: &std::collections::BTreeSet<String>,
    outcomes: &[runner::RunOutcome],
    reports: &[junit::JunitReport],
) -> Option<Vec<String>> {
    if !args.fail_on_flaky || !suite_red {
        return None;
    }
    quarantine::pardon(quarantine, outcomes, reports)
}

/// Label a failing run's failures with what sooth already knew about them.
/// A red run without a report leaves nothing to label: sooth does not guess
/// which test failed.
fn explain_failures(
    suite_red: bool,
    reports: &[junit::JunitReport],
    passes: analyzers::explain::Passes<'_>,
    quarantine: &std::collections::BTreeSet<String>,
) -> Option<Vec<analyzers::explain::Explanation>> {
    if !suite_red {
        return None;
    }
    // Every run's failures: run 1's are not forgiven by a green run 2.
    let failed: Vec<String> = reports
        .iter()
        .flat_map(verify::failed_ids)
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect();
    if failed.is_empty() {
        return None;
    }
    Some(analyzers::explain::explain(&failed, passes, quarantine))
}

/// The quarantine list, degrading to empty: an unreadable file warns and
/// leaves every failure unrecognized (and, with `--fail-on-flaky`,
/// unpardoned).
fn load_quarantine() -> std::collections::BTreeSet<String> {
    let path = std::path::Path::new(quarantine::FILE_NAME);
    quarantine::load(path).unwrap_or_else(|err| {
        report::warn(&format!(
            "could not read `{}`: {err} — no failure will be recognized as a known flake",
            path.display()
        ));
        std::collections::BTreeSet::new()
    })
}

/// The stderr note when the runner's exit and the report disagree, in
/// either direction. The exit code is settled elsewhere; this explains it.
#[expect(clippy::print_stderr)]
fn print_disagreement(runs_failed: bool, report_failures: usize, has_reports: bool) {
    if let Some(note) = disagreement(runs_failed, report_failures, has_reports) {
        eprintln!("sooth: {note}");
    }
}

/// What to say when the runner's exit and its report disagree about the
/// suite — the run still exits 1, and this is why.
fn disagreement(runs_failed: bool, report_failures: usize, has_reports: bool) -> Option<String> {
    if report_failures > 0 && !runs_failed {
        Some(format!(
            "the runner exited 0 but the report shows {report_failures} failing \
             test(s) — exiting 1 (the runner and its report must agree for a 0)"
        ))
    } else if runs_failed && report_failures == 0 && has_reports {
        Some(
            "the runner failed but the report shows 0 failing tests — exiting 1 (a \
             failure is never upgraded to a pass)"
                .to_owned(),
        )
    } else {
        None
    }
}

/// Re-run only the failed tests and classify them, when `--verify` asked
/// for it. A verification that cannot be made degrades to a warning and
/// `None`, and the exit code is not changed. `Err` is the one exception:
/// sooth was asked to stop before the re-runs were done.
fn verify_failures(
    args: &cli::RunArgs,
    plan: Option<&preset::Plan>,
    final_report: Option<&junit::JunitReport>,
    interrupt: &interrupt::Interrupt,
) -> Result<Option<verify::Verdict>, report::Interrupted> {
    let (true, Some(plan), Some(final_report)) = (args.verify, plan, final_report) else {
        return Ok(None);
    };
    match verify::run(plan, final_report, interrupt) {
        Ok(verdict) => Ok(verdict),
        Err(verify::Unverified::Skipped(why)) => {
            report::warn(&why);
            Ok(None)
        }
        Err(verify::Unverified::Interrupted) => Err(report::Interrupted::Verification),
    }
}

/// What the passive layer produced for this run: the classification, plus
/// what stood behind it — how many observations from *earlier* runs, and how
/// many of those could never be evidence.
struct HistoryPass {
    analysis: analyzers::history::Analysis,
    /// What the history held before this invocation added to it.
    prior: analyzers::history::PriorEvidence,
}

/// The identity of the code about to run, when this invocation will record
/// history. Read before the first run: what the run writes — the user's
/// `--junit` report included — is not a change to the code it ran.
fn identity_before_run(
    args: &cli::RunArgs,
    report_source: Option<&ReportSource>,
) -> Option<history::CodeIdentity> {
    if args.no_history {
        return None;
    }
    let user_report = match report_source? {
        ReportSource::User(path) => Some(path.as_path()),
        ReportSource::Preset(_) => None,
    };
    Some(history::code_identity(
        std::path::Path::new("."),
        user_report,
    ))
}

/// Record this invocation's runs into the local history and classify the
/// accumulated observations. Every failure degrades to a stderr warning:
/// the passive layer must never change the run's outcome.
#[expect(clippy::print_stderr)]
fn record_history(
    identity: Option<history::CodeIdentity>,
    reports: &[junit::JunitReport],
) -> Option<HistoryPass> {
    if reports.is_empty() {
        return None;
    }
    let identity = history::identity_after_run(identity?, std::path::Path::new("."));
    let environment = history::current_environment();
    let at_epoch_secs = history::now_epoch_secs();
    let invocation = history::run_invocation(at_epoch_secs);
    let mut observations = Vec::new();
    for report in reports {
        let durations = analyzers::flaky::run_durations(report);
        for (id, status) in analyzers::flaky::run_outcomes(report) {
            observations.push(history::Observation {
                duration_ms: durations.get(&id).map(history::whole_millis),
                id,
                status,
                commit: identity.commit.clone(),
                dirty: identity.dirty,
                environment: Some(environment.clone()),
                invocation: Some(invocation.clone()),
                at_epoch_secs,
            });
        }
    }
    let path = std::path::Path::new(history::HISTORY_PATH);
    if let Err(err) = append_history(path, &observations) {
        eprintln!(
            "sooth: could not write {}: {err} — history skipped for this run",
            path.display()
        );
        return None;
    }
    let loaded = load_history(path);
    // This run's own observations are already in the file; only earlier ones
    // can make a failure read as anything but new.
    let earlier = loaded.observations.len().saturating_sub(observations.len());
    let prior = analyzers::history::PriorEvidence::of(
        loaded.observations.get(..earlier).unwrap_or_default(),
    );
    Some(HistoryPass {
        analysis: analyzers::history::analyze(&loaded.observations),
        prior,
    })
}

/// Append to the history. The first write creates `.sooth/`; that is the
/// moment sooth keeps it out of git's view, and the one time it says so.
#[expect(clippy::print_stderr)]
fn append_history(
    path: &std::path::Path,
    observations: &[history::Observation],
) -> std::io::Result<()> {
    let created = path.parent().is_some_and(|dir| !dir.exists());
    history::append(path, observations)?;
    if created && !observations.is_empty() && history::exclude_from_git(std::path::Path::new(".")) {
        eprintln!(
            "sooth: added `.sooth/` to .git/info/exclude — the history is local and yours, \
             and this keeps it out of `git status` and `git add -A`"
        );
    }
    Ok(())
}

/// Load the history file, warning once about lines it could not read.
#[expect(clippy::print_stderr)]
fn load_history(path: &std::path::Path) -> history::Loaded {
    let loaded = history::load(path);
    if loaded.skipped_lines > 0 {
        eprintln!(
            "sooth: ignored {} unreadable line(s) in {}",
            loaded.skipped_lines,
            path.display()
        );
    }
    loaded
}

/// Handle `sooth explain`: label every failure in a JUnit-XML report against
/// the accumulated evidence. Runs nothing, records nothing (see
/// `DECISIONS.md`); exits 0 when the report could be read, 2 when it could not.
#[expect(clippy::print_stdout)]
fn explain(args: &cli::ExplainArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let report = load_report(&args.junit).map_err(SoothError::new)?;
    let loaded = load_history(std::path::Path::new(history::HISTORY_PATH));
    let analysis = analyzers::history::analyze(&loaded.observations);
    let quarantine = load_quarantine();
    // Explain runs nothing, so only the history has anything to contribute.
    let passes = analyzers::explain::Passes {
        history: Some(&analysis),
        ..analyzers::explain::Passes::default()
    };
    let explanations =
        analyzers::explain::explain(&verify::failed_ids(&report), passes, &quarantine);

    if args.json {
        println!("{}", report::explanation_json(&args.junit, &explanations));
        return Ok(ExitCode::SUCCESS);
    }
    if explanations.is_empty() {
        println!(
            "no failures in `{}` — nothing to explain",
            args.junit.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    report::print_explanation(
        &Analyses {
            explanation: Some(&explanations),
            history_observations: Some(analyzers::history::PriorEvidence::of(&loaded.observations)),
            ..Analyses::default()
        },
        style,
    );
    Ok(ExitCode::SUCCESS)
}

/// The human report's sections, in the one order both output modes use.
fn print_sections(
    args: &cli::RunArgs,
    outcomes: &[runner::RunOutcome],
    summary: &JunitSummary,
    analyses: &Analyses<'_>,
    style: report::Style,
) {
    if !quiet_repeats(args) {
        report::print_runs(outcomes, args.runs as usize, style);
    }
    report::print_summary(summary, outcomes.len(), args.slowest.is_some(), style);
    // One line per failing test, then what is known but did not fail: every
    // pass's findings are folded into those two sections (see `DECISIONS.md`).
    report::print_explanation(analyses, style);
    if pardon_gap(args, analyses) {
        report::print_pardon_gap(style);
    }
    report::print_history(analyses, style);
}

/// Whether the run failed on nothing but known flakes while the quarantine
/// pardoned none of them: "nothing new" and exit 1 would otherwise read as a
/// contradiction.
fn pardon_gap(args: &cli::RunArgs, analyses: &Analyses<'_>) -> bool {
    args.fail_on_flaky
        && analyses.pardoned.is_none()
        && analyses.explanation.is_some_and(|explanations| {
            analyzers::explain::Counts::of(explanations).only_known_flakes()
                && explanations.iter().any(|test| !test.quarantined)
        })
}

/// Handle `sooth import`: bring reports produced elsewhere into the local
/// history. Import is the sole observer of these runs, which is why it may
/// record them where `explain` must not (see `DECISIONS.md`). Validation
/// happens before any write: an invocation imports all of its new files or
/// none of them. Exit 0, or 2 when a file is unreadable or the history
/// cannot be written — writing is the job here, not a side effect.
/// One report file accepted for import, waiting for the all-or-nothing write.
struct Incoming {
    at_epoch_secs: u64,
    observations: Vec<history::Observation>,
    hash: u64,
    name: String,
    /// Imported before: `--replace` puts this in the earlier import's place.
    replaces: bool,
}

/// One import file's yield: its per-test outcomes plus the timestamp the
/// content itself carries. `None` is a valid file with nothing to record.
type FileYield = Option<(
    std::collections::BTreeMap<String, (junit::TestStatus, Option<u64>)>,
    Option<String>,
)>;

/// The outcomes one import file yields — `Ok(None)` is a green console log,
/// which names no tests. A file that does not parse is sooth's error.
#[expect(clippy::print_stdout)]
fn file_outcomes(
    content: &str,
    log: Option<cli::LogFormat>,
    path: &std::path::Path,
) -> Result<FileYield, SoothError> {
    match log {
        Some(cli::LogFormat::Phpunit) => match phpunit_log::parse_str(content) {
            Ok(parsed) if parsed.failures.is_empty() => {
                println!(
                    "{}: no failures to record — a green log names no tests",
                    path.display()
                );
                Ok(None)
            }
            // A console log names its failures and no durations.
            Ok(parsed) => Ok(Some((
                parsed
                    .failures
                    .into_iter()
                    .map(|(id, status)| (id, (status, None)))
                    .collect(),
                parsed.at,
            ))),
            Err(err) => Err(SoothError::new(format!(
                "failed to parse PHPUnit log `{}`: {err}",
                path.display()
            ))),
        },
        None => match junit::parse_str(content) {
            Ok(report) => {
                let durations = analyzers::flaky::run_durations(&report);
                let outcomes = analyzers::flaky::run_outcomes(&report)
                    .into_iter()
                    .map(|(id, status)| {
                        let millis = durations.get(&id).map(history::whole_millis);
                        (id, (status, millis))
                    })
                    .collect();
                Ok(Some((outcomes, junit::report_timestamp(content))))
            }
            Err(err) => Err(SoothError::new(format!(
                "failed to parse JUnit-XML report `{}`: {err}",
                path.display()
            ))),
        },
    }
}

/// `--commit` as the sha local runs record, or why it is refused.
fn imported_commit(given: &str) -> Result<String, SoothError> {
    history::resolve_commit(std::path::Path::new("."), given).map_err(|error| {
        let why = match error {
            history::CommitError::NotASha => {
                "is not a sha — pass the commit CI ran on, not a branch or revision name"
            }
            history::CommitError::Unknown => {
                "names no commit this repository knows — pass the full sha, or fetch it first"
            }
        };
        SoothError::new(format!("`--commit {given}` {why}"))
    })
}

#[expect(clippy::print_stdout)]
fn import(args: &cli::ImportArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let commit = args.commit.as_deref().map(imported_commit).transpose()?;
    let ledger_path = std::path::Path::new(history::IMPORTED_PATH);
    let ledger = history::imported_hashes(ledger_path);
    let (mut incoming, lines) = read_imports(args, commit.as_deref(), &ledger)?;

    // Ascending time order, so the file stays readable front to back; the
    // analysis itself no longer depends on it.
    incoming.sort_by_key(|entry| entry.at_epoch_secs);
    let history_path = std::path::Path::new(history::HISTORY_PATH);
    write_imports(history_path, &incoming)?;
    // Said once it is true: the files are in the history now.
    for line in lines {
        println!("{line}");
    }
    record_ledger(ledger_path, &incoming);

    let loaded = load_history(history_path);
    let analysis = analyzers::history::analyze(&loaded.observations);
    report::print_history(
        &Analyses {
            history: Some(&analysis),
            ..Analyses::default()
        },
        style,
    );
    report::print_history_size(loaded.observations.len(), loaded.truncated);
    Ok(ExitCode::SUCCESS)
}

/// Read every file of an import into observations, plus the line that
/// says what became of each. Nothing is written here: a file that cannot
/// be read or parsed fails the whole import before the history is touched.
fn read_imports(
    args: &cli::ImportArgs,
    commit: Option<&str>,
    ledger: &std::collections::BTreeSet<u64>,
) -> Result<(Vec<Incoming>, Vec<String>), SoothError> {
    let mut incoming: Vec<Incoming> = Vec::new();
    let mut lines = Vec::new();
    let mut named_before = std::collections::BTreeSet::new();
    for path in &args.reports {
        let content = std::fs::read_to_string(path).map_err(|err| {
            SoothError::new(format!("could not read `{}`: {err}", path.display()))
        })?;
        let hash = history::fnv1a64(content.as_bytes());
        // The same content twice on one command line is one file.
        if !named_before.insert(hash) {
            lines.push(format!(
                "{}: skipped (same content as an earlier file of this import)",
                path.display()
            ));
            continue;
        }
        let replaces = ledger.contains(&hash);
        if replaces && !args.replace {
            lines.push(format!(
                "{}: skipped (already imported) — pass `--replace` to import it again \
                 in place of its earlier observations",
                path.display()
            ));
            continue;
        }
        let parsed = file_outcomes(&content, args.log, path)?;
        let Some((outcomes, content_stamp)) = parsed else {
            continue;
        };
        let at_epoch_secs = content_stamp
            .and_then(|stamp| history::iso8601_to_epoch(&stamp))
            .or_else(|| file_state(path).and_then(|(mtime, _)| epoch_secs(mtime)))
            .unwrap_or_else(history::now_epoch_secs);
        let observations: Vec<history::Observation> = outcomes
            .into_iter()
            .map(|(id, (status, duration_ms))| history::Observation {
                id,
                status,
                duration_ms,
                commit: commit.map(str::to_owned),
                // --commit asserts a clean checkout of that commit; without
                // it the code state is unknowable, not clean.
                dirty: commit.map(|_| false),
                environment: Some(args.env.clone()),
                invocation: Some(history::import_invocation(hash)),
                at_epoch_secs,
            })
            .collect();
        lines.push(format!(
            "{}: {}{}",
            path.display(),
            report::count(observations.len(), "observation"),
            if replaces {
                ", replacing its earlier import"
            } else {
                ""
            }
        ));
        incoming.push(Incoming {
            at_epoch_secs,
            observations,
            hash,
            name: path.display().to_string(),
            replaces,
        });
    }
    Ok((incoming, lines))
}

/// Put an import's observations into the history: appended, or — when
/// `--replace` imports a file again — written in the place of that file's
/// earlier observations, in one rewrite.
#[expect(clippy::print_stderr)]
fn write_imports(history_path: &std::path::Path, incoming: &[Incoming]) -> Result<(), SoothError> {
    let observations: Vec<history::Observation> = incoming
        .iter()
        .flat_map(|entry| entry.observations.iter().cloned())
        .collect();
    let replaced: Vec<String> = incoming
        .iter()
        .filter(|entry| entry.replaces)
        .map(|entry| history::import_invocation(entry.hash))
        .collect();
    let failed = |err: std::io::Error| {
        SoothError::new(format!("could not write {}: {err}", history_path.display()))
    };
    if !replaced.is_empty() {
        match history::replace_invocations(history_path, &replaced, &observations) {
            Ok(Some(0)) => {
                eprintln!(
                    "sooth: found no observations of the earlier import in {} — nothing \
                     was removed, and the file's observations were added. An import \
                     recorded before v0.3.1 carries no id to find it by",
                    history_path.display()
                );
                return Ok(());
            }
            Ok(Some(_)) => return Ok(()),
            // No history file: nothing to replace, so this is a plain import.
            Ok(None) => {}
            Err(err) => return Err(failed(err)),
        }
    }
    append_history(history_path, &observations).map_err(failed)
}

/// Note the newly imported files in the ledger. A replaced file is in it
/// already.
#[expect(clippy::print_stderr)]
fn record_ledger(ledger_path: &std::path::Path, incoming: &[Incoming]) {
    let ledger: Vec<(u64, String)> = incoming
        .iter()
        .filter(|entry| !entry.replaces)
        .map(|entry| (entry.hash, entry.name.clone()))
        .collect();
    if let Err(err) = history::record_imported(ledger_path, &ledger) {
        // The observations are recorded; only the dedupe bookkeeping failed.
        eprintln!(
            "sooth: could not write {}: {err} — re-importing these files will double-count",
            ledger_path.display()
        );
    }
}

/// `sooth history` looks at the evidence without adding to it: nothing runs,
/// so the observer principle leaves nothing to record.
#[expect(clippy::print_stdout)]
fn history_command(args: &cli::HistoryArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let path = std::path::Path::new(history::HISTORY_PATH);
    if args.prune {
        let pruned = history::prune(path)
            .map_err(|err| SoothError::new(format!("could not prune {}: {err}", path.display())))?;
        // No file: the empty-history line below says all there is to say.
        if let Some(pruned) = pruned {
            report::print_pruned(pruned);
        }
    }
    let loaded = load_history(path);
    if loaded.observations.is_empty() {
        println!("the history is empty — every `sooth run` and `sooth import` adds to it");
        return Ok(ExitCode::SUCCESS);
    }
    let analysis = analyzers::history::analyze(&loaded.observations);
    if analysis.is_empty() {
        println!("nothing stands out: no proven flakes, no failing-since pointers");
    }
    report::print_history(
        &Analyses {
            history: Some(&analysis),
            ..Analyses::default()
        },
        style,
    );
    report::print_history_gap(
        Some(analyzers::history::PriorEvidence::of(&loaded.observations)),
        style,
    );
    report::print_history_size(loaded.observations.len(), loaded.truncated);
    Ok(ExitCode::SUCCESS)
}

/// Seconds since the epoch for a file timestamp; `None` before 1970.
fn epoch_secs(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// Print the run's output in the shape the flags ask for. Returns an exit
/// code only when emitting itself failed (the JSON file could not be
/// written).
#[expect(clippy::print_stdout, clippy::too_many_arguments)]
fn emit_output(
    args: &cli::RunArgs,
    outcomes: &[runner::RunOutcome],
    junit_summary: Option<&JunitSummary>,
    analyses: &Analyses<'_>,
    report_failures: usize,
    failed: bool,
    style: report::Style,
) -> Result<(), SoothError> {
    match (junit_summary, &args.json) {
        // Bare --json: sooth's own stdout output is exactly one line of
        // JSON, printed after the wrapped command finished (last-line
        // contract — the child's output still shares the stream).
        (Some(summary), Some(None)) => {
            println!("{}", report::to_json(outcomes, summary, analyses));
        }
        // --json=PATH: the machine output goes to a file, the human report
        // stays on stdout.
        (Some(summary), Some(Some(path))) => {
            print_sections(args, outcomes, summary, analyses, style);
            let json = report::to_json(outcomes, summary, analyses);
            write_json_file(path, json)?;
            println!(
                "{}",
                verdict(
                    analyses,
                    outcomes,
                    junit_summary,
                    report_failures,
                    failed,
                    style
                )
            );
        }
        (Some(summary), None) => {
            print_sections(args, outcomes, summary, analyses, style);
            println!(
                "{}",
                verdict(
                    analyses,
                    outcomes,
                    junit_summary,
                    report_failures,
                    failed,
                    style
                )
            );
        }
        (None, _) => {
            // Unreachable with json set: rejected_flag exits earlier when
            // --json has no report source. Assert that invariant locally so
            // a weakened guard fails loudly instead of dropping output.
            debug_assert!(args.json.is_none());
            if !quiet_repeats(args) {
                report::print_runs(outcomes, args.runs as usize, style);
            }
            println!(
                "{}",
                verdict(analyses, outcomes, None, report_failures, failed, style)
            );
            print_report_advice(&args.command);
        }
    }
    Ok(())
}

/// The closing verdict line, pardon-aware: a pardoned run explains why it
/// exits 0 instead of claiming a clean pass.
#[expect(clippy::too_many_arguments)]
fn verdict(
    analyses: &Analyses<'_>,
    outcomes: &[runner::RunOutcome],
    junit_summary: Option<&JunitSummary>,
    report_failures: usize,
    failed: bool,
    style: report::Style,
) -> String {
    // Cut short, the runs carry no verdict — pardoned or not. A suite that
    // ran to its end has one, verified or not.
    if let Some(report::Interrupted::Runs { requested, failed }) = analyses.interrupted {
        return report::interrupted_line(outcomes, requested, failed, style);
    }
    match analyses.pardoned {
        Some(pardoned) => {
            let not_flakes = analyses
                .explanation
                .unwrap_or_default()
                .iter()
                .filter(|explanation| explanation.listed_but_not_a_flake())
                .count();
            report::pardoned_verdict(outcomes, pardoned.len(), not_flakes, style)
        }
        None => report::verdict_line(outcomes, junit_summary, report_failures, failed, style),
    }
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
        if args.fail_on_flaky {
            return Some(
                "`--fail-on-flaky` requires a report: `--junit <PATH>` or `--preset <RUNNER>`",
            );
        }
    }
    if args.show_output && args.runs == 1 {
        return Some(
            "`--show-output` is for repeated runs (`--runs N`): a single run always shows \
             its output",
        );
    }
    if args.changed.is_some() {
        if args.preset.is_none() {
            return Some(
                "`--changed` needs `--preset <RUNNER>`: which files are tests and how to \
                 run only them is runner knowledge",
            );
        }
        if args.runs == 1 {
            return Some("a gate of one run proves nothing — pass `--runs` (20 is a good start)");
        }
    }
    if args.verify {
        match args.preset {
            None => {
                return Some(
                    "`--verify` needs `--preset <RUNNER>`: it re-invokes the runner on the \
                     failed tests, which sooth can only do for a known runner",
                );
            }
            Some(preset) if !preset::supports_selection(preset) => {
                return Some(
                    "`--verify` is not supported for this preset yet — sooth cannot \
                     restrict its runner to a subset of tests",
                );
            }
            Some(_) => {}
        }
        if args.runs > 1 {
            return Some(
                "`--verify` re-runs the failures itself; use it with a single run (drop `--runs`)",
            );
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

/// Whether one run failed, by the same two signals as [`suite_failed`]: the
/// runner's exit, or failures in the report that run left.
fn run_failed(outcome: &runner::RunOutcome, report: Option<&junit::JunitReport>) -> bool {
    !outcome.success || report.is_some_and(junit::JunitReport::has_failures)
}

/// How many of the runs failed. With a report source every run that
/// finished left a report, at the same index as its outcome.
fn failed_runs(outcomes: &[runner::RunOutcome], reports: &[junit::JunitReport]) -> usize {
    outcomes
        .iter()
        .enumerate()
        .filter(|(index, outcome)| run_failed(outcome, reports.get(*index)))
        .count()
}

/// The lines that say why a run's report cannot be used, in the command's
/// own terms: the reason, what a gate of several files should check first,
/// and how the run itself ended.
fn unusable_report(
    session: &Session<'_>,
    error: &ReportError,
    (outcomes, requested_runs): (&[runner::RunOutcome], u32),
) -> Vec<String> {
    let Some(source) = session.report_source else {
        return Vec::new();
    };
    let path = source.path();
    let preset_plan = match source {
        ReportSource::Preset(_) => session.plan,
        ReportSource::User(_) => None,
    };
    let unwritten = !path.exists();
    let mut lines = vec![match (error, preset_plan) {
        (ReportError::Untouched, _) => format!(
            "the JUnit-XML report at `{}` was not touched by this run — it \
             predates the test command. Is the runner writing its report to this path?",
            path.display()
        ),
        // A temp path the user never chose is no use in a message: say what
        // the command most likely ran into instead.
        (ReportError::Unreadable(_), Some(plan)) if unwritten => format!(
            "the test command wrote no JUnit-XML report — {}",
            plan.no_report_hint()
        ),
        (ReportError::Unreadable(err), _) => format!(
            "failed to parse JUnit-XML report `{}`: {err}",
            path.display()
        ),
    }];
    if matches!(error, ReportError::Untouched) {
        return lines;
    }
    lines.extend(
        preset_plan
            .and_then(|plan| plan.single_path_note(session.gated_files))
            .filter(|_| unwritten),
    );
    lines.extend(crash_context(outcomes, requested_runs));
    lines
}

/// Load the JUnit-XML report at `path`, or say why not.
fn load_report(path: &std::path::Path) -> Result<junit::JunitReport, String> {
    junit::parse_file(path).map_err(|err| {
        format!(
            "failed to parse JUnit-XML report `{}`: {err}",
            path.display()
        )
    })
}

/// When the report is unusable and the runner itself failed, keep the run
/// facts sooth already measured instead of discarding them with the report:
/// after a long run, "the crash output above is the real story" beats a bare
/// parse error about a temp path. The total is the *requested* `--runs`
/// count, not the executed count. `None` when every run succeeded.
fn crash_context(outcomes: &[runner::RunOutcome], requested_runs: u32) -> Option<String> {
    let (index, outcome) = outcomes
        .iter()
        .enumerate()
        .rev()
        .find(|(_, outcome)| !outcome.success)?;
    Some(format!(
        "run {} of {} failed ({}, {:.2?}) — the runner's own output above likely \
         explains the unusable report",
        index + 1,
        requested_runs,
        outcome.status_label(),
        outcome.duration
    ))
}

#[cfg(test)]
mod tests {
    use super::{pardon_gap, rejected_flag, suite_failed};
    use crate::analyzers::explain::{Explanation, Verdict};
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
        let Command::Run(args) = parsed.command else {
            panic!("expected `run`");
        };
        args
    }

    fn test_case(name: &str, status: TestStatus, duration_seconds: f64) -> TestCase {
        TestCase {
            name: name.to_owned(),
            classname: None,
            duration: Some(Duration::from_secs_f64(duration_seconds)),
            status,
            failure_type: None,
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
    fn a_run_is_counted_as_failed_by_its_exit_or_by_its_report() {
        let outcomes = [outcome(true), outcome(true), outcome(false)];
        let reports = [
            report_of(TestStatus::Passed),
            // Red by its report alone: the runner exited 0.
            report_of(TestStatus::Failed),
            report_of(TestStatus::Passed),
        ];
        assert_eq!(super::failed_runs(&outcomes, &reports), 2);
        // Without a report source there is only the exit to go by.
        assert_eq!(super::failed_runs(&outcomes, &[]), 1);
    }

    #[test]
    fn a_preset_run_that_writes_no_report_gets_an_actionable_error() {
        use crate::reported::{ReportError, ReportSource};
        let plan = crate::preset::Plan::resolve(&["pytest".to_owned()], crate::cli::Preset::Pytest);
        let source = ReportSource::temp().expect("temp report");
        let spawn = (vec!["pytest".to_owned()], Vec::new());
        let session = super::Session {
            spawn: &spawn,
            report_source: Some(&source),
            plan: Some(&plan),
            gated_files: 0,
            interrupt: &crate::interrupt::Interrupt::never(),
        };
        let unwritten = ReportError::Unreadable(
            crate::junit::parse_file(source.path()).expect_err("nothing was written"),
        );
        let lines = super::unusable_report(&session, &unwritten, (&[outcome(false)], 1));
        // Not a parse error about a temp path the user never chose.
        assert!(lines[0].contains("wrote no JUnit-XML report"), "{lines:?}");
        assert!(lines[0].contains("pytest"), "{lines:?}");
        assert!(!lines[0].contains("failed to parse"), "{lines:?}");
        assert!(lines[1].contains("run 1 of 1 failed"), "{lines:?}");
    }

    #[test]
    fn a_users_report_left_untouched_is_said_without_a_guess_at_the_cause() {
        use crate::reported::{ReportError, ReportSource};
        let source = ReportSource::User(std::path::PathBuf::from("build/junit.xml"));
        let spawn = (vec!["pytest".to_owned()], Vec::new());
        let session = super::Session {
            spawn: &spawn,
            report_source: Some(&source),
            plan: None,
            gated_files: 0,
            interrupt: &crate::interrupt::Interrupt::never(),
        };
        let lines =
            super::unusable_report(&session, &ReportError::Untouched, (&[outcome(false)], 1));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("`build/junit.xml` was not touched by this run"));
    }

    #[test]
    fn a_missing_user_junit_file_reports_the_path() {
        let missing = std::env::temp_dir().join("sooth-test-no-such-report.xml");
        let message =
            super::load_report(&missing).expect_err("a missing --junit file should error");
        assert!(message.contains("failed to parse"));
        assert!(message.contains("sooth-test-no-such-report.xml"));
    }

    #[test]
    fn crash_context_names_the_failed_run_and_its_status() {
        let outcomes = [outcome(true), outcome(false)];
        let context = super::crash_context(&outcomes, 2).expect("a failed run should give context");
        assert!(context.contains("run 2 of 2"));
        assert!(context.contains("runner exit=1"));
        assert!(context.contains("output above"));
    }

    #[test]
    fn crash_context_counts_against_the_requested_runs_not_the_executed_ones() {
        let outcomes = [outcome(false)];
        let context = super::crash_context(&outcomes, 3).expect("a failed run should give context");
        assert!(context.contains("run 1 of 3"));
    }

    #[test]
    fn crash_context_is_silent_when_every_run_succeeded() {
        assert_eq!(super::crash_context(&[outcome(true)], 1), None);
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

    #[test]
    fn reportless_fail_on_flaky_is_rejected() {
        let args = parse_run_args(&["sooth", "run", "--fail-on-flaky", "--", "true"]);
        let reason = rejected_flag(&args).expect("flag should be rejected");
        assert!(reason.contains("--fail-on-flaky"), "got: {reason}");
    }

    #[test]
    fn show_output_without_repeats_is_rejected() {
        // One run always shows its output: the flag would do nothing.
        let args = parse_run_args(&["sooth", "run", "--show-output", "--", "true"]);
        assert!(rejected_flag(&args).is_some_and(|reason| reason.contains("--show-output")));
        let args = parse_run_args(&["sooth", "run", "--show-output", "--runs", "2", "--", "true"]);
        assert_eq!(rejected_flag(&args), None);
    }

    #[test]
    fn verify_with_multiple_runs_is_rejected() {
        // The acceptance side of this boundary — --verify with the default
        // single run — is pinned end to end by the stdout contract suite.
        let args = parse_run_args(&[
            "sooth", "run", "--verify", "--preset", "pytest", "--runs", "2", "--", "pytest",
        ]);
        let reason = rejected_flag(&args).expect("flag should be rejected");
        assert!(reason.contains("single run"), "got: {reason}");
    }

    #[test]
    fn the_runner_and_its_report_disagreeing_is_said_in_both_directions() {
        use super::disagreement;
        // Exit 0 over a red report.
        assert!(disagreement(false, 2, true).is_some_and(|note| note.contains("shows 2 failing")));
        // A red exit over a green report — only when there is a report.
        assert!(disagreement(true, 0, true).is_some_and(|note| note.contains("0 failing tests")));
        assert_eq!(disagreement(true, 0, false), None);
        // Agreement, in either colour, needs no note.
        assert_eq!(disagreement(false, 0, true), None);
        assert_eq!(disagreement(true, 3, true), None);
    }

    fn known_flake(id: &str, quarantined: bool) -> Explanation {
        Explanation {
            observed: None,
            id: id.to_owned(),
            verdict: Verdict::KnownFlake {
                failed_runs: 1,
                observed_runs: 4,
                failures_confined_to: None,
                red_since: None,
            },
            quarantined,
        }
    }

    fn gap_for(cmdline: &[&str], explanations: &[Explanation]) -> bool {
        pardon_gap(
            &parse_run_args(cmdline),
            &crate::report::Analyses {
                explanation: Some(explanations),
                ..crate::report::Analyses::default()
            },
        )
    }

    #[test]
    fn nothing_new_yet_no_pardon_is_explained_not_left_contradictory() {
        let flagged = [
            "sooth",
            "run",
            "--fail-on-flaky",
            "--junit",
            "r.xml",
            "--",
            "true",
        ];
        assert!(
            gap_for(&flagged, &[known_flake("c::a", false)]),
            "an unlisted known flake under --fail-on-flaky needs the note"
        );
        assert!(
            !gap_for(&flagged, &[known_flake("c::a", true)]),
            "a listed flake was pardoned; there is no gap to explain"
        );
        assert!(
            !gap_for(
                &["sooth", "run", "--junit", "r.xml", "--", "true"],
                &[known_flake("c::a", false)]
            ),
            "without --fail-on-flaky no pardon was ever asked for"
        );
    }

    #[test]
    fn a_new_failure_needs_no_pardon_note() {
        // The verdict speaks for itself: something new failed.
        let explanations = [
            known_flake("c::a", false),
            Explanation {
                id: "c::fresh".to_owned(),
                observed: None,
                verdict: Verdict::Unknown,
                quarantined: false,
            },
        ];
        assert!(!gap_for(
            &[
                "sooth",
                "run",
                "--fail-on-flaky",
                "--junit",
                "r.xml",
                "--",
                "true"
            ],
            &explanations
        ));
    }
}
