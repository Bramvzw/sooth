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

/// A test that passed, then stopped passing at a commit boundary and never
/// recovered: green before the streak is what makes it a regression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingSince {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    /// The first commit the trailing failure streak was observed on.
    pub commit: String,
    /// Length of that trailing streak.
    pub failed_runs: usize,
}

/// A test that failed every observed run. No commit broke it as far as the
/// history can tell: `first_commit` is where sooth first saw it, not a cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeverPassed {
    pub id: String,
    pub first_commit: String,
    pub failed_runs: usize,
}

/// The failures that close a proven flake's window, on commits it was never
/// seen passing on. A fact beside the flake label, not a second verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedStreak {
    /// The first commit of the streak.
    pub commit: String,
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
    /// Set when the flake has failed every run since a commit, across at
    /// least [`RED_STREAK_INVOCATIONS`] invocations (see `DECISIONS.md`).
    pub red_since: Option<RedStreak>,
}

/// How many invocations in a row a proven flake must fail, on commits it
/// never passed on, before the streak is worth saying. Two is what a 50%
/// flake does one time in four.
pub const RED_STREAK_INVOCATIONS: usize = 3;

/// The outcome of the history pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    /// Proven flaky — at least one clean commit observed both passing and
    /// failing. Counts cover the whole window, sorted like the active pass.
    pub flaky: Vec<HistoricFlake>,
    /// Regression pointers, sorted by streak length (longest first), then id.
    pub failing_since: Vec<FailingSince>,
    /// Never seen passing, sorted like `failing_since`.
    pub never_passed: Vec<NeverPassed>,
}

impl Analysis {
    pub fn is_empty(&self) -> bool {
        self.flaky.is_empty() && self.failing_since.is_empty() && self.never_passed.is_empty()
    }
}

/// Which invocation an observation belongs to. A line without one stands
/// alone, so legacy histories keep the per-line evidence they always had.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Invocation<'a> {
    Run(&'a str),
    Line(usize),
}

/// The invocations that saw a test pass and fail on one clean commit.
#[derive(Default)]
struct Outcomes<'a> {
    passed_in: BTreeSet<Invocation<'a>>,
    failed_in: BTreeSet<Invocation<'a>>,
}

impl Outcomes<'_> {
    /// A pass in one invocation and a failure in another.
    fn prove_flaky(&self) -> bool {
        !self.passed_in.is_empty()
            && !self.failed_in.is_empty()
            && self.passed_in.union(&self.failed_in).nth(1).is_some()
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
/// test's last [`WINDOW_PER_TEST`] invocations, signal-carrying
/// observations only (skips carry none).
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
        keep_last_invocations(&mut signal);
        classify(id, &signal, &mut analysis);
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
        .never_passed
        .sort_by(|a, b| b.failed_runs.cmp(&a.failed_runs).then(a.id.cmp(&b.id)));
    analysis
}

/// Cut a test's observations down to its last [`WINDOW_PER_TEST`]
/// invocations, keeping every run those made.
fn keep_last_invocations(signal: &mut Vec<&Observation>) {
    let mut seen: BTreeSet<Invocation> = BTreeSet::new();
    let mut first_kept = 0;
    for (line, observation) in signal.iter().copied().enumerate().rev() {
        let invocation = observation
            .invocation
            .as_deref()
            .map_or(Invocation::Line(line), Invocation::Run);
        if seen.len() == WINDOW_PER_TEST && !seen.contains(&invocation) {
            first_kept = line + 1;
            break;
        }
        seen.insert(invocation);
    }
    signal.drain(..first_kept);
}

/// File one test's window under what it proves: a flake, a regression, a
/// test never seen passing — or nothing.
fn classify(id: &str, signal: &[&Observation], analysis: &mut Analysis) {
    if signal.len() < 2 {
        return;
    }
    let failed_count = signal.iter().filter(|o| failed(o)).count();
    if failed_count == 0 {
        return;
    }

    // Flaky evidence: one clean commit with both outcomes, from at least
    // two invocations — a pass in one, a failure in another. Mixed
    // outcomes inside one invocation are the active pass's to judge.
    let mut per_clean_commit: BTreeMap<&str, Outcomes> = BTreeMap::new();
    for (line, observation) in signal.iter().enumerate() {
        if observation.dirty != Some(false) {
            continue;
        }
        if let Some(commit) = &observation.commit {
            let invocation = observation
                .invocation
                .as_deref()
                .map_or(Invocation::Line(line), Invocation::Run);
            let outcomes = per_clean_commit.entry(commit).or_default();
            if failed(observation) {
                outcomes.failed_in.insert(invocation);
            } else {
                outcomes.passed_in.insert(invocation);
            }
        }
    }
    let proven_flaky = per_clean_commit.values().any(Outcomes::prove_flaky);

    // The failures that close the window, newest first. Dirty reds count
    // in the streak; they just cannot carry its address. For a proven
    // flake the streak stops at a commit it was also seen passing on:
    // there a failure is the flake being a flake.
    let passed_on: BTreeSet<&str> = per_clean_commit
        .iter()
        .filter(|(_, outcomes)| !outcomes.passed_in.is_empty())
        .map(|(commit, _)| *commit)
        .collect();
    let streak: Vec<&Observation> = signal
        .iter()
        .rev()
        .copied()
        .take_while(|o| {
            failed(o) && !(proven_flaky && clean_commit(o).is_some_and(|c| passed_on.contains(c)))
        })
        .collect();
    let streak_invocations: BTreeSet<Invocation> = streak
        .iter()
        .enumerate()
        .map(|(line, o)| {
            o.invocation
                .as_deref()
                .map_or(Invocation::Line(line), Invocation::Run)
        })
        .collect();
    // Anchored on the streak's earliest *clean* red.
    let anchor = streak.iter().rev().find_map(|o| clean_commit(o));

    if proven_flaky {
        let red_since = anchor
            .filter(|_| streak_invocations.len() >= RED_STREAK_INVOCATIONS)
            .map(|commit| RedStreak {
                commit: commit.to_owned(),
                failed_runs: streak.len(),
            });
        analysis.flaky.push(HistoricFlake {
            outcomes: TestOutcomes {
                id: id.to_owned(),
                passed: signal.len() - failed_count,
                failed: failed_count,
            },
            failures_confined_to: failures_confined_to(signal),
            red_since,
        });
        return;
    }

    // A streak across at least two invocations. After green it is a
    // regression pointer; with nothing but red in the window, no commit
    // can be blamed and the test was simply never seen passing.
    if streak_invocations.len() < 2 {
        return;
    }
    let Some(commit) = anchor else {
        return;
    };
    if streak.len() == signal.len() {
        analysis.never_passed.push(NeverPassed {
            id: id.to_owned(),
            first_commit: commit.to_owned(),
            failed_runs: streak.len(),
        });
    } else {
        analysis.failing_since.push(FailingSince {
            id: id.to_owned(),
            commit: commit.to_owned(),
            failed_runs: streak.len(),
        });
    }
}

/// The commit an observation can testify about: known, and clean.
fn clean_commit(observation: &Observation) -> Option<&str> {
    if observation.dirty != Some(false) {
        return None;
    }
    observation.commit.as_deref()
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
            invocation: None,
            at_epoch_secs: 0,
        }
    }

    fn in_run(id: &str, status: TestStatus, commit: &str, run: &str) -> Observation {
        Observation {
            invocation: Some(run.to_owned()),
            ..clean(id, status, commit)
        }
    }

    #[test]
    fn mixed_outcomes_inside_one_invocation_prove_nothing_in_the_history() {
        // One `--runs 3` on one clean commit: fail, pass, fail. The active
        // pass judges that sequence; the history counts it as one piece of
        // evidence, so it cannot prove flaky on its own.
        let history = [
            in_run("c::t", TestStatus::Failed, "aaa", "r1"),
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Failed, "aaa", "r1"),
        ];
        assert!(analyze(&history).flaky.is_empty());
    }

    #[test]
    fn a_failure_streak_inside_one_invocation_is_no_regression_yet() {
        let history = [
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Failed, "bbb", "r2"),
            in_run("c::t", TestStatus::Failed, "bbb", "r2"),
        ];
        assert!(analyze(&history).failing_since.is_empty());

        let history = [
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Failed, "bbb", "r2"),
            in_run("c::t", TestStatus::Failed, "bbb", "r3"),
        ];
        assert_eq!(analyze(&history).failing_since[0].commit, "bbb");
    }

    #[test]
    fn a_pass_and_a_failure_in_two_invocations_prove_flaky() {
        let history = [
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Failed, "aaa", "r2"),
        ];
        let analysis = analyze(&history);
        assert_eq!(analysis.flaky.len(), 1);
        assert_eq!(analysis.flaky[0].outcomes.passed, 2);
        assert_eq!(analysis.flaky[0].outcomes.failed, 1);
    }

    fn clean(id: &str, status: TestStatus, commit: &str) -> Observation {
        obs(id, status, Some(commit), Some(false))
    }

    #[test]
    fn the_window_drops_the_oldest_observations_beyond_the_limit() {
        use crate::history::WINDOW_PER_TEST;
        // The lone failure is the oldest of WINDOW+1 observations, so the
        // window drops exactly it — and with it the only flaky evidence.
        let mut history = vec![clean("c::t", TestStatus::Failed, "aaa")];
        history.extend((0..WINDOW_PER_TEST).map(|_| clean("c::t", TestStatus::Passed, "aaa")));
        assert!(
            analyze(&history).is_empty(),
            "the failure must have left the window"
        );

        // One observation fewer and the same failure sits inside the window.
        let analysis = analyze(&history[..WINDOW_PER_TEST]);
        assert_eq!(analysis.flaky.len(), 1);
        assert_eq!(analysis.flaky[0].outcomes.failed, 1);
        assert_eq!(analysis.flaky[0].outcomes.passed, WINDOW_PER_TEST - 1);
    }

    #[test]
    fn a_proven_flake_survives_green_gates_of_twenty_runs() {
        // The gate recommends `--runs 20`. Three of those on the same commit
        // are sixty observations, but three invocations.
        let mut history = vec![
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Failed, "aaa", "r2"),
        ];
        for gate in ["g1", "g2", "g3"] {
            history.extend((0..20).map(|_| in_run("c::t", TestStatus::Passed, "aaa", gate)));
        }
        let analysis = analyze(&history);
        assert_eq!(analysis.flaky.len(), 1, "the proof left the window");
        assert_eq!(analysis.flaky[0].outcomes.failed, 1);
        assert_eq!(analysis.flaky[0].outcomes.passed, 61);
    }

    #[test]
    fn the_window_is_a_tests_last_invocations_with_all_their_runs() {
        use crate::history::WINDOW_PER_TEST;
        // The failure is the oldest invocation; each later one has two runs.
        let later = |count: usize| {
            let mut history = vec![
                in_run("c::t", TestStatus::Passed, "aaa", "first-pass"),
                in_run("c::t", TestStatus::Failed, "aaa", "the-failure"),
            ];
            for run in 0..count {
                let name = format!("later-{run}");
                history.push(in_run("c::t", TestStatus::Passed, "aaa", &name));
                history.push(in_run("c::t", TestStatus::Passed, "aaa", &name));
            }
            history
        };
        // WINDOW later invocations push the failure (and the pass before it) out.
        assert!(analyze(&later(WINDOW_PER_TEST)).is_empty());
        // One fewer and the failure is the oldest invocation inside.
        let analysis = analyze(&later(WINDOW_PER_TEST - 1));
        assert_eq!(analysis.flaky.len(), 1);
        assert_eq!(analysis.flaky[0].outcomes.passed, (WINDOW_PER_TEST - 1) * 2);
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
    fn a_test_never_seen_passing_is_not_a_regression() {
        // No commit broke it as far as the history knows: `aaa` is only
        // where sooth first looked.
        let history = [
            clean("c::t", TestStatus::Failed, "aaa"),
            clean("c::t", TestStatus::Failed, "bbb"),
        ];
        let analysis = analyze(&history);
        assert!(analysis.flaky.is_empty());
        assert!(analysis.failing_since.is_empty());
        assert_eq!(analysis.never_passed[0].first_commit, "aaa");
        assert_eq!(analysis.never_passed[0].failed_runs, 2);
    }

    /// Flaky on `aaa`, green on `bbb`, then `reds` red invocations on `ccc`.
    fn flake_then_reds_on_a_new_commit(reds: usize) -> Vec<Observation> {
        let mut history = vec![
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Failed, "aaa", "r2"),
            in_run("c::t", TestStatus::Passed, "bbb", "r3"),
            in_run("c::t", TestStatus::Passed, "bbb", "r4"),
        ];
        for run in 0..reds {
            history.push(in_run(
                "c::t",
                TestStatus::Failed,
                "ccc",
                &format!("c{run}"),
            ));
        }
        history
    }

    #[test]
    fn a_known_flake_red_on_every_run_since_a_commit_says_so() {
        let analysis = analyze(&flake_then_reds_on_a_new_commit(3));
        assert!(analysis.failing_since.is_empty());
        let streak = analysis.flaky[0].red_since.as_ref().expect("a red streak");
        assert_eq!(streak.commit, "ccc");
        assert_eq!(streak.failed_runs, 3);
    }

    #[test]
    fn two_red_invocations_are_what_a_flake_does_anyway() {
        let analysis = analyze(&flake_then_reds_on_a_new_commit(2));
        assert_eq!(analysis.flaky.len(), 1);
        assert_eq!(analysis.flaky[0].red_since, None);
    }

    #[test]
    fn a_flake_failing_on_a_commit_it_also_passed_on_has_no_red_streak() {
        let history = [
            in_run("c::t", TestStatus::Passed, "aaa", "r1"),
            in_run("c::t", TestStatus::Failed, "aaa", "r2"),
            in_run("c::t", TestStatus::Failed, "aaa", "r3"),
            in_run("c::t", TestStatus::Failed, "aaa", "r4"),
        ];
        assert_eq!(analyze(&history).flaky[0].red_since, None);
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
        assert_eq!(analysis.flaky[0].red_since, None);
    }
}
