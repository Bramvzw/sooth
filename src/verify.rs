//! Failure re-verification: after a failing run, re-run *only the failed
//! tests* and classify them (see `DECISIONS.md`).

use std::collections::BTreeMap;

use crate::junit::{JunitReport, TestStatus};

/// How many times a failed test is re-run (why two: see `DECISIONS.md`).
pub const VERIFY_RUNS: u32 = 2;

/// The signature a failure carries: its kind (assertion failure vs error)
/// and the exception class from the report's `type` attribute, when the
/// runner writes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureSignature {
    pub status: TestStatus,
    pub failure_type: Option<String>,
}

impl FailureSignature {
    /// The signature as the report names it: the exception class, or the
    /// kind when the report carries no `type`.
    fn describe(&self) -> String {
        match (&self.failure_type, self.status) {
            (Some(class), _) => class.clone(),
            (None, TestStatus::Error) => "an error".to_owned(),
            (None, _) => "a failure".to_owned(),
        }
    }
}

/// Whether two failures read as the same one. Absent metadata never
/// manufactures a difference: kinds must match, classes only when both
/// sides carry one.
fn same_failure(suite: &FailureSignature, isolation: &FailureSignature) -> bool {
    suite.status == isolation.status
        && match (&suite.failure_type, &isolation.failure_type) {
            (Some(left), Some(right)) => left == right,
            _ => true,
        }
}

/// A failure whose re-run failed with a different signature: an artifact of
/// isolation, not a reproduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifferentFailure {
    pub id: String,
    /// What the suite's run saw, as `describe`d.
    pub suite: String,
    /// What the re-run saw instead.
    pub isolation: String,
}

/// What re-verification concluded about the run's failures.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Failed the suite and failed every verification run it appeared in,
    /// with the same signature.
    pub real: Vec<String>,
    /// Failed the suite but passed at least one verification run.
    pub flaky_or_order: Vec<String>,
    /// Failed every verification run, but with a different signature than
    /// the suite saw — nothing was reproduced (see `DECISIONS.md`).
    pub failed_differently: Vec<DifferentFailure>,
    /// Failed the suite but never appeared in a verification report.
    pub unverified: Vec<String>,
}

/// Classify each originally-failed test against the verification reports;
/// order is preserved within each bucket.
pub fn classify(failed: &[FailedTest], verify_reports: &[JunitReport]) -> Verdict {
    let per_run: Vec<BTreeMap<String, FailureSignature>> =
        verify_reports.iter().map(run_signatures).collect();

    let mut verdict = Verdict::default();
    for test in failed {
        let mut seen = false;
        let mut passed_once = false;
        let mut mismatch: Option<FailureSignature> = None;
        for run in &per_run {
            // A skip carries no signal: it does not count as re-run.
            match run.get(&test.id) {
                Some(signature) if signature.status == TestStatus::Passed => {
                    seen = true;
                    passed_once = true;
                }
                Some(signature)
                    if matches!(signature.status, TestStatus::Failed | TestStatus::Error) =>
                {
                    seen = true;
                    if !same_failure(&test.signature, signature) && mismatch.is_none() {
                        mismatch = Some(signature.clone());
                    }
                }
                _ => {}
            }
        }
        if !seen {
            verdict.unverified.push(test.id.clone());
        } else if passed_once {
            verdict.flaky_or_order.push(test.id.clone());
        } else if let Some(isolation) = mismatch {
            verdict.failed_differently.push(DifferentFailure {
                id: test.id.clone(),
                suite: test.signature.describe(),
                isolation: isolation.describe(),
            });
        } else {
            verdict.real.push(test.id.clone());
        }
    }
    verdict
}

/// One run's signature per test id, collapsing duplicates to the worst
/// status (the same rule as `run_outcomes`), with the `type` riding along.
fn run_signatures(report: &JunitReport) -> BTreeMap<String, FailureSignature> {
    let mut map: BTreeMap<String, FailureSignature> = BTreeMap::new();
    for case in &report.test_cases {
        let candidate = FailureSignature {
            status: case.status,
            failure_type: case.failure_type.clone(),
        };
        map.entry(case.qualified_name())
            .and_modify(|current| {
                if case.status.severity() > current.status.severity() {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    map
}

/// A failed test carrying both halves of its identity: `id` is the joined
/// `classname::name` that reports, history, and quarantine key on; `name`
/// is the raw `name` attribute selection needs. The halves travel
/// separately — a name may itself contain `::`, so the join is one-way.
#[derive(Debug, PartialEq, Eq)]
pub struct FailedTest {
    pub id: String,
    pub name: String,
    /// What the suite's run saw, for the re-run to reproduce — or not.
    pub signature: FailureSignature,
}

/// The suite's failed tests, collapsed per report (worst status wins).
pub fn failed_tests(report: &JunitReport) -> Vec<FailedTest> {
    run_signatures(report)
        .into_iter()
        .filter(|(_, signature)| matches!(signature.status, TestStatus::Failed | TestStatus::Error))
        .map(|(id, signature)| {
            let name = report
                .test_cases
                .iter()
                .find(|case| case.qualified_name() == id)
                .map_or_else(|| id.clone(), |case| case.name.clone());
            FailedTest {
                id,
                name,
                signature,
            }
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

    /// The suite's failed tests (with their signatures) out of one report.
    fn failed(cases: &str) -> Vec<super::FailedTest> {
        failed_tests(&report(cases))
    }

    #[test]
    fn a_failure_that_passes_a_verification_run_is_flaky_or_order_dependent() {
        let failed = failed(r#"<testcase classname="c" name="wob"><failure/></testcase>"#);
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
    fn a_failure_that_fails_every_verification_run_the_same_way_is_real() {
        let failed = failed(r#"<testcase classname="c" name="dead"><failure/></testcase>"#);
        let verify = [
            report(r#"<testcase classname="c" name="dead"><failure/></testcase>"#),
            report(r#"<testcase classname="c" name="dead"><failure/></testcase>"#),
        ];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.real, ["c::dead"]);
        assert!(verdict.flaky_or_order.is_empty());
        assert!(verdict.failed_differently.is_empty());
    }

    #[test]
    fn a_re_run_that_fails_with_a_different_kind_is_not_a_reproduction() {
        // The dogfooded case: an assertion in the suite, a bootstrap error
        // in isolation — nothing was reproduced.
        let failed = failed(r#"<testcase classname="c" name="boot"><failure/></testcase>"#);
        let verify = [
            report(r#"<testcase classname="c" name="boot"><error/></testcase>"#),
            report(r#"<testcase classname="c" name="boot"><error/></testcase>"#),
        ];
        let verdict = classify(&failed, &verify);
        assert!(verdict.real.is_empty(), "a different failure is no proof");
        assert_eq!(verdict.failed_differently.len(), 1);
        let different = &verdict.failed_differently[0];
        assert_eq!(different.id, "c::boot");
        assert_eq!(different.suite, "a failure");
        assert_eq!(different.isolation, "an error");
    }

    #[test]
    fn a_re_run_that_fails_with_a_different_class_is_not_a_reproduction() {
        let failed =
            failed(r#"<testcase classname="c" name="t"><error type="TypeError"/></testcase>"#);
        let verify = [report(
            r#"<testcase classname="c" name="t"><error type="RuntimeException"/></testcase>"#,
        )];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.failed_differently.len(), 1);
        assert_eq!(verdict.failed_differently[0].suite, "TypeError");
        assert_eq!(verdict.failed_differently[0].isolation, "RuntimeException");
    }

    #[test]
    fn a_missing_type_on_either_side_never_manufactures_a_difference() {
        // Same kind, one side without a type attribute: real, as before —
        // absent metadata is not evidence of anything.
        let failed = failed(
            r#"<testcase classname="c" name="t"><failure type="AssertionError"/></testcase>"#,
        );
        let verify = [report(
            r#"<testcase classname="c" name="t"><failure/></testcase>"#,
        )];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.real, ["c::t"]);
        assert!(verdict.failed_differently.is_empty());
    }

    #[test]
    fn a_pass_on_re_run_outranks_a_differing_failure() {
        // Passed once and failed differently once: the pass carries the
        // stronger signal — flaky or order-dependent, as before.
        let failed = failed(r#"<testcase classname="c" name="t"><failure/></testcase>"#);
        let verify = [
            report(r#"<testcase classname="c" name="t"><error/></testcase>"#),
            report(r#"<testcase classname="c" name="t"/>"#),
        ];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.flaky_or_order, ["c::t"]);
        assert!(verdict.failed_differently.is_empty());
    }

    #[test]
    fn a_failure_the_selection_never_re_ran_is_unverified_not_real() {
        let failed = failed(r#"<testcase classname="c" name="missed"><failure/></testcase>"#);
        let verify = [report(r#"<testcase classname="c" name="other"/>"#)];
        let verdict = classify(&failed, &verify);
        assert_eq!(verdict.unverified, ["c::missed"]);
        assert!(verdict.real.is_empty());
    }

    #[test]
    fn a_failure_that_is_only_skipped_on_re_run_is_unverified_not_real() {
        let failed = failed(r#"<testcase classname="c" name="skippy"><failure/></testcase>"#);
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
        let failed = failed(r#"<testcase classname="c" name="t"><failure/></testcase>"#);
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
