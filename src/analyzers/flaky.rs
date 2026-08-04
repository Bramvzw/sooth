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

/// The outcome of the flaky pass: what is flaky, and what is simply broken.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Mixed outcomes — the actual flakes, sorted by failure rate (highest
    /// first), then by id for a stable order.
    pub flaky: Vec<TestOutcomes>,
    /// Failed every observed run — broken, not flaky.
    pub broken: Vec<TestOutcomes>,
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

/// Aggregate per-test outcomes across the runs' reports and split them into
/// flaky (mixed) and broken (always failing). Tests that passed every run —
/// the healthy majority — are not reported at all.
pub fn analyze(reports: &[JunitReport]) -> Analysis {
    let mut by_test: BTreeMap<String, TestOutcomes> = BTreeMap::new();
    for report in reports {
        for (id, status) in run_outcomes(report) {
            let entry = by_test.entry(id.clone()).or_insert_with(|| TestOutcomes {
                id,
                passed: 0,
                failed: 0,
            });
            match status {
                TestStatus::Passed => entry.passed += 1,
                TestStatus::Failed | TestStatus::Error => entry.failed += 1,
                TestStatus::Skipped => {}
            }
        }
    }

    let mut analysis = Analysis::default();
    for outcomes in by_test.into_values() {
        if outcomes.failed == 0 || outcomes.observed() == 0 {
            continue;
        }
        if outcomes.passed > 0 {
            analysis.flaky.push(outcomes);
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
        let runs = [
            report(
                r#"<testcase classname="c" name="often"/><testcase classname="c" name="rare"/>"#,
            ),
            report(
                r#"<testcase classname="c" name="often"><failure/></testcase><testcase classname="c" name="rare"/>"#,
            ),
            report(
                r#"<testcase classname="c" name="often"><failure/></testcase><testcase classname="c" name="rare"><failure/></testcase>"#,
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
