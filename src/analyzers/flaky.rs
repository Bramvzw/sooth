//! Flaky detection over repeated fixed-order runs.
//!
//! A test is *flaky* iff it shows mixed outcomes across the observed runs;
//! failed-every-run is *broken*, never flaky (see `DECISIONS.md`). Skipped
//! observations carry no signal and are excluded from the rate.

use std::collections::BTreeMap;

use crate::junit::{JunitReport, TestStatus};

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
        if self.observed() == 0 {
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
            ((self.failed as f64 / self.observed() as f64) * 100.0).round() as u32
        }
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
}

impl Analysis {
    pub fn is_empty(&self) -> bool {
        self.flaky.is_empty() && self.broken.is_empty()
    }
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
    analysis
}

#[cfg(test)]
mod tests {
    use super::analyze;
    use crate::junit::parse_str;

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
        assert!(analyze(&runs).is_empty());
    }
}
