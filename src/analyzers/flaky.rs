//! Flaky detection over repeated fixed-order runs.
//!
//! A test is *flaky* iff it shows mixed outcomes across the observed runs;
//! failed-every-run is *broken*, never flaky (see `DECISIONS.md`). Skipped
//! observations carry no signal and are excluded from the rate.

use std::collections::{BTreeMap, BTreeSet};

use crate::junit::{JunitReport, TestCase, TestStatus};

/// One test's aggregated outcomes across the observed runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestOutcomes {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    /// Runs in which the test passed.
    pub passed: usize,
    /// Runs in which the test failed or errored.
    pub failed: usize,
}

impl TestOutcomes {
    /// Runs that carried signal (passed or failed; skips are excluded).
    pub fn observed(&self) -> usize {
        self.passed + self.failed
    }

    /// Failure rate over the observed runs, in percent (rounded).
    pub fn failure_rate_percent(&self) -> u32 {
        failure_rate_percent(self.failed, self.observed())
    }
}

/// Failed over observed, in percent (rounded) — the one rate formula every
/// pass reports with.
pub fn failure_rate_percent(failed: usize, observed: usize) -> u32 {
    if observed == 0 {
        return 0;
    }
    // Percent of at most 100 always fits u32; precision loss over usize
    // counts of realistic run counts is not a concern.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    {
        ((failed as f64 / observed as f64) * 100.0).round() as u32
    }
}

/// A mixed-outcome test whose sequence flipped exactly once and never came
/// back: the suite's environment changed under the repeat, which is a
/// different finding than flakiness (see `DECISIONS.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonotoneFlip {
    pub outcomes: TestOutcomes,
    /// The 1-based run of the last observation in the opening state.
    pub flipped_after_run: usize,
    /// Whether the opening state was green (green→red) or red (red→green).
    pub started_green: bool,
}

/// A failing test observed in exactly one run while absent from others:
/// neither "flaky" nor "broken" can honestly be claimed from one sighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoneFailure {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    /// Runs of this invocation in which the id never appeared at all.
    pub absent_runs: usize,
}

/// The outcome of the flaky pass: what is flaky, what merely flipped once,
/// what was seen only once, and what is simply broken.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Mixed outcomes that bounced — the actual flakes, sorted by failure
    /// rate (highest first), then by id for a stable order.
    pub flaky: Vec<TestOutcomes>,
    /// Failed every observed run — broken, not flaky.
    pub broken: Vec<TestOutcomes>,
    /// Mixed outcomes that flipped once and never returned (the exact rule
    /// lives on `monotone_flip`).
    pub monotone: Vec<MonotoneFlip>,
    /// Failed the single run they appeared in, absent from the rest.
    pub lone_failures: Vec<LoneFailure>,
    /// Runs (1-based) that reported their shared tests in a different order
    /// than run 1, which is the precondition this pass assumes but cannot
    /// enforce (see `DECISIONS.md`).
    pub reordered_runs: Vec<usize>,
}

/// One run's outcome per test id. Duplicate ids within one report
/// (data-provider rows, retry reporters) collapse to the worst status and
/// count once — mixed duplicates in a single run must never read as
/// flakiness. This collapse is also the shape history records per run.
pub(crate) fn run_outcomes(report: &JunitReport) -> BTreeMap<String, TestStatus> {
    let mut run_outcome: BTreeMap<String, TestStatus> = BTreeMap::new();
    for case in &report.test_cases {
        run_outcome
            .entry(case.qualified_name())
            .and_modify(|status| {
                if case.status.severity() > status.severity() {
                    *status = case.status;
                }
            })
            .or_insert(case.status);
    }
    run_outcome
}

/// Aggregate per-test outcomes across the runs' reports and split them by
/// what the *sequence* supports: flaky (bounced), monotone (flipped once,
/// never back), lone (one sighting), or broken (failed every observed run).
/// Tests that passed every run — the healthy majority — are not reported.
pub fn analyze(reports: &[JunitReport]) -> Analysis {
    let per_run: Vec<BTreeMap<String, TestStatus>> = reports.iter().map(run_outcomes).collect();
    let ids: BTreeSet<String> = per_run.iter().flat_map(|run| run.keys().cloned()).collect();

    let mut analysis = Analysis::default();
    for id in ids {
        // The signal-bearing observations, in run order (skips carry none).
        let observations: Vec<(usize, bool)> = per_run
            .iter()
            .enumerate()
            .filter_map(|(index, run)| match run.get(&id) {
                Some(TestStatus::Passed) => Some((index + 1, false)),
                Some(TestStatus::Failed | TestStatus::Error) => Some((index + 1, true)),
                Some(TestStatus::Skipped) | None => None,
            })
            .collect();
        let failed = observations.iter().filter(|(_, failed)| *failed).count();
        let passed = observations.len() - failed;
        if failed == 0 {
            continue;
        }
        let outcomes = TestOutcomes {
            id: id.clone(),
            passed,
            failed,
        };
        let absent_runs = per_run.iter().filter(|run| !run.contains_key(&id)).count();
        if passed > 0 {
            match monotone_flip(&observations) {
                Some((flipped_after_run, started_green)) => {
                    analysis.monotone.push(MonotoneFlip {
                        outcomes,
                        flipped_after_run,
                        started_green,
                    });
                }
                None => analysis.flaky.push(outcomes),
            }
        } else if observations.len() == 1 && absent_runs > 0 {
            // Present-but-skipped elsewhere is a stable name and stays
            // broken; only true absence questions the identity.
            analysis.lone_failures.push(LoneFailure { id, absent_runs });
        } else {
            analysis.broken.push(outcomes);
        }
    }
    analysis.flaky.sort_by(|a, b| {
        b.failure_rate_percent()
            .cmp(&a.failure_rate_percent())
            .then(a.id.cmp(&b.id))
    });
    analysis.reordered_runs = reordered_runs(reports);
    analysis
}

/// The run after which a mixed sequence flipped for good, when it did:
/// exactly one direction change, and the new state seen at least twice
/// (see `DECISIONS.md`).
fn monotone_flip(observations: &[(usize, bool)]) -> Option<(usize, bool)> {
    let mut change_at = None;
    for (index, pair) in observations.windows(2).enumerate() {
        if pair[0].1 != pair[1].1 {
            if change_at.is_some() {
                return None; // it came back: a real flake
            }
            change_at = Some(index);
        }
    }
    let index = change_at?;
    (observations.len() - index > 2).then(|| (observations[index].0, !observations[0].1))
}

/// The ids a run reported, in report order, first occurrence only.
fn execution_order(report: &JunitReport) -> Vec<String> {
    let mut seen = BTreeSet::new();
    report
        .test_cases
        .iter()
        .map(TestCase::qualified_name)
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// Runs whose shared tests appear in a different relative order than run 1.
/// Only the shared subset is compared, so a run cut short (`--stop-on-failure`)
/// is not mistaken for a reordering; fewer than two shared tests carry no
/// order signal at all.
fn reordered_runs(reports: &[JunitReport]) -> Vec<usize> {
    let Some(first) = reports.first().map(execution_order) else {
        return Vec::new();
    };
    let mut reordered = Vec::new();
    for (index, report) in reports.iter().enumerate().skip(1) {
        let other = execution_order(report);
        let shared: BTreeSet<&String> = first
            .iter()
            .collect::<BTreeSet<&String>>()
            .intersection(&other.iter().collect())
            .copied()
            .collect();
        if shared.len() < 2 {
            continue;
        }
        let keep = |ids: &[String]| -> Vec<String> {
            ids.iter()
                .filter(|id| shared.contains(id))
                .cloned()
                .collect()
        };
        if keep(&first) != keep(&other) {
            reordered.push(index + 1);
        }
    }
    reordered
}

#[cfg(test)]
mod tests {
    use super::analyze;
    use crate::junit::parse_str;
    use std::fmt::Write as _;

    fn report(cases: &str) -> crate::junit::JunitReport {
        parse_str(&format!("<testsuite>{cases}</testsuite>")).unwrap()
    }

    #[test]
    fn mixed_outcomes_are_flaky_and_ranked_by_failure_rate() {
        // Both sequences bounce (R G R and G R G): flakes, not flips.
        let runs = [
            report(
                r#"<testcase classname="c" name="often"><failure/></testcase><testcase classname="c" name="rare"/>"#,
            ),
            report(
                r#"<testcase classname="c" name="often"/><testcase classname="c" name="rare"><failure/></testcase>"#,
            ),
            report(
                r#"<testcase classname="c" name="often"><failure/></testcase><testcase classname="c" name="rare"/>"#,
            ),
        ];
        let analysis = analyze(&runs);
        assert_eq!(analysis.flaky.len(), 2);
        assert_eq!(analysis.flaky[0].id, "c::often");
        assert_eq!(analysis.flaky[0].failure_rate_percent(), 67);
        assert_eq!(analysis.flaky[1].id, "c::rare");
        assert_eq!(analysis.flaky[1].failure_rate_percent(), 33);
        assert!(analysis.broken.is_empty());
    }

    /// Two tests in the order given, both passing — order is the only variable.
    fn ordered(ids: &[&str]) -> crate::junit::JunitReport {
        let mut cases = String::new();
        for id in ids {
            let _ = write!(cases, r#"<testcase classname="c" name="{id}"/>"#);
        }
        report(&cases)
    }

    #[test]
    fn runs_in_one_order_report_no_reordering() {
        let runs = [ordered(&["a", "b", "c"]), ordered(&["a", "b", "c"])];
        assert!(analyze(&runs).reordered_runs.is_empty());
    }

    #[test]
    fn a_swapped_run_is_named_by_its_number() {
        let runs = [ordered(&["a", "b", "c"]), ordered(&["c", "b", "a"])];
        assert_eq!(analyze(&runs).reordered_runs, [2]);
    }

    #[test]
    fn only_the_runs_that_differ_are_named() {
        let runs = [
            ordered(&["a", "b"]),
            ordered(&["a", "b"]),
            ordered(&["b", "a"]),
        ];
        assert_eq!(analyze(&runs).reordered_runs, [3]);
    }

    #[test]
    fn a_run_that_stopped_early_is_not_a_reordering() {
        // --stop-on-failure leaves fewer tests, not a different order: only
        // the shared subset is compared.
        let runs = [ordered(&["a", "b", "c", "d"]), ordered(&["a", "b"])];
        assert!(analyze(&runs).reordered_runs.is_empty());

        // Shorter *and* swapped is still a reordering.
        let swapped = [ordered(&["a", "b", "c", "d"]), ordered(&["b", "a"])];
        assert_eq!(analyze(&swapped).reordered_runs, [2]);
    }

    #[test]
    fn fewer_than_two_shared_tests_carry_no_order_signal() {
        let runs = [ordered(&["a", "b"]), ordered(&["a", "z"])];
        assert!(analyze(&runs).reordered_runs.is_empty());
    }

    #[test]
    fn duplicate_ids_are_judged_on_their_first_appearance() {
        // A data-provider row repeats an id; the repeat must not read as a
        // move relative to a run that lists it once.
        let repeated = report(
            r#"<testcase classname="c" name="a"/><testcase classname="c" name="b"/><testcase classname="c" name="a"/>"#,
        );
        let runs = [repeated, ordered(&["a", "b"])];
        assert!(analyze(&runs).reordered_runs.is_empty());
    }

    #[test]
    fn a_single_run_can_never_be_reordered() {
        assert!(analyze(&[ordered(&["a", "b"])]).reordered_runs.is_empty());
    }

    #[test]
    fn a_sequence_that_flips_once_and_stays_is_monotone_not_flaky() {
        // The #113 shape: green, then red for the rest of the invocation —
        // the suite changed under the repeat; the test itself is predictable.
        let runs = [
            report(r#"<testcase classname="c" name="polluted"/>"#),
            report(r#"<testcase classname="c" name="polluted"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="polluted"><failure/></testcase>"#),
        ];
        let analysis = analyze(&runs);
        assert!(analysis.flaky.is_empty(), "a flip is not a bounce");
        assert_eq!(analysis.monotone.len(), 1);
        let flip = &analysis.monotone[0];
        assert_eq!(flip.outcomes.id, "c::polluted");
        assert_eq!(flip.flipped_after_run, 1);
        assert!(flip.started_green);
    }

    #[test]
    fn the_reverse_flip_is_monotone_too() {
        let runs = [
            report(r#"<testcase classname="c" name="healed"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="healed"/>"#),
            report(r#"<testcase classname="c" name="healed"/>"#),
        ];
        let analysis = analyze(&runs);
        assert_eq!(analysis.monotone.len(), 1);
        let flip = &analysis.monotone[0];
        assert_eq!(flip.flipped_after_run, 1);
        assert!(!flip.started_green);
    }

    #[test]
    fn a_flip_only_at_the_final_run_is_still_flaky() {
        // "Never back" needs a chance to have come back: a single trailing
        // red is indistinguishable from an ordinary flake.
        let runs = [
            report(r#"<testcase classname="c" name="wob"/>"#),
            report(r#"<testcase classname="c" name="wob"/>"#),
            report(r#"<testcase classname="c" name="wob"><failure/></testcase>"#),
        ];
        let analysis = analyze(&runs);
        assert!(analysis.monotone.is_empty());
        assert_eq!(analysis.flaky.len(), 1);
    }

    #[test]
    fn a_sequence_that_bounces_back_stays_flaky() {
        let runs = [
            report(r#"<testcase classname="c" name="wob"/>"#),
            report(r#"<testcase classname="c" name="wob"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="wob"/>"#),
        ];
        let analysis = analyze(&runs);
        assert!(analysis.monotone.is_empty(), "it came back: a real flake");
        assert_eq!(analysis.flaky.len(), 1);
    }

    #[test]
    fn two_observations_cannot_tell_a_flip_from_a_flake() {
        let runs = [
            report(r#"<testcase classname="c" name="wob"/>"#),
            report(r#"<testcase classname="c" name="wob"><failure/></testcase>"#),
        ];
        let analysis = analyze(&runs);
        assert!(analysis.monotone.is_empty());
        assert_eq!(analysis.flaky.len(), 1);
    }

    #[test]
    fn a_failure_seen_in_only_one_of_several_runs_is_lone_not_broken() {
        // The #137 shape: a name that changes per run exists in one report
        // and is absent from the rest — one sighting proves no "broken".
        let runs = [
            report(r#"<testcase classname="c" name="drift-1"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="drift-2"/>"#),
            report(r#"<testcase classname="c" name="drift-3"/>"#),
        ];
        let analysis = analyze(&runs);
        assert!(analysis.broken.is_empty(), "one sighting is not broken");
        assert_eq!(analysis.lone_failures.len(), 1);
        assert_eq!(analysis.lone_failures[0].id, "c::drift-1");
        assert_eq!(analysis.lone_failures[0].absent_runs, 2);
    }

    #[test]
    fn a_single_failed_observation_that_was_skipped_elsewhere_stays_broken() {
        // Present-but-skipped is a stable name: nothing questions the
        // identity, so the broken claim (1 observed run) stands.
        let runs = [
            report(r#"<testcase classname="c" name="s"><skipped/></testcase>"#),
            report(r#"<testcase classname="c" name="s"><failure/></testcase>"#),
        ];
        let analysis = analyze(&runs);
        assert!(analysis.lone_failures.is_empty());
        assert_eq!(analysis.broken.len(), 1);
    }

    #[test]
    fn always_failing_is_broken_not_flaky() {
        let runs = [
            report(r#"<testcase classname="c" name="dead"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="dead"><error/></testcase>"#),
        ];
        let analysis = analyze(&runs);
        assert!(analysis.flaky.is_empty());
        assert_eq!(analysis.broken.len(), 1);
        assert_eq!(analysis.broken[0].id, "c::dead");
    }

    #[test]
    fn skips_carry_no_signal() {
        let runs = [
            report(r#"<testcase classname="c" name="s"><skipped/></testcase>"#),
            report(r#"<testcase classname="c" name="s"><failure/></testcase>"#),
        ];
        let analysis = analyze(&runs);
        // one failure, zero passes among observed runs: broken, rate on 1 run
        assert_eq!(analysis.broken.len(), 1);
        assert_eq!(analysis.broken[0].observed(), 1);
    }

    #[test]
    fn duplicate_ids_within_one_report_do_not_fake_flakiness() {
        // A data provider whose row B always fails, rows sharing one name:
        // deterministic, so this must be broken — never flaky — and each
        // run counts once, not once per row.
        let row_mix = r#"<testcase classname="c" name="row"/><testcase classname="c" name="row"><failure/></testcase>"#;
        let runs = [report(row_mix), report(row_mix)];
        let analysis = analyze(&runs);
        assert!(
            analysis.flaky.is_empty(),
            "deterministic failure got called flaky"
        );
        assert_eq!(analysis.broken.len(), 1);
        assert_eq!(analysis.broken[0].observed(), 2);
    }

    #[test]
    fn all_green_reports_nothing() {
        let runs = [
            report(r#"<testcase classname="c" name="ok"/>"#),
            report(r#"<testcase classname="c" name="ok"/>"#),
        ];
        assert_eq!(analyze(&runs), super::Analysis::default());
    }
}
