//! Failure re-verification: after a failing run, re-run *only the failed
//! tests* and classify them (see `DECISIONS.md`).

use std::collections::BTreeMap;

use crate::analyzers::flaky::run_outcomes;
use crate::junit::{JunitReport, TestStatus};

/// How many times a failed test is re-run (why two: see `DECISIONS.md`).
pub const VERIFY_RUNS: u32 = 2;

/// What re-verification concluded about the run's failures.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Failed the suite and failed every verification run it appeared in.
    pub real: Vec<String>,
    /// Failed the suite but passed at least one verification run — flaky
    /// or order-dependent.
    pub flaky_or_order: Vec<String>,
    /// Failed the suite but never appeared in a verification report —
    /// reported apart, never folded into `real`.
    pub unverified: Vec<String>,
}

impl Verdict {
    pub fn is_empty(&self) -> bool {
        self.real.is_empty() && self.flaky_or_order.is_empty() && self.unverified.is_empty()
    }
}

/// Classify each originally-failed id against the verification reports.
/// `failed_ids` must already be the suite's failed identities; their order
/// is preserved within each bucket.
pub fn classify(failed_ids: &[String], verify_reports: &[JunitReport]) -> Verdict {
    let per_run: Vec<BTreeMap<String, TestStatus>> =
        verify_reports.iter().map(run_outcomes).collect();

    let mut verdict = Verdict::default();
    for id in failed_ids {
        let mut seen = false;
        let mut passed_once = false;
        for run in &per_run {
            if let Some(status) = run.get(id) {
                seen = true;
                if matches!(status, TestStatus::Passed) {
                    passed_once = true;
                }
            }
        }
        if !seen {
            verdict.unverified.push(id.clone());
        } else if passed_once {
            verdict.flaky_or_order.push(id.clone());
        } else {
            verdict.real.push(id.clone());
        }
    }
    verdict
}

/// The suite's failed identities, collapsed per report (worst status wins).
pub fn failed_ids(report: &JunitReport) -> Vec<String> {
    run_outcomes(report)
        .into_iter()
        .filter(|(_, status)| matches!(status, TestStatus::Failed | TestStatus::Error))
        .map(|(id, _)| id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{classify, failed_ids};
    use crate::junit::parse_str;

    fn report(cases: &str) -> crate::junit::JunitReport {
        parse_str(&format!("<testsuite>{cases}</testsuite>")).unwrap()
    }

    #[test]
    fn a_failure_that_passes_a_verification_run_is_flaky_or_order_dependent() {
        let failed = vec!["c::wob".to_owned()];
        let verify = [
            report(r#"<testcase classname="c" name="wob"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="wob"/>"#),
        ];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.flaky_or_order, ["c::wob"]);
        assert!(verdict.real.is_empty());
        assert!(verdict.unverified.is_empty());
    }

    #[test]
    fn a_failure_that_fails_every_verification_run_is_real() {
        let failed = vec!["c::dead".to_owned()];
        let verify = [
            report(r#"<testcase classname="c" name="dead"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="dead"><error/></testcase>"#),
        ];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.real, ["c::dead"]);
        assert!(verdict.flaky_or_order.is_empty());
    }

    #[test]
    fn a_failure_the_selection_never_re_ran_is_unverified_not_real() {
        let failed = vec!["c::missed".to_owned()];
        let verify = [report(r#"<testcase classname="c" name="other"/>"#)];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.unverified, ["c::missed"]);
        assert!(verdict.real.is_empty());
    }

    #[test]
    fn one_pass_across_runs_is_enough_to_clear_a_real_label() {
        let failed = vec!["c::t".to_owned()];
        let verify = [
            report(r#"<testcase classname="c" name="t"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="t"/>"#),
        ];
        assert_eq!(classify(&failed, &verify).flaky_or_order, ["c::t"]);
    }

    #[test]
    fn failed_ids_collapses_duplicate_rows_to_one_entry() {
        let report = report(
            r#"<testcase classname="c" name="row"/><testcase classname="c" name="row"><failure/></testcase>"#,
        );
        assert_eq!(failed_ids(&report), ["c::row"]);
    }

    #[test]
    fn failed_ids_ignores_passing_and_skipped_tests() {
        let report = report(
            r#"<testcase classname="c" name="ok"/><testcase classname="c" name="skip"><skipped/></testcase><testcase classname="c" name="bad"><failure/></testcase>"#,
        );
        assert_eq!(failed_ids(&report), ["c::bad"]);
    }
}
