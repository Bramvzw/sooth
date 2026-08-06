//! Classification of a red run's failures against the evidence sooth already
//! has: the accumulated history and the quarantine list (see `DECISIONS.md`).

use std::collections::BTreeSet;

use crate::analyzers::flaky::Analysis as FlakyAnalysis;
use crate::analyzers::history::Analysis as HistoryAnalysis;
use crate::verify;

/// What the accumulated evidence says about one failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    KnownFlake {
        failed_runs: usize,
        observed_runs: usize,
    },
    FailingSince {
        commit: String,
        failed_runs: usize,
    },
    Unknown,
}

/// What *this* invocation saw, as opposed to what the history remembers. The
/// repeat and verify variants never mix: `--verify` requires a single run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// Mixed outcomes across this invocation's runs.
    Flaky {
        failed_runs: usize,
        observed_runs: usize,
    },
    /// Failed every run of this invocation.
    Broken { observed_runs: usize },
    /// Verification reproduced the failure.
    Real,
    /// Verification passed it in isolation.
    FlakyOrOrder,
    /// Verification never actually re-ran it.
    Unverified,
}

/// One failure of the run being explained, with everything sooth knows about
/// it on one value: what this invocation saw, what the history remembers, and
/// whether the team already listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    pub observed: Option<Observed>,
    pub verdict: Verdict,
    pub quarantined: bool,
}

/// How the failures divide over the verdicts — a partition, so the four
/// counts sum to the failure count.
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

    /// Nothing failed that sooth or the team did not already know as a flake.
    /// A regression is known too, but it is a real failure and never counts.
    pub fn only_known_flakes(&self) -> bool {
        self.total() > 0 && self.failing_since == 0 && self.new == 0
    }
}

/// Every pass's view of the run's failures, gathered per test. A `None`
/// history means it was not consulted, which is not an empty one — the caller
/// reports the difference.
pub fn explain(
    failed_ids: &[String],
    passes: Passes<'_>,
    quarantine: &BTreeSet<String>,
) -> Vec<Explanation> {
    failed_ids
        .iter()
        .map(|id| Explanation {
            observed: observed_for(id, &passes),
            verdict: passes
                .history
                .map_or(Verdict::Unknown, |analysis| verdict_for(id, analysis)),
            quarantined: quarantine.contains(id),
            id: id.clone(),
        })
        .collect()
}

/// The passes whose findings are folded into the per-test lines.
#[derive(Default, Clone, Copy)]
pub struct Passes<'a> {
    pub active: Option<&'a FlakyAnalysis>,
    pub verify: Option<&'a verify::Verdict>,
    pub history: Option<&'a HistoryAnalysis>,
}

/// What this invocation saw about one id. The active pass and verification
/// cannot both have run, so the first match is the only match.
fn observed_for(id: &str, passes: &Passes<'_>) -> Option<Observed> {
    if let Some(active) = passes.active {
        if let Some(test) = active.flaky.iter().find(|test| test.id == id) {
            return Some(Observed::Flaky {
                failed_runs: test.failed,
                observed_runs: test.observed(),
            });
        }
        if let Some(test) = active.broken.iter().find(|test| test.id == id) {
            return Some(Observed::Broken {
                observed_runs: test.observed(),
            });
        }
    }
    let verdict = passes.verify?;
    let owned = id.to_owned();
    if verdict.real.contains(&owned) {
        return Some(Observed::Real);
    }
    if verdict.flaky_or_order.contains(&owned) {
        return Some(Observed::FlakyOrOrder);
    }
    verdict
        .unverified
        .contains(&owned)
        .then_some(Observed::Unverified)
}

/// Flaky proof wins over a regression pointer, as in the history pass itself.
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

    fn explained(ids: &[&str], quarantine: &[&str]) -> Vec<super::Explanation> {
        let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
        let quarantine: BTreeSet<String> = quarantine.iter().map(|id| (*id).to_owned()).collect();
        let history = history();
        let passes = super::Passes {
            history: Some(&history),
            ..super::Passes::default()
        };
        explain(&ids, passes, &quarantine)
    }

    #[test]
    fn a_proven_flake_carries_its_counts_from_the_history() {
        let explained = explained(&["c::wobbly"], &[]);
        assert_eq!(
            explained[0].verdict,
            Verdict::KnownFlake {
                failed_runs: 4,
                observed_runs: 50,
            }
        );
        assert!(Counts::of(&explained).only_known_flakes());
    }

    #[test]
    fn a_regression_is_reported_as_such_and_never_reads_as_clean() {
        let explained = explained(&["c::regressed"], &[]);
        assert_eq!(
            explained[0].verdict,
            Verdict::FailingSince {
                commit: "abc1234def".to_owned(),
                failed_runs: 6,
            }
        );
        let counts = Counts::of(&explained);
        assert_eq!(counts.failing_since, 1);
        assert!(!counts.only_known_flakes());
    }

    #[test]
    fn a_failure_the_history_cannot_explain_is_new() {
        let explained = explained(&["c::wobbly", "c::fresh"], &[]);
        assert_eq!(explained[1].verdict, Verdict::Unknown);
        let counts = Counts::of(&explained);
        assert_eq!((counts.known_flakes, counts.new), (1, 1));
        assert!(!counts.only_known_flakes());
    }

    #[test]
    fn a_quarantined_failure_is_known_without_history_evidence() {
        let explained = explained(&["c::listed"], &["c::listed"]);
        assert!(explained[0].quarantined);
        let counts = Counts::of(&explained);
        assert_eq!(counts.quarantined, 1);
        assert!(counts.only_known_flakes());
    }

    #[test]
    fn history_evidence_wins_the_count_over_the_quarantine_label() {
        let explained = explained(&["c::wobbly"], &["c::wobbly"]);
        assert!(explained[0].quarantined);
        let counts = Counts::of(&explained);
        assert_eq!((counts.known_flakes, counts.quarantined), (1, 0));
        assert_eq!(counts.total(), 1);
    }

    #[test]
    fn without_a_history_only_the_quarantine_can_speak() {
        let ids = ["c::wobbly".to_owned(), "c::listed".to_owned()];
        let quarantine = BTreeSet::from(["c::listed".to_owned()]);
        let explained = explain(&ids, super::Passes::default(), &quarantine);
        assert_eq!(explained[0].verdict, Verdict::Unknown);
        assert_eq!(Counts::of(&explained).new, 1);
        assert!(explained[1].quarantined);
    }

    #[test]
    fn this_invocations_runs_are_carried_beside_the_history_verdict() {
        // The two axes are independent: a test can be broken *and* unknown.
        let active = crate::analyzers::flaky::Analysis {
            flaky: vec![TestOutcomes {
                id: "c::wobbly".to_owned(),
                passed: 1,
                failed: 2,
            }],
            broken: vec![TestOutcomes {
                id: "c::fresh".to_owned(),
                passed: 0,
                failed: 3,
            }],
            reordered_runs: Vec::new(),
        };
        let history = history();
        let passes = super::Passes {
            active: Some(&active),
            history: Some(&history),
            ..super::Passes::default()
        };
        let ids = ["c::wobbly".to_owned(), "c::fresh".to_owned()];
        let explained = explain(&ids, passes, &BTreeSet::new());

        assert_eq!(
            explained[0].observed,
            Some(super::Observed::Flaky {
                failed_runs: 2,
                observed_runs: 3,
            })
        );
        assert!(matches!(explained[0].verdict, Verdict::KnownFlake { .. }));
        assert_eq!(
            explained[1].observed,
            Some(super::Observed::Broken { observed_runs: 3 })
        );
        assert_eq!(explained[1].verdict, Verdict::Unknown);
    }

    #[test]
    fn verification_speaks_when_the_repeat_pass_did_not_run() {
        let verdict = crate::verify::Verdict {
            real: vec!["c::dead".to_owned()],
            flaky_or_order: vec!["c::wobbly".to_owned()],
            unverified: vec!["c::missed".to_owned()],
        };
        let passes = super::Passes {
            verify: Some(&verdict),
            ..super::Passes::default()
        };
        let ids = [
            "c::dead".to_owned(),
            "c::wobbly".to_owned(),
            "c::missed".to_owned(),
            "c::other".to_owned(),
        ];
        let explained = explain(&ids, passes, &BTreeSet::new());

        assert_eq!(explained[0].observed, Some(super::Observed::Real));
        assert_eq!(explained[1].observed, Some(super::Observed::FlakyOrOrder));
        assert_eq!(explained[2].observed, Some(super::Observed::Unverified));
        assert_eq!(explained[3].observed, None, "no pass saw this one");
    }

    #[test]
    fn a_green_run_explains_nothing() {
        let explained = explained(&[], &[]);
        assert!(explained.is_empty());
        assert!(!Counts::of(&explained).only_known_flakes());
    }
}
