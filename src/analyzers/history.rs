//! Classification of the accumulated run history (see `DECISIONS.md`).
//!
//! Flaky requires proof: mixed outcomes on one *clean* commit. A green→red
//! flip at a commit boundary is a regression pointer ("failing since"),
//! never flaky. Observations on dirty or unknown code count in the totals
//! but can never be evidence. One new red observation concludes nothing.

use std::collections::BTreeMap;

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

/// The outcome of the history pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Proven flaky — at least one clean commit observed both passing and
    /// failing. Counts cover the whole window, sorted like the active pass.
    pub flaky: Vec<TestOutcomes>,
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

/// Classify the history. Observations must be in append (time) order; the
/// analysis looks at each test's last [`WINDOW_PER_TEST`] signal-carrying
/// observations (skips carry none).
pub fn analyze(observations: &[Observation]) -> Analysis {
    let mut by_test: BTreeMap<&str, Vec<&Observation>> = BTreeMap::new();
    for observation in observations {
        if observation.status == TestStatus::Skipped {
            continue;
        }
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
            analysis.flaky.push(TestOutcomes {
                id: id.to_owned(),
                passed: signal.len() - failed_count,
                failed: failed_count,
            });
            continue;
        }

        // Regression pointer: a trailing failure streak of at least two,
        // anchored on clean, known code.
        let streak: Vec<&&Observation> = signal.iter().rev().take_while(|o| failed(o)).collect();
        if streak.len() < 2 {
            continue;
        }
        let first_red = streak[streak.len() - 1];
        if first_red.dirty != Some(false) {
            continue;
        }
        let Some(commit) = &first_red.commit else {
            continue;
        };
        analysis.failing_since.push(FailingSince {
            id: id.to_owned(),
            commit: commit.clone(),
            failed_runs: streak.len(),
        });
    }

    analysis.flaky.sort_by(|a, b| {
        b.failure_rate_percent()
            .cmp(&a.failure_rate_percent())
            .then(a.id.cmp(&b.id))
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
        assert_eq!(analysis.flaky[0].id, "c::t");
        assert_eq!(analysis.flaky[0].passed, 2);
        assert_eq!(analysis.flaky[0].failed, 1);
        assert!(analysis.failing_since.is_empty());
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
