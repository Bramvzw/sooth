//! The order pass: what changes when the same tests run in another order.
//!
//! Its own pass, never mixed with the flaky one (see `DECISIONS.md`): the
//! flaky pass repeats one order, this one compares two. A test whose outcome
//! differs between them is only a candidate — a flaky test differs too — so
//! both orders are run a second time, and only a test that does the same
//! thing twice in each order, and a different thing in the other, is called
//! order-dependent. That is still a sample of two orders, not a proof over
//! all of them: a dependency both orders happen to satisfy stays invisible.

use std::collections::{BTreeMap, BTreeSet};

use crate::analyzers::flaky;
use crate::junit::{JunitReport, TestStatus};
use crate::verify::{self, FailureSignature};

/// In which of the two orders a test passed. It failed in the other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassedIn {
    /// The order the command runs its tests in by itself.
    Given,
    /// The shuffled order.
    Shuffled,
}

/// What a test did when it ran alone, and what that says about a result
/// that follows the order. A shuffle shows that a dependency exists; only
/// the test by itself shows which way it runs, and that decides the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alone {
    /// It fails alone the way it failed in the suite: it needs something a
    /// test before it leaves behind. The fix is a setup of its own.
    NeedsPredecessor,
    /// It passes alone: something a test before it leaves behind breaks
    /// it. The fix is in the test that leaks.
    BrokenByPredecessor,
    /// It fails alone, but not the way it failed in the suite: isolation
    /// brought out another failure, which says nothing about this one.
    FailedDifferently,
    /// It passed alone once and failed alone once. Nothing ran before it
    /// either time, so no order explains that.
    Mixed,
    /// It was not run alone, or its runs alone had no result for it.
    Unknown,
}

/// A test whose outcome was not the same in both orders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// The test's identity (`classname::name`).
    pub id: String,
    pub passed_in: PassedIn,
    /// [`Alone::Unknown`] until [`direct`] has heard the test's runs alone.
    pub alone: Alone,
}

/// The two orders side by side, before anything is concluded from them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Tests with a pass or a failure in both orders. A skip carries no
    /// signal, and a test only one run reported cannot be compared.
    pub compared: usize,
    /// Whether the shuffled run ran those tests in another order at all.
    /// When it did not, the pass compared an order with itself.
    pub reordered: bool,
    /// Sorted by id.
    pub differences: Vec<Difference>,
}

/// What the pass found: every difference, filed under what it proves.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub compared: usize,
    /// The outcome followed the order: one thing in the given order, twice,
    /// and the other in the shuffled order, twice.
    pub order_dependent: Vec<Difference>,
    /// Differed between the orders once, and then did not do the same
    /// thing again in one of them. No order explains that; it is what a
    /// flaky test does.
    pub unstable: Vec<Difference>,
    /// Differed, but the history already proved the test flaky, so its
    /// difference says nothing about order — however the replay went.
    pub known_flakes: Vec<Difference>,
}

impl Analysis {
    /// Whether any test's outcome followed the order.
    pub fn found_dependence(&self) -> bool {
        !self.order_dependent.is_empty()
    }
}

/// Whether the test passed in a run, for the tests that said so: `None` for
/// a skip.
fn passed(status: TestStatus) -> Option<bool> {
    match status {
        TestStatus::Passed => Some(true),
        TestStatus::Failed | TestStatus::Error => Some(false),
        TestStatus::Skipped => None,
    }
}

/// One run's verdict per test id, skips left out.
fn verdicts(report: &JunitReport) -> BTreeMap<String, bool> {
    flaky::run_outcomes(report)
        .into_iter()
        .filter_map(|(id, status)| Some((id, passed(status)?)))
        .collect()
}

/// Lay the run in the command's own order beside the shuffled one.
pub fn compare(given: &JunitReport, shuffled: &JunitReport) -> Comparison {
    let in_given = verdicts(given);
    let in_shuffled = verdicts(shuffled);
    let mut comparison = Comparison {
        reordered: flaky::reordered(given, shuffled),
        ..Comparison::default()
    };
    for (id, passed_given) in in_given {
        let Some(passed_shuffled) = in_shuffled.get(&id).copied() else {
            continue;
        };
        comparison.compared += 1;
        if passed_given != passed_shuffled {
            comparison.differences.push(Difference {
                id,
                passed_in: if passed_given {
                    PassedIn::Given
                } else {
                    PassedIn::Shuffled
                },
                alone: Alone::Unknown,
            });
        }
    }
    comparison
}

/// Both orders run a second time, to see what holds.
#[derive(Clone, Copy)]
pub struct Again<'a> {
    pub given: &'a JunitReport,
    pub shuffled: &'a JunitReport,
}

/// File every difference under what it proves. `again` is both orders run
/// a second time, made only when there was a difference to check: a flake
/// that happened to fail in the first run would otherwise read as a test
/// that fails in that order. `proven_flaky` are the tests the history has
/// already seen pass and fail on one commit. A difference a second run does
/// not speak to — the test was skipped there, or there were no second runs
/// — is not proven and counts as unstable.
pub fn analyze(
    comparison: Comparison,
    again: Option<Again<'_>>,
    proven_flaky: &BTreeSet<String>,
) -> Analysis {
    let given_again = again.map(|again| verdicts(again.given)).unwrap_or_default();
    let shuffled_again = again
        .map(|again| verdicts(again.shuffled))
        .unwrap_or_default();
    let mut analysis = Analysis {
        compared: comparison.compared,
        ..Analysis::default()
    };
    for difference in comparison.differences {
        let passed_given = difference.passed_in == PassedIn::Given;
        // In each order it did, the second time, what it did the first.
        let held = given_again.get(&difference.id) == Some(&passed_given)
            && shuffled_again.get(&difference.id) == Some(&!passed_given);
        let bucket = if proven_flaky.contains(&difference.id) {
            &mut analysis.known_flakes
        } else if held {
            &mut analysis.order_dependent
        } else {
            &mut analysis.unstable
        };
        bucket.push(difference);
    }
    analysis
}

/// One order-dependent test with what it takes to hear it alone: the
/// failure it showed in the order it failed in, and its runs by itself.
pub struct Lone<'a> {
    pub id: &'a str,
    /// The failure the suite saw, for the runs alone to reproduce — or not.
    pub suite_failure: &'a FailureSignature,
    /// The reports of its runs alone. Empty when it was not run alone.
    pub runs: &'a [JunitReport],
}

/// Settle which way each dependency runs, from the tests' runs alone. A
/// test that did not do the same thing in every run alone is no longer
/// called order-dependent: with nothing before it either time, its result
/// changed all the same, and it is filed under the differences that did
/// not hold.
pub fn direct(analysis: &mut Analysis, lone: &[Lone<'_>]) {
    let heard: BTreeMap<&str, Alone> = lone.iter().map(|test| (test.id, alone(test))).collect();
    let (mixed, mut settled): (Vec<Difference>, Vec<Difference>) =
        std::mem::take(&mut analysis.order_dependent)
            .into_iter()
            .map(|mut difference| {
                difference.alone = heard
                    .get(difference.id.as_str())
                    .copied()
                    .unwrap_or(Alone::Unknown);
                difference
            })
            .partition(|difference| difference.alone == Alone::Mixed);
    analysis.order_dependent.append(&mut settled);
    analysis.unstable.extend(mixed);
    analysis.unstable.sort_by(|a, b| a.id.cmp(&b.id));
}

/// What one test's runs alone say. Every run has to speak for it — a run
/// that skipped it or did not report it leaves the question open — and one
/// reproduction of the suite's failure is a reproduction, as for `--verify`.
fn alone(test: &Lone<'_>) -> Alone {
    let mut passed = 0;
    let mut reproduced = false;
    for run in test.runs {
        match verify::run_signatures(run).get(test.id) {
            Some(signature) if signature.status == TestStatus::Passed => passed += 1,
            Some(signature)
                if matches!(signature.status, TestStatus::Failed | TestStatus::Error) =>
            {
                reproduced |= verify::same_failure(test.suite_failure, signature);
            }
            _ => return Alone::Unknown,
        }
    }
    if test.runs.is_empty() {
        Alone::Unknown
    } else if passed == test.runs.len() {
        Alone::BrokenByPredecessor
    } else if passed > 0 {
        Alone::Mixed
    } else if reproduced {
        Alone::NeedsPredecessor
    } else {
        Alone::FailedDifferently
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze, compare, direct, Again, Alone, Analysis, Difference, Lone, PassedIn};
    use crate::junit::TestStatus;
    use crate::junit::{parse_str, JunitReport};
    use crate::verify::FailureSignature;
    use std::collections::BTreeSet;

    /// A report of the named tests in this order; a name ending in `!`
    /// failed, one ending in `?` was skipped.
    fn run(tests: &[&str]) -> JunitReport {
        use std::fmt::Write as _;
        let mut cases = String::new();
        for test in tests {
            let name = test.trim_end_matches(['!', '?']);
            let child = match test.chars().last() {
                Some('!') => "<failure/>",
                Some('?') => "<skipped/>",
                _ => "",
            };
            let _ = write!(
                cases,
                r#"<testcase classname="c" name="{name}">{child}</testcase>"#
            );
        }
        parse_str(&format!("<testsuite>{cases}</testsuite>")).unwrap()
    }

    fn difference(id: &str, passed_in: PassedIn) -> Difference {
        Difference {
            id: id.to_owned(),
            passed_in,
            alone: Alone::Unknown,
        }
    }

    /// A plain assertion failure, as `run`'s `!` writes it.
    fn assertion() -> FailureSignature {
        FailureSignature {
            status: TestStatus::Failed,
            failure_type: None,
        }
    }

    /// What `direct` makes of one order-dependent test `c::t`, given its
    /// runs alone: where it ended up, and with which answer.
    fn directed(runs: &[JunitReport], suite_failure: &FailureSignature) -> (bool, Alone) {
        let mut analysis = Analysis {
            compared: 2,
            order_dependent: vec![difference("c::t", PassedIn::Given)],
            ..Analysis::default()
        };
        let lone = [Lone {
            id: "c::t",
            suite_failure,
            runs,
        }];
        direct(&mut analysis, &lone);
        let still_dependent = analysis.found_dependence();
        let filed = analysis
            .order_dependent
            .iter()
            .chain(&analysis.unstable)
            .next()
            .expect("the test is filed somewhere");
        (still_dependent, filed.alone)
    }

    #[test]
    fn a_test_that_fails_alone_as_in_the_suite_needs_a_test_before_it() {
        let runs = [run(&["t!"]), run(&["t!"])];
        assert_eq!(
            directed(&runs, &assertion()),
            (true, Alone::NeedsPredecessor)
        );
    }

    #[test]
    fn a_test_that_passes_alone_is_broken_by_a_test_before_it() {
        let runs = [run(&["t"]), run(&["t"])];
        assert_eq!(
            directed(&runs, &assertion()),
            (true, Alone::BrokenByPredecessor)
        );
    }

    #[test]
    fn a_test_that_does_not_do_the_same_thing_alone_twice_is_not_order_dependent() {
        // Nothing ran before it either time, and its result changed: that
        // is a flake, whatever its four runs in the suite looked like.
        let runs = [run(&["t"]), run(&["t!"])];
        assert_eq!(directed(&runs, &assertion()), (false, Alone::Mixed));
    }

    #[test]
    fn a_failure_alone_that_is_not_the_suites_failure_says_nothing_about_it() {
        // The suite saw a TypeError; alone the test trips over something
        // else. A misconfigured selection does exactly this.
        let type_error = FailureSignature {
            status: TestStatus::Error,
            failure_type: Some("TypeError".to_owned()),
        };
        let runs = [run(&["t!"]), run(&["t!"])];
        assert_eq!(
            directed(&runs, &type_error),
            (true, Alone::FailedDifferently)
        );
    }

    #[test]
    fn a_test_its_runs_alone_do_not_speak_for_keeps_its_question_open() {
        // Skipped alone, absent from the report, or never run alone.
        for runs in [
            vec![run(&["t?"]), run(&["t!"])],
            vec![run(&["other"])],
            Vec::new(),
        ] {
            assert_eq!(directed(&runs, &assertion()), (true, Alone::Unknown));
        }
    }

    #[test]
    fn a_test_nobody_ran_alone_stays_order_dependent_with_no_direction() {
        let mut analysis = Analysis {
            compared: 2,
            order_dependent: vec![difference("c::t", PassedIn::Given)],
            ..Analysis::default()
        };
        direct(&mut analysis, &[]);
        assert_eq!(
            analysis.order_dependent,
            [difference("c::t", PassedIn::Given)]
        );
    }

    fn none_proven() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn a_test_that_follows_the_order_both_times_depends_on_it() {
        // `needs` passes behind `setup` and fails in front of it; `victim`
        // fails behind `leak` and passes in front of it.
        let given = run(&["setup", "needs", "victim", "leak"]);
        let shuffled = run(&["leak", "victim!", "needs!", "setup"]);
        let comparison = compare(&given, &shuffled);
        assert_eq!(comparison.compared, 4);
        assert!(comparison.reordered);

        let again = Again {
            given: &given,
            shuffled: &shuffled,
        };
        let analysis = analyze(comparison, Some(again), &none_proven());
        assert_eq!(
            analysis.order_dependent,
            [
                difference("c::needs", PassedIn::Given),
                difference("c::victim", PassedIn::Given)
            ]
        );
        assert!(analysis.unstable.is_empty());
        assert!(analysis.found_dependence());
    }

    #[test]
    fn a_failure_in_the_given_order_that_passes_shuffled_is_a_difference_too() {
        let given = run(&["leak", "victim!"]);
        let shuffled = run(&["victim", "leak"]);
        let again = Again {
            given: &given,
            shuffled: &shuffled,
        };
        let analysis = analyze(compare(&given, &shuffled), Some(again), &none_proven());
        assert_eq!(
            analysis.order_dependent,
            [difference("c::victim", PassedIn::Shuffled)]
        );
    }

    #[test]
    fn a_difference_that_does_not_hold_in_the_shuffled_order_is_not_about_order() {
        // The same shuffled order, run again, and the test passes: an order
        // cannot explain a result that changes while the order does not.
        let given = run(&["a", "wob"]);
        let shuffled = run(&["wob!", "a"]);
        let again = Again {
            given: &given,
            shuffled: &run(&["wob", "a"]),
        };
        let analysis = analyze(compare(&given, &shuffled), Some(again), &none_proven());
        assert!(analysis.order_dependent.is_empty());
        assert_eq!(analysis.unstable, [difference("c::wob", PassedIn::Given)]);
        assert!(!analysis.found_dependence());
    }

    #[test]
    fn a_flake_that_failed_in_the_first_run_is_not_a_test_that_fails_in_that_order() {
        // It failed once in the given order and passed shuffled, twice. A
        // replay of the shuffled order alone would call that a dependency;
        // the given order, run again, passes too.
        let given = run(&["a", "wob!"]);
        let shuffled = run(&["wob", "a"]);
        let again = Again {
            given: &run(&["a", "wob"]),
            shuffled: &shuffled,
        };
        let analysis = analyze(compare(&given, &shuffled), Some(again), &none_proven());
        assert!(analysis.order_dependent.is_empty());
        assert_eq!(
            analysis.unstable,
            [difference("c::wob", PassedIn::Shuffled)]
        );
    }

    #[test]
    fn a_difference_without_a_word_from_a_second_run_is_not_proven() {
        let given = run(&["a", "b"]);
        let shuffled = run(&["b!", "a"]);
        // Skipped the second time, or no second runs at all.
        let skipped = run(&["b?", "a"]);
        let silent = Again {
            given: &given,
            shuffled: &skipped,
        };
        for again in [Some(silent), None] {
            let analysis = analyze(compare(&given, &shuffled), again, &none_proven());
            assert!(analysis.order_dependent.is_empty());
            assert_eq!(analysis.unstable.len(), 1);
        }
    }

    #[test]
    fn a_test_the_history_proved_flaky_is_never_called_order_dependent() {
        let given = run(&["a", "wob"]);
        let shuffled = run(&["wob!", "a"]);
        let proven = BTreeSet::from(["c::wob".to_owned()]);
        // Even when both orders held: a flake can do the same thing twice.
        let again = Again {
            given: &given,
            shuffled: &shuffled,
        };
        let analysis = analyze(compare(&given, &shuffled), Some(again), &proven);
        assert!(analysis.order_dependent.is_empty());
        assert_eq!(
            analysis.known_flakes,
            [difference("c::wob", PassedIn::Given)]
        );
    }

    #[test]
    fn the_same_outcomes_in_both_orders_are_no_finding() {
        // Red in both orders is a failing test, not an order-dependent one.
        let given = run(&["a", "b!", "c"]);
        let shuffled = run(&["c", "b!", "a"]);
        let comparison = compare(&given, &shuffled);
        assert!(comparison.differences.is_empty());
        assert_eq!(comparison.compared, 3);
    }

    #[test]
    fn a_skip_and_a_test_only_one_run_saw_are_not_compared() {
        let given = run(&["a", "b", "only_given", "s?"]);
        let shuffled = run(&["s!", "b", "a", "only_shuffled!"]);
        let comparison = compare(&given, &shuffled);
        assert_eq!(comparison.compared, 2);
        assert!(comparison.differences.is_empty());
    }

    #[test]
    fn a_shuffle_that_changed_nothing_is_said_to_have_changed_nothing() {
        let given = run(&["a", "b", "c"]);
        assert!(!compare(&given, &given).reordered);
        assert!(compare(&given, &run(&["c", "a", "b"])).reordered);
    }
}
