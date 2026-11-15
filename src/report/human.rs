//! The printed report: every line of it that a person reads.

#![expect(
    clippy::print_stdout,
    reason = "The printed report is the one place that prints it (AGENTS.md)."
)]

use std::borrow::Cow;
use std::time::Duration;

use super::{
    count, Analyses, ImportFate, ImportedFile, Interrupted, JunitSummary, Replay, RunReport, Style,
};
use crate::analyzers::history::PriorEvidence;
use crate::analyzers::{explain, flaky, history};
use crate::gate;
use crate::runner::RunOutcome;

/// One line per run. The runner's own exit code is labeled `runner exit=` on
/// purpose: a bare `exit=2` reads as sooth's own exit-code contract, where 2
/// means "sooth itself failed" — two vocabularies that must stay distinct.
pub fn print_runs(outcomes: &[RunOutcome], requested: usize, style: Style) {
    for (index, outcome) in outcomes.iter().enumerate() {
        print_run(index + 1, requested, outcome, style);
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

/// The gate found no test file to prove anything about.
pub fn print_empty_gate(selection: &gate::Selection) {
    println!(
        "gate: no new or changed tests against {} — nothing to prove",
        selection.base
    );
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

/// The closing line when the runs were cut short. No verdict: the runs that
/// were asked for did not all happen, so the suite was neither proven green
/// nor seen through to red. It says what did happen: how many of the
/// `requested` runs finished — `outcomes` — and how many of those `failed`.
/// Not during which run: a request to stop can also land between two.
pub fn interrupted_line(
    outcomes: &[RunOutcome],
    requested: usize,
    failed: usize,
    style: Style,
) -> String {
    let total: Duration = outcomes.iter().map(|outcome| outcome.duration).sum();
    let finished = outcomes.len();
    let how = match (finished, failed) {
        (1, 0) => "it passed".to_owned(),
        (1, _) => "it failed".to_owned(),
        (_, 0) => "none of them failed".to_owned(),
        (_, failed) => format!("{failed} of them failed"),
    };
    style.yellow(&format!(
        "result: interrupted after {finished} of {} — {how} ({total:.2?} total)",
        count(requested, "run")
    ))
}

/// The closing line of a run.
pub fn print_verdict(ended: &RunReport<'_>, style: Style) {
    // The verdict below is the suite's own and may read PASSED under a
    // pardon; next to an exit of 2 that needs saying where the verdict is.
    if ended.analyses.interrupted == Some(Interrupted::Verification) {
        println!(
            "{}",
            style.yellow(
                "note: interrupted before the failures were verified — they stay unverified"
            )
        );
    }
    println!("{}", verdict(ended, style));
}

/// The closing verdict line, pardon-aware: a pardoned run explains why it
/// exits 0 instead of claiming a clean pass.
fn verdict(ended: &RunReport<'_>, style: Style) -> String {
    let RunReport {
        outcomes, analyses, ..
    } = *ended;
    // Cut short, the runs carry no verdict — pardoned or not. A suite that
    // ran to its end has one, verified or not.
    if let Some(Interrupted::Runs { requested }) = analyses.interrupted {
        return interrupted_line(outcomes, requested, ended.failed_runs, style);
    }
    match analyses.pardoned {
        Some(pardoned) => {
            let not_flakes = analyses
                .explanation
                .unwrap_or_default()
                .iter()
                .filter(|explanation| explanation.listed_but_not_a_flake())
                .count();
            pardoned_verdict(outcomes, pardoned.len(), not_flakes, style)
        }
        None => verdict_line(ended, style),
    }
}

/// The closing verdict line: sooth's suite-level judgement at a glance.
fn verdict_line(ended: &RunReport<'_>, style: Style) -> String {
    let RunReport {
        outcomes,
        summary,
        report_failures,
        failed_runs,
        ..
    } = *ended;
    let total: Duration = outcomes.iter().map(|outcome| outcome.duration).sum();
    let runs = outcomes.len();
    if ended.failed {
        // A run that exited 0 over a red report failed like any other: the
        // count is the one the lines above were made with.
        let detail = if outcomes.iter().any(|outcome| !outcome.success) {
            format!("{failed_runs} of {runs} runs failed")
        } else {
            // Every run claimed success and a report disagrees; the stderr
            // note carries the full story.
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
    if !consulted && explanation.category() == explain::Category::New {
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
        // Which of the two an unknown failure is, is the category's rule.
        explain::Verdict::Unknown => match explanation.category() {
            explain::Category::Quarantined => style.yellow(&format!(
                "quarantined (listed in {})",
                crate::quarantine::FILE_NAME
            )),
            _ => style.bold_red("new (nothing in history)"),
        },
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

/// `sooth explain` on a report without failures.
pub fn print_nothing_to_explain(report: &std::path::Path) {
    println!("no failures in `{}` — nothing to explain", report.display());
}

/// `sooth history` before anything was recorded.
pub fn print_empty_history() {
    println!("the history is empty — every `sooth run` and `sooth import` adds to it");
}

/// `sooth history` over evidence that proves nothing yet.
pub fn print_nothing_stands_out() {
    println!("nothing stands out: no proven flakes, no failing-since pointers");
}

/// What became of one imported file, in one line.
pub fn print_imported(file: &ImportedFile<'_>) {
    let path = file.path.display();
    match file.fate {
        ImportFate::SameContent => {
            println!("{path}: skipped (same content as an earlier file of this import)");
        }
        ImportFate::AlreadyImported => println!(
            "{path}: skipped (already imported) — pass `--replace` to import it again \
             in place of its earlier observations"
        ),
        ImportFate::NothingToRecord => {
            println!("{path}: no failures to record — a green log names no tests");
        }
        ImportFate::Recorded {
            observations,
            replaced,
        } => println!(
            "{path}: {}{}",
            count(observations, "observation"),
            if replaced {
                ", replacing its earlier import"
            } else {
                ""
            }
        ),
    }
}

/// How much history the analysis above rests on. A file larger than one
/// load reads is said as such: the count is then the newest part, not all.
pub fn print_history_size(observations: usize, truncated: bool) {
    let observations = count(observations, "observation");
    if truncated {
        println!(
            "history: the newest {observations} were read; the file holds more than sooth \
             loads at once, so older ones are out of reach — `sooth history --prune` \
             drops what no analysis reads"
        );
    } else {
        println!("history now holds {observations}");
    }
}

/// The size line right after a prune of a history that is still larger
/// than one load: the advice to prune would send its reader in a circle.
pub fn print_history_beyond_one_load(observations: usize) {
    println!(
        "history: the newest {} were read. The file is pruned and still holds more than \
         sooth loads at once: for a suite this size, fewer than each test's last {} \
         invocations are read",
        count(observations, "observation"),
        crate::history::WINDOW_PER_TEST
    );
}

/// What `sooth history --prune` did, in one line.
pub fn print_pruned(pruned: crate::history::Pruned) {
    if pruned.removed == 0 {
        println!(
            "pruned: nothing to remove — the history holds nothing older than each test's \
             last {} invocations",
            crate::history::WINDOW_PER_TEST
        );
        return;
    }
    println!(
        "pruned: removed {} older than each test's last {} invocations, kept {} ({} → {})",
        count(pruned.removed, "observation"),
        crate::history::WINDOW_PER_TEST,
        pruned.kept,
        human_bytes(pruned.bytes_before),
        human_bytes(pruned.bytes_after)
    );
}

/// A file size for a sentence: `812 B`, `34.2 kB`, `1.3 GB`.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let mut unit = 0;
    let mut tenths = bytes.saturating_mul(10);
    while tenths >= 10_000 && unit + 1 < UNITS.len() {
        tenths /= 1000;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::{outcome, plain, test_case};
    use super::verdict_line;
    use crate::analyzers::explain;
    use crate::junit::{JunitReport, TestStatus};
    use crate::report::JunitSummary;

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

    /// The verdict line of runs that ended as given: `failed` of them
    /// failed, and the worst report showed `report_failures` failing tests.
    fn verdict_of(
        outcomes: &[crate::runner::RunOutcome],
        summary: Option<&JunitSummary>,
        (failed, report_failures): (usize, usize),
    ) -> String {
        let ended = crate::report::RunReport {
            outcomes,
            summary,
            analyses: &crate::report::Analyses::default(),
            report_failures,
            failed_runs: failed,
            failed: failed > 0,
        };
        verdict_line(&ended, plain())
    }

    #[test]
    fn the_verdict_names_failed_runs() {
        let line = verdict_of(&[outcome(true), outcome(false)], None, (1, 0));
        assert!(line.contains("FAILED"));
        assert!(line.contains("1 of 2 runs failed"));
    }

    #[test]
    fn the_verdict_counts_a_run_that_is_red_by_its_report_alone() {
        // Three runs, one of them failing by its exit and one by its
        // report only: two failed, and the line must not say one.
        let runs = [outcome(true), outcome(false), outcome(true)];
        let line = verdict_of(&runs, None, (2, 1));
        assert!(line.contains("2 of 3 runs failed"), "got: {line}");
    }

    #[test]
    fn the_verdict_blames_the_report_when_runs_were_green() {
        let report = JunitReport {
            test_cases: vec![test_case("bad", TestStatus::Failed, 0.1)],
        };
        let summary = JunitSummary::from_report(&report, 0);
        let line = verdict_of(&[outcome(true)], Some(&summary), (1, 1));
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
        let line = verdict_of(&[outcome(true)], Some(&summary), (0, 0));
        assert!(line.contains("PASSED"));
        assert!(line.contains("1 of 1 runs, 1 test ("));
    }

    #[test]
    fn a_zero_test_pass_says_it_proved_nothing() {
        let line = verdict_of(&[outcome(true)], Some(&JunitSummary::empty()), (0, 0));
        assert!(line.contains("✓ PASSED"), "the exit contract holds: {line}");
        assert!(
            line.contains("0 tests, so this run proved nothing"),
            "got: {line}"
        );
    }

    #[test]
    fn an_interrupted_run_says_how_many_runs_finished_and_how_they_went() {
        use super::interrupted_line;
        let two = [outcome(true), outcome(true)];
        assert!(interrupted_line(&two, 5, 0, plain())
            .starts_with("result: interrupted after 2 of 5 runs — none of them failed ("));
        // Failed is the caller's count: a run can be red by its report alone.
        assert!(interrupted_line(&two, 5, 1, plain())
            .starts_with("result: interrupted after 2 of 5 runs — 1 of them failed ("));
        let one = [outcome(true)];
        assert!(interrupted_line(&one, 3, 0, plain())
            .starts_with("result: interrupted after 1 of 3 runs — it passed ("));
        assert!(interrupted_line(&one, 3, 1, plain())
            .starts_with("result: interrupted after 1 of 3 runs — it failed ("));
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
    fn a_file_size_reads_in_the_unit_that_fits() {
        use super::human_bytes;
        assert_eq!(human_bytes(812), "812 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_000), "1.0 kB");
        assert_eq!(human_bytes(34_250), "34.2 kB");
        assert_eq!(human_bytes(67_100_000), "67.1 MB");
        assert_eq!(human_bytes(13_000_000_000), "13.0 GB");
        assert_eq!(human_bytes(5_000_000_000_000), "5000.0 GB");
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
}
