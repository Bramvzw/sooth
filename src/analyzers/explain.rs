//! Classification of a red run's failures against what sooth already knows:
//! the accumulated history and the committed quarantine list (see
//! `DECISIONS.md`). This pass runs no tests and draws no new conclusions —
//! it only looks up each failure in the evidence that already exists.

use std::collections::BTreeSet;

use crate::analyzers::history::Analysis as HistoryAnalysis;

/// What the accumulated evidence says about one failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Proven flaky by the history: mixed outcomes on one clean commit.
    KnownFlake {
        failed_runs: usize,
        observed_runs: usize,
    },
    /// A regression pointer from the history — known, but never a flake.
    FailingSince { commit: String, failed_runs: usize },
    /// The history holds no proof either way about this failure.
    Unknown,
}

/// One failure of the run being explained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    pub verdict: Verdict,
    /// Listed in the quarantine file — the team already knows this one.
    pub quarantined: bool,
}

/// How the failures divide over the verdicts. The categories partition the
/// failures: a quarantined test the history could classify is counted by its
/// history verdict, so the four counts sum to the failure count.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub known_flakes: usize,
    pub failing_since: usize,
    /// Quarantined failures the history could not classify.
    pub quarantined: usize,
    pub new: usize,
}

impl Counts {
    pub fn of(explanations: &[Explanation]) -> Self {
        let mut counts = Self::default();
        for explanation in explanations {
            match explanation.verdict {
                Verdict::KnownFlake { .. } => counts.known_flakes += 1,
                Verdict::FailingSince { .. } => counts.failing_since += 1,
                Verdict::Unknown if explanation.quarantined => counts.quarantined += 1,
                Verdict::Unknown => counts.new += 1,
            }
        }
        counts
    }

    pub fn total(&self) -> usize {
        self.known_flakes + self.failing_since + self.quarantined + self.new
    }

    /// Every failure is a flake sooth or the team already knew about — the
    /// answer that unblocks a red build. A regression pointer is known too,
    /// but it is a real failure and must never read as "nothing new".
    pub fn only_known_flakes(&self) -> bool {
        self.total() > 0 && self.failing_since == 0 && self.new == 0
    }
}

/// Look up each failed id in the evidence. `history` is `None` when the
/// history was not consulted (`--no-history`), which is not the same as an
/// empty history: the caller says so in the report instead of letting every
/// failure read as new.
pub fn explain(
    failed_ids: &[String],
    history: Option<&HistoryAnalysis>,
    quarantine: &BTreeSet<String>,
) -> Vec<Explanation> {
    failed_ids
        .iter()
        .map(|id| Explanation {
            verdict: history.map_or(Verdict::Unknown, |analysis| verdict_for(id, analysis)),
            quarantined: quarantine.contains(id),
            id: id.clone(),
        })
        .collect()
}

/// The history's verdict for one id. Flaky proof wins over a regression
/// pointer, matching the history pass's own precedence.
fn verdict_for(id: &str, history: &HistoryAnalysis) -> Verdict {
    if let Some(test) = history.flaky.iter().find(|test| test.id == id) {
        return Verdict::KnownFlake {
            failed_runs: test.failed,
            observed_runs: test.observed(),
        };
    }
    if let Some(test) = history.failing_since.iter().find(|test| test.id == id) {
        return Verdict::FailingSince {
            commit: test.commit.clone(),
            failed_runs: test.failed_runs,
        };
    }
    Verdict::Unknown
}

#[cfg(test)]
mod tests {
    use super::{explain, Counts, Verdict};
    use crate::analyzers::flaky::TestOutcomes;
    use crate::analyzers::history::{Analysis as HistoryAnalysis, FailingSince};
    use std::collections::BTreeSet;

    fn history() -> HistoryAnalysis {
        HistoryAnalysis {
            flaky: vec![TestOutcomes {
                id: "c::wobbly".to_owned(),
                passed: 46,
                failed: 4,
            }],
            failing_since: vec![FailingSince {
                id: "c::regressed".to_owned(),
                commit: "abc1234def".to_owned(),
                failed_runs: 6,
            }],
        }
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn a_proven_flake_carries_its_rate_from_the_history() {
        let explained = explain(&ids(&["c::wobbly"]), Some(&history()), &BTreeSet::new());
        assert_eq!(
            explained[0].verdict,
            Verdict::KnownFlake {
                failed_runs: 4,
                observed_runs: 50,
            }
        );
        assert_eq!(crate::analyzers::flaky::failure_rate_percent(4, 50), 8);
        assert_eq!(Counts::of(&explained).known_flakes, 1);
    }

    #[test]
    fn a_regression_pointer_is_reported_as_such_never_as_a_flake() {
        let explained = explain(&ids(&["c::regressed"]), Some(&history()), &BTreeSet::new());
        assert_eq!(
            explained[0].verdict,
            Verdict::FailingSince {
                commit: "abc1234def".to_owned(),
                failed_runs: 6,
            }
        );
        // Known, but a real failure: it must not make the run read as clean.
        let counts = Counts::of(&explained);
        assert_eq!(counts.failing_since, 1);
        assert!(!counts.only_known_flakes());
    }

    #[test]
    fn a_failure_the_history_cannot_explain_is_new() {
        let explained = explain(&ids(&["c::fresh"]), Some(&history()), &BTreeSet::new());
        assert_eq!(explained[0].verdict, Verdict::Unknown);
        assert_eq!(Counts::of(&explained).new, 1);
    }

    #[test]
    fn a_quarantined_failure_is_known_even_without_history_evidence() {
        let quarantine = BTreeSet::from(["c::listed".to_owned()]);
        let explained = explain(&ids(&["c::listed"]), Some(&history()), &quarantine);
        assert!(explained[0].quarantined);
        let counts = Counts::of(&explained);
        assert_eq!(counts.quarantined, 1);
        assert!(counts.only_known_flakes());
    }

    #[test]
    fn history_evidence_wins_the_count_over_the_quarantine_label() {
        // The categories partition the failures: a quarantined proven flake
        // is counted once, as the flake the history proved it is.
        let quarantine = BTreeSet::from(["c::wobbly".to_owned()]);
        let explained = explain(&ids(&["c::wobbly"]), Some(&history()), &quarantine);
        assert!(explained[0].quarantined);
        let counts = Counts::of(&explained);
        assert_eq!(counts.known_flakes, 1);
        assert_eq!(counts.quarantined, 0);
        assert_eq!(counts.total(), 1);
    }

    #[test]
    fn a_mixed_run_is_not_only_known_flakes() {
        let explained = explain(
            &ids(&["c::wobbly", "c::fresh"]),
            Some(&history()),
            &BTreeSet::new(),
        );
        let counts = Counts::of(&explained);
        assert_eq!(counts.known_flakes, 1);
        assert_eq!(counts.new, 1);
        assert!(!counts.only_known_flakes());
    }

    #[test]
    fn without_a_history_only_the_quarantine_can_speak() {
        let quarantine = BTreeSet::from(["c::listed".to_owned()]);
        let explained = explain(&ids(&["c::wobbly", "c::listed"]), None, &quarantine);
        assert_eq!(explained[0].verdict, Verdict::Unknown);
        assert_eq!(Counts::of(&explained).new, 1);
        assert!(explained[1].quarantined);
    }

    #[test]
    fn a_green_run_explains_nothing() {
        let explained = explain(&[], Some(&history()), &BTreeSet::new());
        assert!(explained.is_empty());
        assert!(!Counts::of(&explained).only_known_flakes());
    }
}
