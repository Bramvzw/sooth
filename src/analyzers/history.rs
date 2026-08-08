//! Classification of the accumulated run history (see `DECISIONS.md`).
//!
//! Flaky requires proof: mixed outcomes on one *clean* commit. A green→red
//! flip at a commit boundary is a regression pointer ("failing since"),
//! never flaky. Observations on dirty or unknown code count in the totals
//! but can never be evidence. One new red observation concludes nothing.

use std::collections::{BTreeMap, BTreeSet};

use crate::analyzers::flaky::TestOutcomes;
use crate::history::{Observation, WINDOW_PER_TEST};
use crate::junit::TestStatus;

/// A test that stopped passing at a commit boundary and never recovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingSince {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    /// The first commit the trailing failure streak was observed on.
    pub commit: String,
    /// Length of that trailing streak.
    pub failed_runs: usize,
}

/// A proven flake, plus where its failures came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricFlake {
    pub outcomes: TestOutcomes,
    /// The single environment every failure was observed in, when more than
    /// one environment observed this test at all. That is the whole claim —
    /// "it only breaks over there" — and it needs a second environment to
    /// mean anything.
    pub failures_confined_to: Option<String>,
}

/// The outcome of the history pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Proven flaky — at least one clean commit observed both passing and
    /// failing. Counts cover the whole window, sorted like the active pass.
    pub flaky: Vec<HistoricFlake>,
    /// Regression pointers, sorted by streak length (longest first), then id.
    pub failing_since: Vec<FailingSince>,
}

impl Analysis {
    pub fn is_empty(&self) -> bool {
        self.flaky.is_empty() && self.failing_since.is_empty()
    }
}

fn failed(observation: &Observation) -> bool {
    matches!(observation.status, TestStatus::Failed | TestStatus::Error)
}

/// The one environment every failure came from, when a second environment
/// also observed this test. Both halves are required: without a failure
/// there is nothing to place, and without a second environment "all failures
/// were local" says no more than "all observations were local".
///
/// Observations from before environments were recorded (`None`) are counted
/// as their own unknown environment, so a history that predates this cannot
/// produce a confident claim about one.
fn failures_confined_to(signal: &[&Observation]) -> Option<String> {
    let mut environments = BTreeSet::new();
    let mut failing_environments = BTreeSet::new();
    for observation in signal {
        environments.insert(observation.environment.as_deref());
        if failed(observation) {
            failing_environments.insert(observation.environment.as_deref());
        }
    }
    if environments.len() < 2 || failing_environments.len() != 1 {
        return None;
    }
    failing_environments
        .into_iter()
        .next()
        .flatten()
        .map(str::to_owned)
}

/// Classify the history. Time order comes from `at`, not file position:
/// evidence brought in from elsewhere lands at the end of the file however
/// old it is, and failing-since reads the tail. The sort is stable, so
/// observations sharing one `at` — a single run stamps all of its
/// observations alike — keep their file order. The analysis looks at each
/// test's last [`WINDOW_PER_TEST`] signal-carrying observations (skips
/// carry none).
pub fn analyze(observations: &[Observation]) -> Analysis {
    let mut in_time_order: Vec<&Observation> = observations
        .iter()
        .filter(|observation| observation.status != TestStatus::Skipped)
        .collect();
    in_time_order.sort_by_key(|observation| observation.at_epoch_secs);

    let mut by_test: BTreeMap<&str, Vec<&Observation>> = BTreeMap::new();
    for observation in in_time_order {
        by_test
            .entry(&observation.id)
            .or_default()
            .push(observation);
    }

    let mut analysis = Analysis::default();
    for (id, mut signal) in by_test {
        if signal.len() > WINDOW_PER_TEST {
            signal.drain(..signal.len() - WINDOW_PER_TEST);
        }
        if signal.len() < 2 {
            continue;
        }
        let failed_count = signal.iter().filter(|o| failed(o)).count();
        if failed_count == 0 {
            continue;
        }

        // Flaky evidence: one clean commit with both outcomes.
        let mut per_clean_commit: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
        for observation in &signal {
            if observation.dirty != Some(false) {
                continue;
            }
            if let Some(commit) = &observation.commit {
                let (pass, fail) = per_clean_commit.entry(commit).or_default();
                if failed(observation) {
                    *fail = true;
                } else {
                    *pass = true;
                }
            }
        }
        if per_clean_commit.values().any(|(pass, fail)| *pass && *fail) {
            analysis.flaky.push(HistoricFlake {
                outcomes: TestOutcomes {
                    id: id.to_owned(),
                    passed: signal.len() - failed_count,
                    failed: failed_count,
                },
                failures_confined_to: failures_confined_to(&signal),
            });
            continue;
        }

        // Regression pointer: a trailing failure streak of at least two,
        // anchored on the streak's earliest *clean* red — dirty reds still
        // count in the streak, they just cannot carry the address.
        let streak: Vec<&&Observation> = signal.iter().rev().take_while(|o| failed(o)).collect();
        if streak.len() < 2 {
            continue;
        }
        let anchor = streak
            .iter()
            .rev()
            .find(|o| o.dirty == Some(false) && o.commit.is_some());
        let Some(commit) = anchor.and_then(|o| o.commit.as_ref()) else {
            continue;
        };
        analysis.failing_since.push(FailingSince {
            id: id.to_owned(),
            commit: commit.clone(),
            failed_runs: streak.len(),
        });
    }

    analysis.flaky.sort_by(|a, b| {
        b.outcomes
            .failure_rate_percent()
            .cmp(&a.outcomes.failure_rate_percent())
            .then(a.outcomes.id.cmp(&b.outcomes.id))
    });
    analysis
        .failing_since
        .sort_by(|a, b| b.failed_runs.cmp(&a.failed_runs).then(a.id.cmp(&b.id)));
    analysis
}

#[cfg(test)]
mod tests {
    use super::analyze;
    use crate::history::Observation;
    use crate::junit::TestStatus;

    fn obs(id: &str, status: TestStatus, commit: Option<&str>, dirty: Option<bool>) -> Observation {
        Observation {
            id: id.to_owned(),
            status,
            commit: commit.map(str::to_owned),
            dirty,
            environment: None,
            at_epoch_secs: 0,
        }
    }

    fn clean(id: &str, status: TestStatus, commit: &str) -> Observation {
        obs(id, status, Some(commit), Some(false))
    }

    #[test]
    fn mixed_outcomes_on_one_clean_commit_prove_flaky() {
        let history = [
            clean("c::t", TestStatus::Passed, "aaa"),
            clean("c::t", TestStatus::Failed, "aaa"),
            clean("c::t", TestStatus::Passed, "bbb"),
        ];
        let analysis = analyze(&history);
        assert_eq!(analysis.flaky.len(), 1);
        assert_eq!(analysis.flaky[0].outcomes.id, "c::t");
        assert_eq!(analysis.flaky[0].outcomes.passed, 2);
        assert_eq!(analysis.flaky[0].outcomes.failed, 1);
        assert!(analysis.failing_since.is_empty());
    }

    fn in_env(id: &str, status: TestStatus, commit: &str, environment: &str) -> Observation {
        Observation {
            environment: Some(environment.to_owned()),
            ..clean(id, status, commit)
        }
    }

    #[test]
    fn a_flake_that_only_breaks_in_one_environment_says_which() {
        let history = [
            in_env("c::t", TestStatus::Passed, "aaa", "local"),
            in_env("c::t", TestStatus::Passed, "aaa", "local"),
            in_env("c::t", TestStatus::Passed, "aaa", "ci"),
            in_env("c::t", TestStatus::Failed, "aaa", "ci"),
        ];
        let analysis = analyze(&history);
        assert_eq!(
            analysis.flaky[0].failures_confined_to.as_deref(),
            Some("ci")
        );
    }

    #[test]
    fn failures_spread_over_environments_name_none_of_them() {
        let history = [
            in_env("c::t", TestStatus::Passed, "aaa", "local"),
            in_env("c::t", TestStatus::Failed, "aaa", "local"),
            in_env("c::t", TestStatus::Failed, "aaa", "ci"),
        ];
        assert_eq!(analyze(&history).flaky[0].failures_confined_to, None);
    }

    #[test]
    fn one_environment_alone_proves_nothing_about_environments() {
        // "Every failure was local" says no more than "every run was local".
        let history = [
            in_env("c::t", TestStatus::Passed, "aaa", "local"),
            in_env("c::t", TestStatus::Failed, "aaa", "local"),
        ];
        assert_eq!(analyze(&history).flaky[0].failures_confined_to, None);
    }

    #[test]
    fn a_history_that_predates_environments_makes_no_claim() {
        // Unlabelled observations are their own unknown environment, so they
        // cannot be silently folded into whichever one happens to be labelled.
        let history = [
            clean("c::t", TestStatus::Passed, "aaa"),
            in_env("c::t", TestStatus::Passed, "aaa", "ci"),
            Observation {
                environment: None,
                ..clean("c::t", TestStatus::Failed, "aaa")
            },
        ];
        assert_eq!(analyze(&history).flaky[0].failures_confined_to, None);
    }

    #[test]
    fn a_green_to_red_flip_at_a_commit_boundary_is_failing_since_not_flaky() {
        let history = [
            clean("c::t", TestStatus::Passed, "aaa"),
            clean("c::t", TestStatus::Passed, "aaa"),
            clean("c::t", TestStatus::Failed, "bbb"),
            clean("c::t", TestStatus::Failed, "bbb"),
        ];
        let analysis = analyze(&history);
        assert!(analysis.flaky.is_empty(), "a regression got called flaky");
        assert_eq!(analysis.failing_since.len(), 1);
        assert_eq!(analysis.failing_since[0].commit, "bbb");
        assert_eq!(analysis.failing_since[0].failed_runs, 2);
    }

    #[test]
    fn one_new_red_observation_concludes_nothing() {
        let history = [
            clean("c::t", TestStatus::Passed, "aaa"),
            clean("c::t", TestStatus::Failed, "bbb"),
        ];
        assert!(analyze(&history).is_empty());
    }

    #[test]
    fn dirty_observations_count_in_totals_but_are_never_evidence() {
        // The only mixed pair on one commit involves a dirty run: no proof.
        let history = [
            obs("c::t", TestStatus::Passed, Some("aaa"), Some(true)),
            clean("c::t", TestStatus::Failed, "aaa"),
            clean("c::t", TestStatus::Failed, "aaa"),
        ];
        let analysis = analyze(&history);
        assert!(analysis.flaky.is_empty(), "dirty run was taken as evidence");
        // The trailing streak is anchored on clean code, so it does report.
        assert_eq!(analysis.failing_since.len(), 1);
        assert_eq!(analysis.failing_since[0].failed_runs, 2);
    }

    #[test]
    fn unknown_identity_reports_nothing() {
        let history = [
            obs("c::t", TestStatus::Passed, None, None),
            obs("c::t", TestStatus::Failed, None, None),
            obs("c::t", TestStatus::Failed, None, None),
        ];
        assert!(analyze(&history).is_empty());
    }

    #[test]
    fn failing_since_anchors_on_the_earliest_clean_red_of_the_streak() {
        let history = [
            clean("c::t", TestStatus::Passed, "aaa"),
            obs("c::t", TestStatus::Failed, Some("bbb"), Some(true)),
            clean("c::t", TestStatus::Failed, "bbb"),
            clean("c::t", TestStatus::Failed, "bbb"),
        ];
        let analysis = analyze(&history);
        assert!(analysis.flaky.is_empty());
        assert_eq!(analysis.failing_since.len(), 1);
        assert_eq!(analysis.failing_since[0].commit, "bbb");
        // Dirty reds count in the streak; they only cannot carry the address.
        assert_eq!(analysis.failing_since[0].failed_runs, 3);
    }

    #[test]
    fn time_order_comes_from_at_not_from_file_position() {
        // Imported evidence lands at the end of the file however old it is.
        // In file order the tail here is green, so nothing would be found;
        // in `at` order the reds are the tail and the flip is a regression.
        let stamped = |observation: Observation, secs: u64| Observation {
            at_epoch_secs: secs,
            ..observation
        };
        let history = [
            stamped(clean("c::t", TestStatus::Failed, "bbb"), 30),
            stamped(clean("c::t", TestStatus::Failed, "bbb"), 40),
            stamped(clean("c::t", TestStatus::Passed, "aaa"), 10),
            stamped(clean("c::t", TestStatus::Passed, "aaa"), 20),
        ];
        let analysis = analyze(&history);
        assert!(analysis.flaky.is_empty());
        assert_eq!(
            analysis.failing_since.len(),
            1,
            "the tail was read in file order"
        );
        assert_eq!(analysis.failing_since[0].commit, "bbb");
        assert_eq!(analysis.failing_since[0].failed_runs, 2);
    }

    #[test]
    fn failing_every_observation_reports_since_the_first_commit() {
        let history = [
            clean("c::t", TestStatus::Failed, "aaa"),
            clean("c::t", TestStatus::Failed, "bbb"),
        ];
        let analysis = analyze(&history);
        assert!(analysis.flaky.is_empty());
        assert_eq!(analysis.failing_since[0].commit, "aaa");
    }

    #[test]
    fn skips_carry_no_signal_in_history() {
        let history = [
            obs("c::t", TestStatus::Skipped, Some("aaa"), Some(false)),
            clean("c::t", TestStatus::Failed, "aaa"),
        ];
        // One signal observation left: below the two-observation floor.
        assert!(analyze(&history).is_empty());
    }

    #[test]
    fn flaky_evidence_wins_over_a_trailing_streak() {
        let history = [
            clean("c::t", TestStatus::Passed, "aaa"),
            clean("c::t", TestStatus::Failed, "aaa"),
            clean("c::t", TestStatus::Failed, "bbb"),
            clean("c::t", TestStatus::Failed, "bbb"),
        ];
        let analysis = analyze(&history);
        assert_eq!(analysis.flaky.len(), 1);
        assert!(analysis.failing_since.is_empty());
    }
}
