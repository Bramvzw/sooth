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
    /// Failed the suite but passed at least one verification run.
    pub flaky_or_order: Vec<String>,
    /// Failed the suite but never appeared in a verification report.
    pub unverified: Vec<String>,
}

impl Verdict {
    pub fn is_empty(&self) -> bool {
        self.real.is_empty() && self.flaky_or_order.is_empty() && self.unverified.is_empty()
    }
}

/// Classify each originally-failed id against the verification reports;
/// order is preserved within each bucket.
pub fn classify(failed_ids: &[String], verify_reports: &[JunitReport]) -> Verdict {
    let per_run: Vec<BTreeMap<String, TestStatus>> =
        verify_reports.iter().map(run_outcomes).collect();

    let mut verdict = Verdict::default();
    for id in failed_ids {
        let mut seen = false;
        let mut passed_once = false;
        for run in &per_run {
            // A skip carries no signal: it does not count as re-run.
            match run.get(id) {
                Some(TestStatus::Passed) => {
                    seen = true;
                    passed_once = true;
                }
                Some(TestStatus::Failed | TestStatus::Error) => seen = true,
                Some(TestStatus::Skipped) | None => {}
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

/// A failed test carrying both halves of its identity: `id` is the joined
/// `classname::name` that reports, history, and quarantine key on; `name`
/// is the raw `name` attribute selection needs. The halves travel
/// separately — a name may itself contain `::`, so the join is one-way.
#[derive(Debug, PartialEq, Eq)]
pub struct FailedTest {
    pub id: String,
    pub name: String,
}

/// The suite's failed tests, collapsed per report (worst status wins).
pub fn failed_tests(report: &JunitReport) -> Vec<FailedTest> {
    run_outcomes(report)
        .into_iter()
        .filter(|(_, status)| matches!(status, TestStatus::Failed | TestStatus::Error))
        .map(|(id, _)| {
            let name = report
                .test_cases
                .iter()
                .find(|case| case.qualified_name() == id)
                .map_or_else(|| id.clone(), |case| case.name.clone());
            FailedTest { id, name }
        })
        .collect()
}

/// The suite's failed identities, collapsed per report (worst status wins).
pub fn failed_ids(report: &JunitReport) -> Vec<String> {
    failed_tests(report)
        .into_iter()
        .map(|test| test.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{classify, failed_ids, failed_tests};
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
    fn a_failure_that_is_only_skipped_on_re_run_is_unverified_not_real() {
        let failed = vec!["c::skippy".to_owned()];
        let verify = [
            report(r#"<testcase classname="c" name="skippy"><skipped/></testcase>"#),
            report(r#"<testcase classname="c" name="skippy"><skipped/></testcase>"#),
        ];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.unverified, ["c::skippy"]);
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
    fn failed_tests_carry_the_raw_name_even_when_it_contains_double_colons() {
        // A name may itself contain `::`: the raw name must survive whole,
        // never as a re-split tail of the joined id (#91).
        let report = report(
            r#"<testcase classname="config" name="Config::load reads the env"><failure/></testcase>"#,
        );
        let tests = failed_tests(&report);
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].id, "config::Config::load reads the env");
        assert_eq!(tests[0].name, "Config::load reads the env");
    }

    #[test]
    fn failed_tests_keep_a_bare_name_with_double_colons_whole() {
        // No classname: the id IS the name, and any split would be wrong.
        let report = report(r#"<testcase name="a::b"><failure/></testcase>"#);
        let tests = failed_tests(&report);
        assert_eq!(tests[0].id, "a::b");
        assert_eq!(tests[0].name, "a::b");
    }

    #[test]
    fn failed_ids_ignores_passing_and_skipped_tests() {
        let report = report(
            r#"<testcase classname="c" name="ok"/><testcase classname="c" name="skip"><skipped/></testcase><testcase classname="c" name="bad"><failure/></testcase>"#,
        );
        assert_eq!(failed_ids(&report), ["c::bad"]);
    }
}
