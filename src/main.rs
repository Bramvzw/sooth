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
mod junit;
mod phpunit_log;
mod preset;
mod quarantine;
mod report;
mod runner;
mod verify;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::report::{Analyses, JunitSummary};

/// Exit code for "sooth itself failed", as opposed to "the tests failed" (1).
const EXIT_SOOTH_ERROR: u8 = 2;

/// How many of the slowest tests the summary shows when `--slowest` is not
/// given.
const DEFAULT_SLOWEST: usize = 10;

fn main() -> ExitCode {
    let parsed = Cli::parse();
    match parsed.command {
        Command::Run(args) => run(&args),
        Command::Explain(args) => explain(&args),
        Command::Import(args) => import(&args),
        Command::History(args) => history_command(&args),
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

    let (command_args, selection) = match gated_command(args, style) {
        Ok(Some(gated)) => gated,
        Ok(None) => return ExitCode::SUCCESS,
        Err(exit) => return exit,
    };
    let ((command, envs), report_source) = match prepared_command(args, &command_args) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
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
            match load_run_report(source, &command[0], report_before, &outcomes, args.runs) {
                Ok(report) => reports.push(report),
                Err(exit) => return exit,
            }
        }
    }
    if let Some(ReportSource::Preset(path)) = &report_source {
        // Best-effort cleanup of the preset's private report dir — every
        // run's parse result is already in memory.
        cleanup_preset_report(path);
    }

    let (junit_summary, flaky_analysis) = summarize_reports(args, &reports);
    let history_pass = record_history(args, &reports);
    let history_analysis = history_pass.as_ref().map(|pass| &pass.analysis);
    // The raw command: inject_selected re-injects the report flags itself.
    let verify_verdict = verify_failures(args, &args.command, reports.last());

    let suite_red = suite_failed(&outcomes, &reports);
    // The list labels known flakes on any red run; only --fail-on-flaky
    // lets it steer the exit.
    let quarantine = if suite_red {
        quarantine::load_or_empty(std::path::Path::new(quarantine::FILE_NAME))
    } else {
        std::collections::BTreeSet::new()
    };
    let passes = analyzers::explain::Passes {
        active: flaky_analysis.as_ref(),
        verify: verify_verdict.as_ref(),
        history: history_analysis,
    };
    let explanation = explain_failures(suite_red, &reports, passes, &quarantine);
    let pardoned = pardoned_failures(args, suite_red, &quarantine, &outcomes, &reports);
    let failed = suite_red && pardoned.is_none();
    let analyses = Analyses {
        flaky: flaky_analysis.as_ref(),
        history: history_analysis,
        history_observations: history_pass.as_ref().map(HistoryPass::prior_evidence),
        verify: verify_verdict.as_ref(),
        pardoned: pardoned.as_deref(),
        explanation: explanation.as_deref(),
        gate: selection.as_ref(),
    };
    // The worst run's count, over all reports: the mismatch note and the
    // verdict must not claim "0 failing" because the *last* run was green.
    let report_failures = reports
        .iter()
        .map(junit::JunitReport::failing_count)
        .max()
        .unwrap_or(0);
    let runs_failed = outcomes.iter().any(|outcome| !outcome.success);
    // A pardoned run exits 0; the note's "exiting 1" would then be a lie.
    if failed {
        print_disagreement(runs_failed, report_failures, !reports.is_empty());
    }

    if let Some(exit) = emit_output(
        args,
        &outcomes,
        junit_summary.as_ref(),
        &analyses,
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

/// The command to spawn, its environment, and where its report comes from:
/// the preset injects flags and manages a temp report, `--junit` points at
/// the user's own file.
fn prepared_command(
    args: &cli::RunArgs,
    command: &[String],
) -> Result<(preset::Spawn, Option<ReportSource>), ExitCode> {
    match args.preset {
        Some(chosen) => {
            let path = preset::report_path().map_err(|err| {
                eprintln!("sooth: failed to create a temp directory for the report: {err}");
                ExitCode::from(EXIT_SOOTH_ERROR)
            })?;
            let spawn = preset::inject(command, chosen, &path);
            Ok((spawn, Some(ReportSource::Preset(path))))
        }
        None => Ok((
            (command.to_vec(), Vec::new()),
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

/// The command to spawn plus the gate's selection when `--changed` gated it.
type GatedRun = (Vec<String>, Option<gate::Selection>);

/// The command with the gate's selection appended, plus the selection for
/// the report. `Ok(None)` is the gate's happy fast path: nothing changed,
/// nothing to prove, nothing spawned — its output is already emitted.
fn gated_command(args: &cli::RunArgs, style: report::Style) -> Result<Option<GatedRun>, ExitCode> {
    let Some(base) = &args.changed else {
        return Ok(Some((args.command.clone(), None)));
    };
    let preset = args.preset.expect("rejected_flag guarantees a preset");
    let selection = match gate::resolve(base.as_deref(), preset, std::path::Path::new(".")) {
        Ok(selection) => selection,
        Err(reason) => {
            eprintln!("sooth: {reason}");
            return Err(ExitCode::from(EXIT_SOOTH_ERROR));
        }
    };
    if selection.files.is_empty() {
        return empty_gate(args, &selection).map(|()| None);
    }
    // Bare --json owns sooth's own stdout (one line of JSON); the selection
    // rides in the JSON's `gate` object instead.
    if !matches!(args.json, Some(None)) {
        report::print_gate(&selection, style);
    }
    let mut command = args.command.clone();
    command.extend(preset::selected_paths(preset, &selection.files));
    Ok(Some((command, Some(selection))))
}

/// The empty gate's output, honoring every output mode's contract: bare
/// `--json` emits the document (zero runs), `--json=PATH` writes it and
/// keeps the human line on stdout.
fn empty_gate(args: &cli::RunArgs, selection: &gate::Selection) -> Result<(), ExitCode> {
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
fn write_json_file(path: &std::path::Path, json: String) -> Result<(), ExitCode> {
    std::fs::write(path, json + "\n").map_err(|err| {
        eprintln!(
            "sooth: failed to write JSON report `{}`: {err}",
            path.display()
        );
        ExitCode::from(EXIT_SOOTH_ERROR)
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
    quarantine_pardon(quarantine, outcomes, reports)
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

/// The pardon decision itself: all-or-nothing over every run.
fn quarantine_pardon(
    quarantine: &std::collections::BTreeSet<String>,
    outcomes: &[runner::RunOutcome],
    reports: &[junit::JunitReport],
) -> Option<Vec<String>> {
    if reports.is_empty() || reports.len() != outcomes.len() {
        return None;
    }
    let mut pardoned = std::collections::BTreeSet::new();
    for (outcome, report) in outcomes.iter().zip(reports) {
        // A signal-killed run is a crash, never a pardonable flake — even
        // when the report was written before the kill.
        if outcome.signal.is_some() {
            return None;
        }
        let failing = verify::failed_ids(report);
        if !outcome.success && failing.is_empty() {
            return None;
        }
        for id in failing {
            if !quarantine.contains(&id) {
                return None;
            }
            pardoned.insert(id);
        }
    }
    (!pardoned.is_empty()).then(|| pardoned.into_iter().collect())
}

/// The stderr note when the runner's exit and the report disagree, in
/// either direction. The exit code is settled elsewhere; this explains it.
fn print_disagreement(runs_failed: bool, report_failures: usize, has_reports: bool) {
    if report_failures > 0 && !runs_failed {
        eprintln!(
            "sooth: the runner exited 0 but the report shows {report_failures} failing \
             test(s) — exiting 1 (the runner and its report must agree for a 0)"
        );
    } else if runs_failed && report_failures == 0 && has_reports {
        eprintln!(
            "sooth: the runner failed but the report shows 0 failing tests — \
             exiting 1 (a failure is never upgraded to a pass)"
        );
    }
}

/// Re-run only the failed tests and classify them. Every failure mode
/// degrades to a warning and `None`; the exit code is never changed.
fn verify_failures(
    args: &cli::RunArgs,
    command: &[String],
    final_report: Option<&junit::JunitReport>,
) -> Option<verify::Verdict> {
    if !args.verify {
        return None;
    }
    let preset = args.preset?;
    let report = final_report?;
    let failed = verify::failed_tests(report);
    if failed.is_empty() {
        return None;
    }
    // Selection gets the raw name half; the joined id is never re-split.
    let names: Vec<String> = failed.iter().map(|test| test.name.clone()).collect();
    let mut verify_reports = Vec::with_capacity(verify::VERIFY_RUNS as usize);
    for attempt in 1..=verify::VERIFY_RUNS {
        let path = match preset::report_path() {
            Ok(path) => path,
            Err(err) => {
                eprintln!("sooth: verification skipped — could not create a temp report: {err}");
                return None;
            }
        };
        let Some((cmd, envs)) = preset::inject_selected(command, preset, &path, &names) else {
            eprintln!(
                "sooth: verification is not supported for this preset yet — \
                 sooth cannot restrict its runner to a subset of tests"
            );
            let _ = std::fs::remove_dir(path.parent().unwrap_or(&path));
            return None;
        };
        match runner::run_once(&cmd, &envs) {
            Ok(_) => {}
            Err(err) => {
                eprintln!("sooth: verification run {attempt} failed to start: {err}");
                cleanup_preset_report(&path);
                return None;
            }
        }
        match junit::parse_file(&path) {
            Ok(report) => verify_reports.push(report),
            Err(err) => {
                eprintln!("sooth: verification run {attempt} produced no usable report: {err}");
                cleanup_preset_report(&path);
                return None;
            }
        }
        cleanup_preset_report(&path);
    }
    let ids: Vec<String> = failed.into_iter().map(|test| test.id).collect();
    Some(verify::classify(&ids, &verify_reports))
}

/// What the passive layer produced for this run: the classification, plus
/// what stood behind it — how many observations from *earlier* runs, and how
/// many of those could never be evidence.
struct HistoryPass {
    analysis: analyzers::history::Analysis,
    prior_observations: usize,
    /// Prior observations made on a dirty or unknown tree. They count in the
    /// totals but prove nothing, so a history that is mostly these looks
    /// broken when it is only blind.
    prior_unusable: usize,
}

impl HistoryPass {
    fn prior_evidence(&self) -> report::PriorEvidence {
        report::PriorEvidence {
            observations: self.prior_observations,
            unusable: self.prior_unusable,
        }
    }
}

/// Record this invocation's runs into the local history and classify the
/// accumulated observations. Every failure degrades to a stderr warning:
/// the passive layer must never change the run's outcome.
fn record_history(args: &cli::RunArgs, reports: &[junit::JunitReport]) -> Option<HistoryPass> {
    if args.no_history || reports.is_empty() {
        return None;
    }
    let identity = history::code_identity(std::path::Path::new("."));
    let environment = history::current_environment();
    let at_epoch_secs = history::now_epoch_secs();
    let mut observations = Vec::new();
    for report in reports {
        for (id, status) in analyzers::flaky::run_outcomes(report) {
            observations.push(history::Observation {
                id,
                status,
                commit: identity.commit.clone(),
                dirty: identity.dirty,
                environment: Some(environment.clone()),
                at_epoch_secs,
            });
        }
    }
    let path = std::path::Path::new(history::HISTORY_PATH);
    if let Err(err) = history::append(path, &observations) {
        eprintln!(
            "sooth: could not write {}: {err} — history skipped for this run",
            path.display()
        );
        return None;
    }
    let loaded = load_history(path);
    // This run's own observations are already in the file; only earlier ones
    // can make a failure read as anything but new.
    let prior = loaded.observations.len().saturating_sub(observations.len());
    let prior_unusable = loaded
        .observations
        .iter()
        .take(prior)
        .filter(|observation| observation.dirty != Some(false))
        .count();
    Some(HistoryPass {
        analysis: analyzers::history::analyze(&loaded.observations),
        prior_observations: prior,
        prior_unusable,
    })
}

/// Load the history file, warning once about lines it could not read.
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
fn explain(args: &cli::ExplainArgs) -> ExitCode {
    let style = report::Style::resolved(args.color);
    let report = match load_report(&args.junit, None) {
        Ok(report) => report,
        Err(message) => {
            eprintln!("sooth: {message}");
            return ExitCode::from(EXIT_SOOTH_ERROR);
        }
    };
    let loaded = load_history(std::path::Path::new(history::HISTORY_PATH));
    let analysis = analyzers::history::analyze(&loaded.observations);
    let quarantine = quarantine::load_or_empty(std::path::Path::new(quarantine::FILE_NAME));
    // Explain runs nothing, so only the history has anything to contribute.
    let passes = analyzers::explain::Passes {
        history: Some(&analysis),
        ..analyzers::explain::Passes::default()
    };
    let explanations =
        analyzers::explain::explain(&verify::failed_ids(&report), passes, &quarantine);

    if args.json {
        println!("{}", report::explanation_json(&args.junit, &explanations));
        return ExitCode::SUCCESS;
    }
    if explanations.is_empty() {
        println!(
            "no failures in `{}` — nothing to explain",
            args.junit.display()
        );
        return ExitCode::SUCCESS;
    }
    report::print_explanation(
        &Analyses {
            explanation: Some(&explanations),
            history_observations: Some(report::PriorEvidence {
                observations: loaded.observations.len(),
                unusable: loaded
                    .observations
                    .iter()
                    .filter(|observation| observation.dirty != Some(false))
                    .count(),
            }),
            ..Analyses::default()
        },
        style,
    );
    ExitCode::SUCCESS
}

/// The human report's sections, in the one order both output modes use.
fn print_sections(
    args: &cli::RunArgs,
    outcomes: &[runner::RunOutcome],
    summary: &JunitSummary,
    analyses: &Analyses<'_>,
    style: report::Style,
) {
    report::print_runs(outcomes, style);
    report::print_summary(summary, style);
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
}

/// One import file's yield: its per-test outcomes plus the timestamp the
/// content itself carries. `None` is a valid file with nothing to record.
type FileYield = Option<(
    std::collections::BTreeMap<String, junit::TestStatus>,
    Option<String>,
)>;

/// The outcomes one import file yields — `Ok(None)` is a green console log,
/// which names no tests. Parse failures print here and become exit 2.
fn file_outcomes(
    content: &str,
    log: Option<cli::LogFormat>,
    path: &std::path::Path,
) -> Result<FileYield, ()> {
    match log {
        Some(cli::LogFormat::Phpunit) => match phpunit_log::parse_str(content) {
            Ok(parsed) if parsed.failures.is_empty() => {
                println!(
                    "{}: no failures to record — a green log names no tests",
                    path.display()
                );
                Ok(None)
            }
            Ok(parsed) => Ok(Some((parsed.failures, parsed.at))),
            Err(err) => {
                eprintln!(
                    "sooth: failed to parse PHPUnit log `{}`: {err}",
                    path.display()
                );
                Err(())
            }
        },
        None => match junit::parse_str(content) {
            Ok(report) => Ok(Some((
                analyzers::flaky::run_outcomes(&report),
                junit::report_timestamp(content),
            ))),
            Err(err) => {
                eprintln!(
                    "sooth: failed to parse JUnit-XML report `{}`: {err}",
                    path.display()
                );
                Err(())
            }
        },
    }
}

fn import(args: &cli::ImportArgs) -> ExitCode {
    let style = report::Style::resolved(args.color);
    let ledger_path = std::path::Path::new(history::IMPORTED_PATH);
    let mut seen = history::imported_hashes(ledger_path);
    let mut incoming: Vec<Incoming> = Vec::new();
    for path in &args.reports {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                eprintln!("sooth: could not read `{}`: {err}", path.display());
                return ExitCode::from(EXIT_SOOTH_ERROR);
            }
        };
        let hash = history::fnv1a64(content.as_bytes());
        if !seen.insert(hash) {
            println!("{}: skipped (already imported)", path.display());
            continue;
        }
        let Ok(parsed) = file_outcomes(&content, args.log, path) else {
            return ExitCode::from(EXIT_SOOTH_ERROR);
        };
        let Some((outcomes, content_stamp)) = parsed else {
            continue;
        };
        let at_epoch_secs = content_stamp
            .and_then(|stamp| history::iso8601_to_epoch(&stamp))
            .or_else(|| file_state(path).and_then(|(mtime, _)| epoch_secs(mtime)))
            .unwrap_or_else(history::now_epoch_secs);
        let observations: Vec<history::Observation> = outcomes
            .into_iter()
            .map(|(id, status)| history::Observation {
                id,
                status,
                commit: args.commit.clone(),
                // --commit asserts a clean checkout of that commit; without
                // it the code state is unknowable, not clean.
                dirty: args.commit.as_ref().map(|_| false),
                environment: Some(args.env.clone()),
                at_epoch_secs,
            })
            .collect();
        println!(
            "{}: {}",
            path.display(),
            report::count(observations.len(), "observation")
        );
        incoming.push(Incoming {
            at_epoch_secs,
            observations,
            hash,
            name: path.display().to_string(),
        });
    }

    // Ascending time order, so the file stays readable front to back; the
    // analysis itself no longer depends on it.
    incoming.sort_by_key(|entry| entry.at_epoch_secs);
    let observations: Vec<history::Observation> = incoming
        .iter()
        .flat_map(|entry| entry.observations.iter().cloned())
        .collect();
    let history_path = std::path::Path::new(history::HISTORY_PATH);
    if let Err(err) = history::append(history_path, &observations) {
        eprintln!("sooth: could not write {}: {err}", history_path.display());
        return ExitCode::from(EXIT_SOOTH_ERROR);
    }
    let ledger: Vec<(u64, String)> = incoming
        .iter()
        .map(|entry| (entry.hash, entry.name.clone()))
        .collect();
    if let Err(err) = history::record_imported(ledger_path, &ledger) {
        // The observations are recorded; only the dedupe bookkeeping failed.
        eprintln!(
            "sooth: could not write {}: {err} — re-importing these files will double-count",
            ledger_path.display()
        );
    }

    let loaded = load_history(history_path);
    let analysis = analyzers::history::analyze(&loaded.observations);
    report::print_history(
        &Analyses {
            history: Some(&analysis),
            ..Analyses::default()
        },
        style,
    );
    println!(
        "history now holds {}",
        report::count(loaded.observations.len(), "observation")
    );
    ExitCode::SUCCESS
}

/// `sooth history` looks at the evidence without adding to it: nothing runs,
/// so the observer principle leaves nothing to record.
fn history_command(args: &cli::HistoryArgs) -> ExitCode {
    let style = report::Style::resolved(args.color);
    let loaded = load_history(std::path::Path::new(history::HISTORY_PATH));
    if loaded.observations.is_empty() {
        println!("the history is empty — every `sooth run` and `sooth import` adds to it");
        return ExitCode::SUCCESS;
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
    let unusable = loaded
        .observations
        .iter()
        .filter(|observation| observation.dirty != Some(false))
        .count();
    report::print_history_gap(
        Some(report::PriorEvidence {
            observations: loaded.observations.len(),
            unusable,
        }),
        style,
    );
    println!(
        "history now holds {}",
        report::count(loaded.observations.len(), "observation")
    );
    ExitCode::SUCCESS
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
fn emit_output(
    args: &cli::RunArgs,
    outcomes: &[runner::RunOutcome],
    junit_summary: Option<&JunitSummary>,
    analyses: &Analyses<'_>,
    report_failures: usize,
    failed: bool,
    style: report::Style,
) -> Option<ExitCode> {
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
            if let Err(exit) = write_json_file(path, json) {
                return Some(exit);
            }
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
            report::print_runs(outcomes, style);
            println!(
                "{}",
                report::verdict_line(outcomes, None, report_failures, failed, style)
            );
        }
    }
    None
}

/// The closing verdict line, pardon-aware: a pardoned run explains why it
/// exits 0 instead of claiming a clean pass.
fn verdict(
    analyses: &Analyses<'_>,
    outcomes: &[runner::RunOutcome],
    junit_summary: Option<&JunitSummary>,
    report_failures: usize,
    failed: bool,
    style: report::Style,
) -> String {
    match analyses.pardoned {
        Some(pardoned) => report::pardoned_verdict(outcomes, pardoned.len(), style),
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
    requested_runs: u32,
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
        if let Some(context) = crash_context(outcomes, requested_runs) {
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

/// Best-effort removal of a preset's private report directory (file + dir).
fn cleanup_preset_report(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::{pardon_gap, quarantine_pardon, rejected_flag, suite_failed};
    use crate::analyzers::explain::{Explanation, Verdict};
    use crate::cli::{Cli, Command};
    use crate::junit::{JunitReport, TestCase, TestStatus};
    use crate::runner::RunOutcome;
    use clap::Parser;
    use std::collections::BTreeSet;
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

    #[test]
    fn reportless_fail_on_flaky_is_rejected() {
        let args = parse_run_args(&["sooth", "run", "--fail-on-flaky", "--", "true"]);
        let reason = rejected_flag(&args).expect("flag should be rejected");
        assert!(reason.contains("--fail-on-flaky"), "got: {reason}");
    }

    #[test]
    fn quarantine_pardons_when_every_failure_is_listed() {
        let quarantine = BTreeSet::from(["case".to_owned()]);
        let pardoned = quarantine_pardon(
            &quarantine,
            &[outcome(false), outcome(true)],
            &[report_of(TestStatus::Failed), report_of(TestStatus::Passed)],
        );
        assert_eq!(pardoned, Some(vec!["case".to_owned()]));
    }

    #[test]
    fn an_unlisted_failure_blocks_the_whole_pardon() {
        let pardoned = quarantine_pardon(
            &BTreeSet::new(),
            &[outcome(false)],
            &[report_of(TestStatus::Failed)],
        );
        assert_eq!(pardoned, None);
    }

    #[test]
    fn a_failed_run_with_a_green_report_is_never_pardoned() {
        let quarantine = BTreeSet::from(["case".to_owned()]);
        let pardoned = quarantine_pardon(
            &quarantine,
            &[outcome(false)],
            &[report_of(TestStatus::Passed)],
        );
        assert_eq!(pardoned, None);
    }

    #[test]
    fn a_pardon_needs_a_report_for_every_run() {
        let quarantine = BTreeSet::from(["case".to_owned()]);
        assert_eq!(quarantine_pardon(&quarantine, &[outcome(false)], &[]), None);
    }

    fn known_flake(id: &str, quarantined: bool) -> Explanation {
        Explanation {
            observed: None,
            id: id.to_owned(),
            verdict: Verdict::KnownFlake {
                failed_runs: 1,
                observed_runs: 4,
                failures_confined_to: None,
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

    #[test]
    fn a_signal_killed_run_is_never_pardoned() {
        let quarantine = BTreeSet::from(["case".to_owned()]);
        let killed = RunOutcome {
            exit_code: None,
            signal: Some(9),
            success: false,
            duration: Duration::from_millis(1),
        };
        assert_eq!(
            quarantine_pardon(&quarantine, &[killed], &[report_of(TestStatus::Failed)]),
            None
        );
    }
}
