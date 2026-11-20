//! The timing twin of "failing since" (see `DECISIONS.md`): a test whose
//! runs take longer since a commit than they did before it.
//!
//! A clock says less than an outcome does, so the evidence is narrower
//! than for the rest of the history: a pass on a clean, known commit, in an
//! invocation that ran the whole suite, compared only inside one
//! environment. One sooth invocation is one measurement, however many runs
//! it made.
//!
//! A clock cannot tell a slower test from a slower machine, and no factor
//! takes a machine out of a measurement: on one suite, measured, a busy
//! machine, efficiency cores and a debugger slowed tests by anything from
//! not at all to seventy times. So a test is named only when the other
//! tests kept their time over the runs of its commit. When a quarter of
//! them did not, the test is withheld, and one line says that tests got
//! slower and why they are not named. And every finding carries how many tests got
//! slower with it, and of how many: one of forty is that test, seventeen
//! of forty are not seventeen tests.

use std::collections::{BTreeMap, BTreeSet};

use crate::history::Observation;
use crate::junit::TestStatus;

/// How many times as long a test has to take before it is called slower.
/// With every core of the machine busy, the worst test of 5 ms and more in
/// a measured suite took 1.58 times as long as on an idle one.
pub const SLOWER_FACTOR: u64 = 2;

/// And how much longer, in milliseconds. One run of a 2 ms test was timed
/// at 11 ms: under about twice that, a clock says more about the machine
/// than about the test. It is also what a test has to take to be counted
/// among the tests a finding is one of.
pub const SLOWER_BY_MS: u64 = 20;

/// The invocations needed on each side of a commit. One slow run is a slow
/// run.
pub const RUNS_EACH_SIDE: usize = 3;

/// How far back the "before" reaches: the last invocations in front of the
/// commit, not everything the window holds. What a test took thirty runs
/// ago is not what it took before this commit.
pub const RUNS_BEFORE: usize = 5;

/// From how many tests on, slower at one commit, they are "many at once" —
/// when they are also more than a fifth of the tests that could have been.
pub const MANY_AT_ONCE: usize = 3;

/// How many other tests of [`SLOWER_BY_MS`] or more, timed on both sides
/// of a commit, it takes to read from them whether the clock moved. Fewer,
/// and one test is a large part of a quarter.
pub const TESTS_TO_READ_A_CLOCK: usize = 5;

/// How many times as long a quarter of those tests have to take, in
/// thousandths, for the clock to have moved. Measured, as the upper
/// quartile of their ratios: 1.04 between two stretches of idle runs, 1.01
/// across a commit that slowed one test, 1.13 with six cores busy — and
/// 1.30 under a tracing debugger, 1.41 with every core busy, 1.74 as CI
/// with every core busy, where the load alone made a test twice as slow,
/// 2.6 and 2.8 on efficiency cores.
pub const CLOCK_MOVED_PER_MILLE: u64 = 1250;

/// An invocation that ran fewer than nine tenths of the tests of the
/// largest one that timed a test is a part of the suite, not the suite.
const WHOLE_SUITE_TENTHS: usize = 9;

/// A test that takes longer since a commit, in one environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowerSince {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    /// The first commit of the slower runs. Where the clock changed, which
    /// is not always what changed it: see `DECISIONS.md`.
    pub commit: String,
    /// Where both sides were measured.
    pub environment: Option<String>,
    /// The median of the last invocations in front of the commit.
    pub before_ms: u64,
    /// The median of every invocation since.
    pub since_ms: u64,
    pub runs_before: usize,
    pub runs_since: usize,
    /// How many tests got slower at this commit in this environment, this
    /// one included.
    pub tests_slower: usize,
    /// Of how many: the tests timed there since the commit that take
    /// [`SLOWER_BY_MS`] or more, which are the ones a clock could name.
    pub tests_timed: usize,
}

impl SlowerSince {
    /// What one run of the test costs more than it did.
    pub fn gained_ms(&self) -> u64 {
        self.since_ms.saturating_sub(self.before_ms)
    }

    /// Whether this test got slower in such company that the company is
    /// the finding: three tests or more, and more than a fifth of those a
    /// clock could have named. A machine on battery, a debugger, a fixture
    /// every test shares — or that many tests, each on its own. The clock
    /// reads the same for all of them.
    pub fn many_at_once(&self) -> bool {
        self.tests_slower >= MANY_AT_ONCE && self.tests_slower * 5 > self.tests_timed
    }
}

/// Tests that got slower since a commit by the rule and are not named,
/// because the clock they were timed on moved with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withheld {
    /// The commit the tests would have been named with.
    pub commit: String,
    pub environment: Option<String>,
    /// How many tests.
    pub tests: usize,
    /// How many times as long a quarter of the other tests took over the
    /// same invocations, at least, in thousandths.
    pub times_per_mille: u64,
    /// How many other tests that was read from.
    pub tests_read: usize,
}

/// What the slow pass found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Slower {
    pub tests: Vec<SlowerSince>,
    pub withheld: Vec<Withheld>,
}

/// One invocation's measurement of one test.
#[derive(Clone, Copy)]
struct Timing<'a> {
    commit: &'a str,
    /// When the invocation was made, in the history's seconds.
    at: u64,
    ms: u64,
}

/// What one environment's invocations timed, per test, in the order they
/// were made.
type Clock<'a> = BTreeMap<&'a str, Vec<Timing<'a>>>;

/// The tests that take longer since a commit — one finding per test and
/// environment at most — and those that do and are not named. `windows`
/// is each test's window of the history in time order, `sizes` the number
/// of tests each invocation ran.
pub fn slower_since(
    windows: &BTreeMap<&str, Vec<&Observation>>,
    sizes: &BTreeMap<&str, usize>,
) -> Slower {
    let mut clocks: BTreeMap<Option<&str>, Clock> = BTreeMap::new();
    for (id, window) in windows {
        for (environment, timings) in timings(window, sizes) {
            clocks.entry(environment).or_default().insert(id, timings);
        }
    }
    let mut found = Slower::default();
    for (environment, clock) in &clocks {
        let mut readings = Readings::of(clock);
        // Per commit, the tests named with it — and those withheld, each
        // with what the clock did under it.
        let mut named: BTreeMap<&str, Vec<SlowerSince>> = BTreeMap::new();
        let mut withheld: BTreeMap<&str, Vec<Moved>> = BTreeMap::new();
        for (id, timings) in clock {
            match since_a_commit(id, *environment, timings, &mut readings) {
                Some(Since::Named(finding, commit)) => {
                    named.entry(commit).or_default().push(finding);
                }
                Some(Since::Withheld(commit, moved)) => {
                    withheld.entry(commit).or_default().push(moved);
                }
                None => {}
            }
        }
        for (commit, together) in named {
            let tests_timed = readings
                .began(commit)
                .map_or(0, |began| timed_since(clock, began));
            let tests_slower = together.len();
            found
                .tests
                .extend(together.into_iter().map(|finding| SlowerSince {
                    tests_slower,
                    tests_timed,
                    ..finding
                }));
        }
        for (commit, moved) in withheld {
            // What holds for every one of them: the least any of their
            // clocks moved, read from the fewest tests.
            found.withheld.push(Withheld {
                commit: commit.to_owned(),
                environment: environment.map(str::to_owned),
                tests: moved.len(),
                times_per_mille: moved
                    .iter()
                    .map(|moved| moved.times_per_mille)
                    .min()
                    .unwrap_or(0),
                tests_read: moved
                    .iter()
                    .map(|moved| moved.tests_read)
                    .min()
                    .unwrap_or(0),
            });
        }
    }
    found
}

/// Twice as long, and 20 ms more.
fn is_slower(before_ms: u64, since_ms: u64) -> bool {
    since_ms >= SLOWER_FACTOR.saturating_mul(before_ms.max(1))
        && since_ms.saturating_sub(before_ms) >= SLOWER_BY_MS
}

/// The invocations of one test from `began` on.
fn from<'a, 'b>(timings: &'b [Timing<'a>], began: u64) -> &'b [Timing<'a>] {
    &timings[timings.partition_point(|timing| timing.at < began)..]
}

/// How many tests of one environment a clock could have named since
/// `began`: timed in three invocations from then on, at a median of
/// [`SLOWER_BY_MS`] or more. A test faster than that cannot be 20 ms
/// slower than anything.
fn timed_since(clock: &Clock<'_>, began: u64) -> usize {
    clock
        .values()
        .map(|timings| from(timings, began))
        .filter(|since| since.len() >= RUNS_EACH_SIDE && median_of(since) >= SLOWER_BY_MS)
        .count()
}

/// A clock that moved under a test: by how much, and read from how many.
#[derive(Clone, Copy)]
struct Moved {
    times_per_mille: u64,
    tests_read: usize,
}

/// The clock of one environment, read once per commit: where each commit's
/// runs begin and end, and what the tests did over them.
struct Readings<'a, 'b> {
    clock: &'b Clock<'a>,
    /// When each commit was first run, by any test.
    firsts: BTreeMap<&'a str, u64>,
    /// Per reading, the ratios of the tests that can say, sorted.
    read: BTreeMap<Reading, Vec<(u64, &'a str)>>,
}

/// One reading of a clock: the commit's first run, before which the
/// before ends, and the invocations read over.
type Reading = (u64, Vec<u64>);

impl<'a, 'b> Readings<'a, 'b> {
    fn of(clock: &'b Clock<'a>) -> Self {
        let mut firsts: BTreeMap<&str, u64> = BTreeMap::new();
        for timing in clock.values().flatten() {
            let first = firsts.entry(timing.commit).or_insert(timing.at);
            *first = (*first).min(timing.at);
        }
        Self {
            clock,
            firsts,
            read: BTreeMap::new(),
        }
    }

    /// When `commit` was first run on this clock, by any test.
    fn began(&self, commit: &str) -> Option<u64> {
        self.firsts.get(commit).copied()
    }

    /// The invocations the clock under a test is read over: those it was
    /// timed in on its commit, `since` being its runs from its first one
    /// there — up to the first run of the next commit — and when those are
    /// fewer than three, its first three from there on. Runs it was not in
    /// are not read: a machine that changed while it sat out is not its
    /// clock.
    fn during(&self, since: &[Timing<'_>]) -> Vec<u64> {
        let start = since.first().map_or(0, |timing| timing.at);
        let next = self
            .firsts
            .values()
            .copied()
            .filter(|&first| first > start)
            .min()
            .unwrap_or(u64::MAX);
        let on_commit: Vec<u64> = since
            .iter()
            .map(|timing| timing.at)
            .take_while(|&at| at < next)
            .collect();
        if on_commit.len() >= RUNS_EACH_SIDE {
            on_commit
        } else {
            since
                .iter()
                .map(|timing| timing.at)
                .take(RUNS_EACH_SIDE)
                .collect()
        }
    }

    /// Whether the clock moved under `commit` for the test `id`, whose runs
    /// from its first one there on are `since`: the other tests are read
    /// over the invocations the test was timed in (see [`Readings::during`])
    /// against their runs before the commit's first run by any test.
    /// Before the commit, so that runs of the commit itself, made on a
    /// machine that was already slower, are not the before; over the
    /// test's own invocations, so that a machine that changed partway
    /// through the commit is read where the test was timed, and not
    /// averaged away by runs it was not in.
    ///
    /// The others that took [`SLOWER_BY_MS`] or more before — which a
    /// slower machine cannot fail to slow — timed in three of those
    /// invocations and in three before, are laid out by their ratio, the
    /// median over those invocations against the median of the last five
    /// before, and the one a quarter from the top is read: when it is
    /// [`CLOCK_MOVED_PER_MILLE`] or more, a quarter of the suite got slower
    /// with the test, and that is not the test.
    ///
    /// A quarter, and not the typical test: a busy machine slows the tests
    /// that compute and leaves the ones that wait alone, and half of a
    /// suite may wait. `None` when the clock held, and when fewer than
    /// [`TESTS_TO_READ_A_CLOCK`] tests can say.
    fn moved(&mut self, commit: &str, since: &[Timing<'_>], id: &str) -> Option<Moved> {
        let began = self.began(commit)?;
        let key = (began, self.during(since));
        if !self.read.contains_key(&key) {
            let ratios = self.ratios(began, &key.1);
            self.read.insert(key.clone(), ratios);
        }
        let ratios = self.read.get(&key)?;
        let own = ratios.iter().position(|(_, test)| *test == id);
        let others = ratios.len() - usize::from(own.is_some());
        if others < TESTS_TO_READ_A_CLOCK {
            return None;
        }
        // A quarter from the top of the others: past the test's own place,
        // one further along.
        let quarter = others * 3 / 4;
        let at = if own.is_some_and(|own| own <= quarter) {
            quarter + 1
        } else {
            quarter
        };
        let times_per_mille = ratios[at].0;
        (times_per_mille >= CLOCK_MOVED_PER_MILLE).then_some(Moved {
            times_per_mille,
            tests_read: others,
        })
    }

    /// Every test's ratio over the invocations `during`, against its runs
    /// before `began`, for those that can say, sorted.
    fn ratios(&self, began: u64, during: &[u64]) -> Vec<(u64, &'a str)> {
        let mut ratios: Vec<(u64, &'a str)> = self
            .clock
            .iter()
            .filter_map(|(test, timings)| {
                let ahead = timings.partition_point(|timing| timing.at < began);
                let before = &timings[ahead.saturating_sub(RUNS_BEFORE)..ahead];
                let read: Vec<Timing> = timings
                    .iter()
                    .filter(|timing| during.binary_search(&timing.at).is_ok())
                    .copied()
                    .collect();
                if read.len() < RUNS_EACH_SIDE || before.len() < RUNS_EACH_SIDE {
                    return None;
                }
                let before_ms = median_of(before);
                (before_ms >= SLOWER_BY_MS).then(|| {
                    let times = u128::from(median_of(&read)) * 1000 / u128::from(before_ms);
                    (u64::try_from(times).unwrap_or(u64::MAX), *test)
                })
            })
            .collect();
        ratios.sort_unstable();
        ratios
    }
}

/// One invocation's runs of a test, before they are one measurement.
struct Invocation<'a> {
    name: &'a str,
    commit: &'a str,
    at: u64,
    runs_ms: Vec<u64>,
}

/// Per environment, the invocations that timed a pass of one test on a
/// clean commit while running the whole suite, in the order they were
/// made. An invocation that ran the test several times is one measurement:
/// the median of its runs.
///
/// A part of the suite is no measurement of a test in it. What a test
/// costs depends on what ran before it: the first test to need a fixture
/// pays for it, and run alone every test is the first.
fn timings<'a>(
    window: &[&'a Observation],
    sizes: &BTreeMap<&str, usize>,
) -> BTreeMap<Option<&'a str>, Vec<Timing<'a>>> {
    // The runs of one invocation stand together in the window, so a new
    // invocation is one that differs from the last. A line without one is
    // from before durations were recorded, or not sooth's own.
    let mut made: BTreeMap<Option<&str>, Vec<Invocation>> = BTreeMap::new();
    for observation in window {
        if observation.status != TestStatus::Passed || !observation.is_evidence() {
            continue;
        }
        let (Some(commit), Some(ms), Some(name)) = (
            observation.commit.as_deref(),
            observation.duration_ms,
            observation.invocation.as_deref(),
        ) else {
            continue;
        };
        let here = made.entry(observation.environment.as_deref()).or_default();
        match here.last_mut() {
            Some(last) if last.name == name => last.runs_ms.push(ms),
            _ => here.push(Invocation {
                name,
                commit,
                at: observation.at_epoch_secs,
                runs_ms: vec![ms],
            }),
        }
    }
    made.into_iter()
        .map(|(environment, invocations)| {
            let size = |invocation: &Invocation| sizes.get(invocation.name).copied().unwrap_or(0);
            let whole = invocations.iter().map(size).max().unwrap_or(0);
            let timings = invocations
                .into_iter()
                .filter(|invocation| size(invocation) * 10 >= whole * WHOLE_SUITE_TENTHS)
                .map(|mut invocation| Timing {
                    commit: invocation.commit,
                    at: invocation.at,
                    ms: median(&mut invocation.runs_ms),
                })
                .collect();
            (environment, timings)
        })
        .collect()
}

/// The middle of `values`, which must not be empty; of an even number, the
/// point halfway between the two in the middle.
fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        values[middle - 1].midpoint(values[middle])
    }
}

fn median_of(timings: &[Timing<'_>]) -> u64 {
    median(&mut timings.iter().map(|timing| timing.ms).collect::<Vec<_>>())
}

/// What the rule makes of one test.
enum Since<'a> {
    /// Slower since this commit.
    Named(SlowerSince, &'a str),
    /// Slower since this commit by its own times, on a clock that moved.
    Withheld(&'a str, Moved),
}

/// The commit since which a test takes longer, if the rule holds for one:
///
/// - the commit was not seen before, and no commit that was is among the
///   runs since: code that ran on both sides did not change the clock;
/// - three invocations on each side;
/// - no invocation since as fast as one of the last five before;
/// - all those since take twice as long as those five and 20 ms more, by
///   their medians;
/// - and the other tests kept their time over the commit's own runs (see
///   [`Readings::moved`]).
///
/// When that holds for more than one commit, the test got slower in steps,
/// and the commit of its largest step is named: the earliest of them when
/// two are as large. A commit's step is what its own runs — those up to
/// the next commit — took more than the runs in front of it. When it holds
/// for none only because the clock moved, the test is withheld, for the
/// commit of its largest step.
fn since_a_commit<'a>(
    id: &str,
    environment: Option<&str>,
    timings: &[Timing<'a>],
    readings: &mut Readings<'a, '_>,
) -> Option<Since<'a>> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let new_commits: Vec<usize> = (0..timings.len())
        .filter(|&at| seen.insert(timings[at].commit))
        .collect();
    let mut named: Option<(u64, SlowerSince, &str)> = None;
    let mut withheld: Option<(u64, &str, Moved)> = None;
    for (nth, &at) in new_commits.iter().enumerate() {
        let since = &timings[at..];
        if since.len() < RUNS_EACH_SIDE {
            break;
        }
        let before = &timings[at.saturating_sub(RUNS_BEFORE)..at];
        if before.len() < RUNS_EACH_SIDE {
            continue;
        }
        let ran_before: BTreeSet<&str> = timings[..at].iter().map(|timing| timing.commit).collect();
        if since
            .iter()
            .any(|timing| ran_before.contains(timing.commit))
        {
            continue;
        }
        let fastest_since = since.iter().map(|timing| timing.ms).min()?;
        let slowest_before = before.iter().map(|timing| timing.ms).max()?;
        if fastest_since <= slowest_before {
            continue;
        }
        let (before_ms, since_ms) = (median_of(before), median_of(since));
        if !is_slower(before_ms, since_ms) {
            continue;
        }
        let next_commit = new_commits.get(nth + 1).copied().unwrap_or(timings.len());
        let step_ms = median_of(&timings[at..next_commit]).saturating_sub(before_ms);
        let commit = timings[at].commit;
        if let Some(moved) = readings.moved(commit, since, id) {
            if withheld.as_ref().is_none_or(|(step, ..)| step_ms > *step) {
                withheld = Some((step_ms, commit, moved));
            }
        } else if named.as_ref().is_none_or(|(step, ..)| step_ms > *step) {
            let finding = SlowerSince {
                id: id.to_owned(),
                commit: commit.to_owned(),
                environment: environment.map(str::to_owned),
                before_ms,
                since_ms,
                runs_before: before.len(),
                runs_since: since.len(),
                tests_slower: 1,
                tests_timed: 1,
            };
            named = Some((step_ms, finding, commit));
        }
    }
    match (named, withheld) {
        (Some((_, finding, commit)), _) => Some(Since::Named(finding, commit)),
        (None, Some((_, commit, moved))) => Some(Since::Withheld(commit, moved)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{slower_since, Slower, SlowerSince, Withheld};
    use crate::history::Observation;
    use crate::junit::TestStatus;

    /// One passing, timed observation of `id` on a clean commit, made
    /// locally by the invocation `run`.
    fn timed(id: &str, commit: &str, run: usize, ms: u64) -> Observation {
        Observation {
            id: id.to_owned(),
            status: TestStatus::Passed,
            commit: Some(commit.to_owned()),
            dirty: Some(false),
            environment: Some("local".to_owned()),
            invocation: Some(format!("r{run}")),
            duration_ms: Some(ms),
            at_epoch_secs: run as u64,
        }
    }

    /// What one test took: per commit, the duration each invocation timed.
    type Commits<'a> = &'a [(&'a str, &'a [u64])];

    /// A history of several tests, one invocation after the other. Tests
    /// with the same number of durations were timed by the same
    /// invocations.
    fn suite(tests: &[(&str, Commits)]) -> Vec<Observation> {
        let mut observations = Vec::new();
        for (id, commits) in tests {
            let mut run = 0;
            for (commit, durations) in *commits {
                for ms in *durations {
                    run += 1;
                    observations.push(timed(id, commit, run, *ms));
                }
            }
        }
        observations
    }

    /// The same for the one test `c::t`.
    fn history(commits: Commits) -> Vec<Observation> {
        suite(&[("c::t", commits)])
    }

    /// What the pass finds in `observations`, given as the history hands
    /// them over: per test in time order, with the size of each invocation.
    fn found_all(observations: &[Observation]) -> Slower {
        let mut windows: BTreeMap<&str, Vec<&Observation>> = BTreeMap::new();
        let mut ran: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for observation in observations {
            windows
                .entry(&observation.id)
                .or_default()
                .push(observation);
            if let Some(invocation) = observation.invocation.as_deref() {
                ran.entry(invocation).or_default().insert(&observation.id);
            }
        }
        let sizes = ran.into_iter().map(|(run, ids)| (run, ids.len())).collect();
        slower_since(&windows, &sizes)
    }

    /// The tests the pass names.
    fn found(observations: &[Observation]) -> Vec<SlowerSince> {
        found_all(observations).tests
    }

    /// The commit named for `id`, with the medians on both sides.
    fn named_in(observations: &[Observation], id: &str) -> Option<(String, u64, u64)> {
        let mut findings: Vec<SlowerSince> = found(observations)
            .into_iter()
            .filter(|finding| finding.id == id)
            .collect();
        assert!(findings.len() <= 1, "one environment, one finding at most");
        findings
            .pop()
            .map(|finding| (finding.commit, finding.before_ms, finding.since_ms))
    }

    fn named(commits: Commits) -> Option<(String, u64, u64)> {
        named_in(&history(commits), "c::t")
    }

    fn at(commit: &str, before_ms: u64, since_ms: u64) -> (String, u64, u64) {
        (commit.to_owned(), before_ms, since_ms)
    }

    #[test]
    fn a_test_that_takes_twice_as_long_since_a_commit_is_named_with_it() {
        let observations = history(&[("aaa", &[100, 110, 90]), ("bbb", &[300, 310, 290])]);
        assert_eq!(
            found(&observations),
            vec![SlowerSince {
                id: "c::t".to_owned(),
                commit: "bbb".to_owned(),
                environment: Some("local".to_owned()),
                before_ms: 100,
                since_ms: 300,
                runs_before: 3,
                runs_since: 3,
                tests_slower: 1,
                tests_timed: 1,
            }]
        );
    }

    #[test]
    fn fewer_than_three_invocations_on_a_side_name_nothing() {
        assert_eq!(
            named(&[("aaa", &[100, 100]), ("bbb", &[300, 300, 300])]),
            None
        );
        assert_eq!(
            named(&[("aaa", &[100, 100, 100]), ("bbb", &[300, 300])]),
            None
        );
        // The third run since is what makes it a finding.
        assert!(named(&[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])]).is_some());
    }

    #[test]
    fn slower_means_twice_as_long_and_twenty_milliseconds_more() {
        // 1.9 times as long is the machine as likely as the test.
        assert_eq!(
            named(&[("aaa", &[100, 100, 100]), ("bbb", &[190, 190, 190])]),
            None
        );
        // Almost five times as long, and 19 ms: a clock's doing as well.
        assert_eq!(named(&[("aaa", &[5, 5, 5]), ("bbb", &[24, 24, 24])]), None);
        assert_eq!(
            named(&[("aaa", &[5, 5, 5]), ("bbb", &[25, 25, 25])]),
            Some(at("bbb", 5, 25))
        );
        // A test under a millisecond has a before of zero, and a factor of
        // anything: the 20 ms decide.
        assert_eq!(named(&[("aaa", &[0, 0, 0]), ("bbb", &[19, 19, 19])]), None);
        assert_eq!(
            named(&[("aaa", &[0, 0, 0]), ("bbb", &[300, 300, 300])]),
            Some(at("bbb", 0, 300))
        );
    }

    #[test]
    fn one_run_since_as_fast_as_a_run_before_names_nothing() {
        // One of the runs before was as slow as the fastest since: the two
        // sides overlap, whatever their medians say.
        assert_eq!(
            named(&[
                ("aaa", &[100, 100, 250, 100, 100]),
                ("bbb", &[300, 240, 310])
            ]),
            None
        );
        // Only the last five before the commit are the before: a slow run
        // older than that is not held against the finding.
        assert_eq!(
            named(&[
                ("aaa", &[250, 100, 100, 100, 100, 100]),
                ("bbb", &[300, 240, 310])
            ]),
            Some(at("bbb", 100, 300))
        );
    }

    #[test]
    fn a_test_that_got_fast_again_is_not_named() {
        assert_eq!(
            named(&[
                ("aaa", &[100, 100, 100]),
                ("bbb", &[300, 300, 300]),
                ("ccc", &[100, 100, 100])
            ]),
            None
        );
    }

    #[test]
    fn what_the_test_takes_since_is_the_median_of_every_run_since() {
        // Slower at `bbb` and no slower still on the six runs after it: one
        // commit, and the newer one that changed nothing is not named.
        assert_eq!(
            named(&[
                ("aaa", &[100, 100, 100]),
                ("bbb", &[300, 300, 300]),
                ("ccc", &[300, 310, 320, 330, 340, 350])
            ]),
            Some(at("bbb", 100, 310))
        );
        // A few runs that were something else — a debugger on, say — are a
        // few runs, and the line does not read as if they were the test.
        assert_eq!(
            named(&[
                ("aaa", &[100, 100, 100]),
                (
                    "bbb",
                    &[300, 300, 300, 300, 300, 20_000, 20_000, 20_000, 20_000]
                )
            ]),
            Some(at("bbb", 100, 300))
        );
    }

    #[test]
    fn a_test_that_got_slower_in_steps_is_named_for_its_largest() {
        // A little slower at `bbb` and much slower at `ccc`: the rule holds
        // for both, and `ccc` is where to look — however few runs `bbb`
        // had, which only decide what the five runs before `ccc` were.
        assert_eq!(
            named(&[
                ("aaa", &[4, 4, 4]),
                ("bbb", &[15, 15, 15]),
                ("ccc", &[284, 284, 284])
            ]),
            Some(at("ccc", 15, 284))
        );
        for runs_between in [&[15][..], &[15, 15]] {
            assert_eq!(
                named(&[
                    ("aaa", &[4, 4, 4]),
                    ("bbb", runs_between),
                    ("ccc", &[284, 284, 284])
                ]),
                Some(at("ccc", 4, 284)),
                "{runs_between:?}"
            );
        }
        // Two steps, neither of them twice as long with 20 ms more against
        // the runs on the commit before, and together both. With three runs
        // in between, `ccc` is 36 ms against 19 and no finding of its own:
        // the commit it began at is named.
        for runs_between in [&[19, 19, 19][..], &[19, 19, 19, 19]] {
            assert_eq!(
                named(&[
                    ("aaa", &[10, 10, 10]),
                    ("bbb", runs_between),
                    ("ccc", &[36, 36, 36, 36, 36])
                ]),
                Some(at("bbb", 10, 36)),
                "{runs_between:?}"
            );
        }
        // Two steps of the same size, each a finding of its own: the
        // earlier commit is named, with every run since.
        assert_eq!(
            named(&[
                ("aaa", &[0, 0, 0]),
                ("bbb", &[100, 100, 100]),
                ("ccc", &[200, 200, 200])
            ]),
            Some(at("bbb", 0, 150))
        );
        // With two, the five runs before `ccc` are mostly the old ones,
        // and `ccc` is 36 ms against 10 with the larger step. Either line
        // is true of the clock; which one is read depends on those runs.
        assert_eq!(
            named(&[
                ("aaa", &[10, 10, 10]),
                ("bbb", &[19, 19]),
                ("ccc", &[36, 36, 36, 36, 36])
            ]),
            Some(at("ccc", 10, 36))
        );
    }

    #[test]
    fn the_commit_it_began_at_is_named_however_few_runs_that_one_had() {
        // One run on `bbb`, where it began, and five on `ccc`: the run on
        // `bbb` was slow already, so `ccc` did not do it.
        assert_eq!(
            named(&[
                ("aaa", &[100, 100, 100, 100, 100]),
                ("bbb", &[200]),
                ("ccc", &[200, 200, 200, 200, 200])
            ]),
            Some(at("bbb", 100, 200))
        );
        // And with one run each on three commits since.
        assert_eq!(
            named(&[
                ("aaa", &[100, 100, 100]),
                ("bbb", &[300]),
                ("ccc", &[300]),
                ("ddd", &[300])
            ]),
            Some(at("bbb", 100, 300))
        );
    }

    #[test]
    fn a_commit_that_ran_on_both_sides_did_not_change_the_clock() {
        // Back on `aaa` after a look at `bbb`, and `aaa` is slow now: it
        // was fast on this very code, so the code is not what changed.
        assert_eq!(
            named(&[
                ("aaa", &[10, 10, 10]),
                ("bbb", &[10, 10]),
                ("aaa", &[500, 500, 500])
            ]),
            None
        );
        // Slower from `bbb` on, and then as slow back on `aaa`: the same.
        assert_eq!(
            named(&[
                ("aaa", &[10, 10, 10]),
                ("bbb", &[500, 500, 500]),
                ("aaa", &[500, 500])
            ]),
            None
        );
        // A commit that comes back among the runs since is no objection.
        assert_eq!(
            named(&[
                ("aaa", &[10, 10, 10]),
                ("bbb", &[500, 500]),
                ("ccc", &[500]),
                ("bbb", &[500])
            ]),
            Some(at("bbb", 10, 500))
        );
    }

    #[test]
    fn the_runs_of_one_invocation_are_one_measurement() {
        // `--runs 3` on `bbb`, once: three slow runs, one invocation.
        let mut observations = history(&[("aaa", &[100, 100, 100])]);
        for ms in [300, 300, 300] {
            observations.push(timed("c::t", "bbb", 4, ms));
        }
        assert_eq!(found(&observations), Vec::new());
        // Two more invocations, and each counts with the median of its
        // runs: a single slow run among them moves nothing.
        observations.extend([timed("c::t", "bbb", 5, 310), timed("c::t", "bbb", 6, 290)]);
        observations.extend([timed("c::t", "bbb", 6, 300), timed("c::t", "bbb", 6, 9_000)]);
        let findings = found(&observations);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            (findings[0].since_ms, findings[0].runs_since),
            (300, 3),
            "{findings:?}"
        );
    }

    #[test]
    fn the_median_of_an_even_number_lies_between_the_two_in_the_middle() {
        // Four invocations on each side: neither side has a middle one.
        assert_eq!(
            named(&[("aaa", &[10, 20, 40, 80]), ("bbb", &[300, 320, 340, 360])]),
            Some(at("bbb", 30, 330))
        );
        // And inside an invocation: two runs of 100 and 300 ms are one
        // measurement of 200.
        let mut observations = history(&[("aaa", &[10, 10, 10])]);
        for run in 4..=6 {
            observations.extend([
                timed("c::t", "bbb", run, 100),
                timed("c::t", "bbb", run, 300),
            ]);
        }
        let findings = found(&observations);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].since_ms, 200);
    }

    #[test]
    fn environments_are_different_clocks() {
        // Fast locally on `aaa`, slow in CI on `bbb`: nothing was measured
        // twice on one clock.
        let mut observations = history(&[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])]);
        for observation in &mut observations[3..] {
            observation.environment = Some("ci".to_owned());
        }
        assert_eq!(found(&observations), Vec::new());
        // The same commit, measured in both: named once for each.
        let local = history(&[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])]);
        let mut ci = history(&[("aaa", &[400, 400, 400]), ("bbb", &[900, 900, 900])]);
        for observation in &mut ci {
            observation.environment = Some("ci".to_owned());
            observation.invocation = observation.invocation.take().map(|run| format!("ci-{run}"));
        }
        let both: Vec<Observation> = local.into_iter().chain(ci).collect();
        let findings = found(&both);
        let places: Vec<(Option<&str>, u64, u64)> = findings
            .iter()
            .map(|finding| {
                (
                    finding.environment.as_deref(),
                    finding.before_ms,
                    finding.since_ms,
                )
            })
            .collect();
        assert_eq!(places, [(Some("ci"), 400, 900), (Some("local"), 100, 300)]);
    }

    #[test]
    fn only_a_timed_pass_on_a_clean_commit_is_a_measurement() {
        let slow = |change: fn(&mut Observation)| {
            let mut observations = history(&[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])]);
            for observation in &mut observations[3..] {
                change(observation);
            }
            found(&observations)
        };
        // A dirty tree is not the commit, a failure is not the test doing
        // its work, a report without a duration timed nothing, and a line
        // without an invocation cannot be laid beside the rest of its run.
        assert_eq!(slow(|o| o.dirty = Some(true)), Vec::new());
        assert_eq!(slow(|o| o.dirty = None), Vec::new());
        assert_eq!(slow(|o| o.status = TestStatus::Failed), Vec::new());
        assert_eq!(slow(|o| o.duration_ms = None), Vec::new());
        assert_eq!(slow(|o| o.commit = None), Vec::new());
        assert_eq!(slow(|o| o.invocation = None), Vec::new());
        assert_eq!(slow(|_| {}).len(), 1);
    }

    #[test]
    fn a_part_of_the_suite_is_no_measurement_of_a_test_in_it() {
        // Five runs of two tests, then three of `c::t` alone, where it
        // pays for a fixture the other one used to build first. Nothing
        // about it changed but its company.
        let alone = suite(&[
            ("c::t", &[("aaa", &[0, 0, 0, 0, 0]), ("bbb", &[21, 21, 21])]),
            ("c::other", &[("aaa", &[21, 21, 21, 21, 21])]),
        ]);
        assert_eq!(named_in(&alone, "c::t"), None);
        // In the company it always had, the same clock is a finding.
        let together = suite(&[
            ("c::t", &[("aaa", &[0, 0, 0, 0, 0]), ("bbb", &[21, 21, 21])]),
            (
                "c::other",
                &[("aaa", &[2, 2, 2, 2, 2]), ("bbb", &[2, 2, 2])],
            ),
        ]);
        assert_eq!(named_in(&together, "c::t"), Some(at("bbb", 0, 21)));
        // Nine tests of ten is the suite still: one test less is not what
        // makes a run a part.
        let mut most: Vec<(&str, Commits)> =
            vec![("c::t", &[("aaa", &[0, 0, 0, 0, 0]), ("bbb", &[21, 21, 21])])];
        let ids = [
            "c::1", "c::2", "c::3", "c::4", "c::5", "c::6", "c::7", "c::8",
        ];
        for id in ids {
            most.push((id, &[("aaa", &[1, 1, 1, 1, 1]), ("bbb", &[1, 1, 1])]));
        }
        most.push(("c::gone", &[("aaa", &[1, 1, 1, 1, 1])]));
        assert_eq!(named_in(&suite(&most), "c::t"), Some(at("bbb", 0, 21)));
        // Nine of eleven is a part.
        most.push(("c::gone-too", &[("aaa", &[1, 1, 1, 1, 1])]));
        assert_eq!(named_in(&suite(&most), "c::t"), None);
    }

    /// `c::t` going from 100 ms to 300 ms at `bbb`, beside `others`.
    fn beside(others: &[(&str, Commits)]) -> Vec<SlowerSince> {
        let tripled: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])];
        let mut tests = vec![("c::t", tripled)];
        tests.extend_from_slice(others);
        found(&suite(&tests))
    }

    /// How many tests got slower with `c::t`, of how many, and whether
    /// that is many at once.
    fn company(findings: &[SlowerSince]) -> (usize, usize, bool) {
        let own = findings
            .iter()
            .find(|finding| finding.id == "c::t")
            .expect("c::t is slower");
        (own.tests_slower, own.tests_timed, own.many_at_once())
    }

    #[test]
    fn a_finding_says_how_many_tests_got_slower_with_it_and_of_how_many() {
        let steady: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[50, 50, 50])];
        let quick: Commits = &[("aaa", &[5, 5, 5]), ("bbb", &[5, 5, 5])];
        // One test of five that a clock could name: the four others take
        // 20 ms or more and did not move. Tests faster than that are not
        // counted — they could not have been 20 ms slower than anything.
        let findings = beside(&[
            ("c::1", steady),
            ("c::2", steady),
            ("c::3", steady),
            ("c::4", steady),
            ("c::5", quick),
            ("c::6", quick),
        ]);
        assert_eq!(findings.len(), 1);
        assert_eq!(company(&findings), (1, 5, false));
        // Alone in its suite, it is one of one, and still one test.
        assert_eq!(company(&beside(&[])), (1, 1, false));
    }

    #[test]
    fn many_tests_slower_at_once_are_not_as_many_findings() {
        // On battery every test takes three times as long. Each of them is
        // slower by the rule, and all of them say so of each other.
        let tripled: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[150, 150, 150])];
        let findings = beside(&[("c::x", tripled), ("c::y", tripled), ("c::z", tripled)]);
        assert_eq!(findings.len(), 4);
        assert!(findings.iter().all(SlowerSince::many_at_once));
        assert_eq!(company(&findings), (4, 4, true));
        // Two tests are two tests, whatever else there is.
        assert_eq!(company(&beside(&[("c::x", tripled)])), (2, 2, false));
        // Three of fifteen are a fifth, and not more: three findings. One
        // steady test fewer, and the three are many at once.
        let steady: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[50, 50, 50])];
        let names = [
            "s01", "s02", "s03", "s04", "s05", "s06", "s07", "s08", "s09", "s10", "s11", "s12",
        ];
        let mut others: Vec<(&str, Commits)> = vec![("c::x", tripled), ("c::y", tripled)];
        others.extend(names.iter().map(|name| (*name, steady)));
        assert_eq!(company(&beside(&others)), (3, 15, false));
        others.pop();
        assert_eq!(company(&beside(&others)), (3, 14, true));
    }

    /// `own` beside five tests timed like `others`: enough of them to read
    /// a machine from.
    fn among_five(own: Commits, others: Commits) -> Slower {
        found_all(&suite(&[
            ("c::t", own),
            ("c::1", others),
            ("c::2", others),
            ("c::3", others),
            ("c::4", others),
            ("c::5", others),
        ]))
    }

    /// What is withheld at `bbb`, locally: how many tests, for a clock
    /// that moved how much, read from how many.
    fn withheld(tests: usize, times_per_mille: u64, tests_read: usize) -> Vec<Withheld> {
        vec![Withheld {
            commit: "bbb".to_owned(),
            environment: Some("local".to_owned()),
            tests,
            times_per_mille,
            tests_read,
        }]
    }

    #[test]
    fn a_test_is_not_named_when_the_clock_moved_with_it() {
        // Six tests, each three times as long from `bbb` on: a laptop on
        // battery, a debugger, a fixture they all share. Each is slower by
        // the rule, the five others say the clock moved, and no test is
        // named: six are withheld, and that is said once.
        let own: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])];
        let tripled: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[150, 150, 150])];
        let slower = among_five(own, tripled);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld, withheld(6, 3000, 5));
        // A test that got slower still is not named either: the clock it
        // would be named against is not the one it was timed on before.
        let far: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[3000, 3000, 3000])];
        let slower = among_five(far, tripled);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld, withheld(6, 3000, 5));
        // The others as they were: it is the test.
        let steady: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[50, 50, 50])];
        let slower = among_five(own, steady);
        assert_eq!(slower.withheld, Vec::new());
        assert_eq!(slower.tests.len(), 1);
        // The last five runs before are read for the others as for the
        // test: more than three of them is no reason to leave one out.
        let long_before: Commits = &[("aaa", &[50, 50, 50, 50, 50]), ("bbb", &[150, 150, 150])];
        assert_eq!(
            among_five(long_before, long_before).withheld,
            withheld(6, 3000, 5)
        );
    }

    #[test]
    fn a_clock_has_moved_when_a_quarter_of_the_others_take_a_quarter_longer() {
        let own: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])];
        // 1.24 times as long is a machine doing something else as well.
        let busy: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[124, 124, 124])];
        assert_eq!(among_five(own, busy).tests.len(), 1);
        // 1.25 times is where no test is named against it any more.
        let busier: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[125, 125, 125])];
        let slower = among_five(own, busier);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld, withheld(1, 1250, 5));
        // One other test that got slower is a test that got slower: both
        // are named. Two of the five are a quarter of them, and a machine
        // that slows what computes and spares what waits looks like that.
        let steady: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[50, 50, 50])];
        let tripled: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[150, 150, 150])];
        let beside = |slowed: usize| {
            let others: Vec<(&str, Commits)> = ["c::1", "c::2", "c::3", "c::4", "c::5"]
                .iter()
                .enumerate()
                .map(|(nth, id)| (*id, if nth < slowed { tripled } else { steady }))
                .collect();
            let mut tests = vec![("c::t", own)];
            tests.extend(others);
            found_all(&suite(&tests))
        };
        assert_eq!(beside(1).tests.len(), 2);
        assert_eq!(beside(1).withheld, Vec::new());
        assert_eq!(beside(2).tests, Vec::new());
        assert_eq!(beside(2).withheld, withheld(3, 3000, 5));
    }

    #[test]
    fn a_quarter_of_eight_other_tests_is_two() {
        let own: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])];
        let steady: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[50, 50, 50])];
        let tripled: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[150, 150, 150])];
        let beside = |slowed: usize| {
            let ids = [
                "c::1", "c::2", "c::3", "c::4", "c::5", "c::6", "c::7", "c::8",
            ];
            let mut tests = vec![("c::t", own)];
            tests.extend(
                ids.iter()
                    .enumerate()
                    .map(|(nth, id)| (*id, if nth < slowed { tripled } else { steady })),
            );
            found_all(&suite(&tests))
        };
        // One of eight is a test that got slower, and named with the rest.
        assert_eq!(beside(1).tests.len(), 2);
        // Two of eight are a quarter of them: the clock moved.
        assert_eq!(beside(2).tests, Vec::new());
        assert_eq!(beside(2).withheld, withheld(3, 3000, 8));
    }

    #[test]
    fn a_test_timed_on_one_side_only_says_nothing_about_the_clock() {
        // Four tests went with `bbb` and four came, so the suite kept its
        // size: none of the eight was timed on both sides, there is no clock
        // to read, and the one test that was is named.
        let mut observations = Vec::new();
        for run in 1..=3 {
            observations.push(timed("c::t", "aaa", run, 100));
            for gone in ["c::g1", "c::g2", "c::g3", "c::g4"] {
                observations.push(timed(gone, "aaa", run, 50));
            }
        }
        for run in 4..=6 {
            observations.push(timed("c::t", "bbb", run, 300));
            for new in ["c::n1", "c::n2", "c::n3", "c::n4"] {
                observations.push(timed(new, "bbb", run, 500));
            }
        }
        let slower = found_all(&observations);
        assert_eq!(slower.withheld, Vec::new());
        assert_eq!(slower.tests.len(), 1);
    }

    #[test]
    fn a_clock_is_read_from_five_tests_that_took_20_ms_before_or_from_none() {
        let own: Commits = &[("aaa", &[100, 100, 100]), ("bbb", &[300, 300, 300])];
        // Five tests under 20 ms that take three times as long say nothing
        // about a machine: at that size it is what a clock does.
        let quick: Commits = &[("aaa", &[3, 3, 3]), ("bbb", &[9, 9, 9])];
        let slower = among_five(own, quick);
        assert_eq!(slower.withheld, Vec::new());
        assert_eq!(slower.tests.len(), 1);
        // Nor do tests that only take 20 ms since: they are what got
        // slower, not what it is measured by. All six are named.
        let small: Commits = &[("aaa", &[5, 5, 5]), ("bbb", &[25, 25, 25])];
        let slower = among_five(own, small);
        assert_eq!(slower.withheld, Vec::new());
        assert_eq!(slower.tests.len(), 6);
        // Four others, all three times as long: too few to read a clock
        // from, so they are five tests, said to be many at once.
        let tripled: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[150, 150, 150])];
        let five = found_all(&suite(&[
            ("c::t", own),
            ("c::1", tripled),
            ("c::2", tripled),
            ("c::3", tripled),
            ("c::4", tripled),
        ]));
        assert_eq!(five.withheld, Vec::new());
        assert_eq!(five.tests.len(), 5);
        assert!(five.tests.iter().all(SlowerSince::many_at_once));
        // A clock that got faster withholds nothing.
        let faster: Commits = &[("aaa", &[500, 500, 500]), ("bbb", &[100, 100, 100])];
        let slower = among_five(own, faster);
        assert_eq!(slower.withheld, Vec::new());
        assert_eq!(slower.tests.len(), 1);
    }

    #[test]
    fn a_test_withheld_at_two_commits_as_large_is_withheld_for_the_first() {
        // Slower at `bbb` and again at `ccc`, by the same 100 ms, on a
        // machine that doubled each time: withheld for `bbb`, where it
        // began. The six others, slower by more at `ccc`, are withheld
        // there.
        let own: Commits = &[
            ("aaa", &[0, 0, 0]),
            ("bbb", &[100, 100, 100]),
            ("ccc", &[200, 200, 200]),
        ];
        let others: Commits = &[
            ("aaa", &[50, 50, 50]),
            ("bbb", &[100, 100, 100]),
            ("ccc", &[200, 200, 200]),
        ];
        let slower = found_all(&suite(&[
            ("c::t", own),
            ("c::1", others),
            ("c::2", others),
            ("c::3", others),
            ("c::4", others),
            ("c::5", others),
            ("c::6", others),
        ]));
        assert_eq!(slower.tests, Vec::new());
        let places: Vec<(&str, usize)> = slower
            .withheld
            .iter()
            .map(|withheld| (withheld.commit.as_str(), withheld.tests))
            .collect();
        assert_eq!(places, [("bbb", 1), ("ccc", 6)]);
    }

    #[test]
    fn a_clock_that_moved_at_a_later_commit_is_that_commits_reading() {
        // Slower at `bbb` on a clock that held, and then five runs on a
        // machine that takes 2.5 times as long for everything. `bbb` is
        // read over its own runs, and the test is named with it; what the
        // machine did is read at `ccc`, where the five others are withheld.
        // The time since `bbb` is every run since, the machine's included.
        let own: Commits = &[
            ("aaa", &[10, 10, 10]),
            ("bbb", &[100, 100, 100]),
            ("ccc", &[250, 250, 250, 250, 250]),
        ];
        let others: Commits = &[
            ("aaa", &[30, 30, 30]),
            ("bbb", &[30, 30, 30]),
            ("ccc", &[75, 75, 75, 75, 75]),
        ];
        let slower = among_five(own, others);
        let named: Vec<(&str, &str, u64, u64)> = slower
            .tests
            .iter()
            .map(|test| {
                (
                    test.id.as_str(),
                    test.commit.as_str(),
                    test.before_ms,
                    test.since_ms,
                )
            })
            .collect();
        assert_eq!(named, [("c::t", "bbb", 10, 250)]);
        assert_eq!(
            slower.withheld,
            vec![Withheld {
                commit: "ccc".to_owned(),
                environment: Some("local".to_owned()),
                tests: 5,
                times_per_mille: 2500,
                tests_read: 5,
            }]
        );
        // Other tests that got slower by their own code at `ccc` are no
        // reason to withhold what `bbb` did either.
        let some_slower: Commits = &[
            ("aaa", &[30, 30, 30]),
            ("bbb", &[30, 30, 30]),
            ("ccc", &[60, 60, 60, 60, 60]),
        ];
        let steady: Commits = &[
            ("aaa", &[30, 30, 30]),
            ("bbb", &[30, 30, 30]),
            ("ccc", &[30, 30, 30, 30, 30]),
        ];
        let slower = found_all(&suite(&[
            ("c::t", own),
            ("c::1", some_slower),
            ("c::2", some_slower),
            ("c::3", steady),
            ("c::4", steady),
            ("c::5", steady),
        ]));
        assert!(
            slower
                .tests
                .iter()
                .any(|test| test.id == "c::t" && test.commit == "bbb"),
            "{slower:?}"
        );
    }

    #[test]
    fn a_machine_that_changed_partway_through_a_commit_is_read_from_the_test() {
        // `c::t` failed the first four runs of `bbb`, on a machine as it
        // was, and passed the next three, on one that took 2.5 times as
        // long for everything. Over all of `bbb`'s runs the four before
        // carry the median and the clock held; over the test's own three
        // it moved, and it is withheld.
        let mut observations = Vec::new();
        for run in 1..=5 {
            observations.push(timed("c::t", "aaa", run, 30));
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "aaa", run, 30));
            }
        }
        for run in 6..=12 {
            let ms = if run <= 9 { 30 } else { 75 };
            let mut own = timed("c::t", "bbb", run, ms * 5 / 2);
            if run <= 9 {
                own.status = TestStatus::Failed;
            }
            observations.push(own);
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "bbb", run, ms));
            }
        }
        let slower = found_all(&observations);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld.len(), 1);
        assert_eq!(slower.withheld[0].tests, 1);
        // On the machine as it was all along, it is the test, and named.
        let mut steady = observations.clone();
        for observation in &mut steady {
            if observation.id != "c::t" && observation.commit.as_deref() == Some("bbb") {
                observation.duration_ms = Some(30);
            }
        }
        let slower = found_all(&steady);
        assert_eq!(slower.withheld, Vec::new());
        assert_eq!(slower.tests.len(), 1);
    }

    #[test]
    fn a_machine_that_was_slower_before_the_tests_own_runs_is_not_held_against_it() {
        // `bbb` began with four runs on a machine that took 2.5 times as
        // long, without `c::t`; then three on the machine as it was, where
        // `c::t` took ten times as long by its own code. Over its own runs
        // the others kept their time: it is named.
        let mut observations = Vec::new();
        for run in 1..=5 {
            observations.push(timed("c::t", "aaa", run, 10));
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "aaa", run, 30));
            }
        }
        for run in 6..=12 {
            if run > 9 {
                observations.push(timed("c::t", "bbb", run, 100));
            }
            let ms = if run <= 9 { 75 } else { 30 };
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "bbb", run, ms));
            }
        }
        let slower = found_all(&observations);
        assert_eq!(slower.withheld, Vec::new());
        let named: Vec<(&str, &str)> = slower
            .tests
            .iter()
            .map(|test| (test.id.as_str(), test.commit.as_str()))
            .collect();
        assert_eq!(named, [("c::t", "bbb")]);
    }

    #[test]
    fn the_before_ends_where_the_commit_begins() {
        // The others took 30, 40 and 30 ms before `bbb` and 40 on it: a
        // third longer, and the clock moved. Their first run on `bbb` is
        // not part of their before — with it, the before would be 35 and
        // the clock would have held.
        let own: Commits = &[("aaa", &[10, 10, 10]), ("bbb", &[100, 100, 100])];
        let others: Commits = &[("aaa", &[30, 40, 30]), ("bbb", &[40, 40, 40])];
        let slower = among_five(own, others);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld[0].times_per_mille, 1333);
    }

    #[test]
    fn runs_the_test_sat_out_are_not_its_clock() {
        // `bbb` began with three runs on a machine that took 2.5 times as
        // long, where `c::t` passed at 75 ms; then four on the machine as
        // it was, where it failed. Over the runs it was timed in the
        // others took 2.5 times as long too: withheld, whatever the four
        // runs it sat out say.
        let mut observations = Vec::new();
        for run in 1..=5 {
            observations.push(timed("c::t", "aaa", run, 30));
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "aaa", run, 30));
            }
        }
        for run in 6..=12 {
            let ms = if run <= 8 { 75 } else { 30 };
            let mut own = timed("c::t", "bbb", run, 75);
            if run > 8 {
                own.status = TestStatus::Failed;
            }
            observations.push(own);
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "bbb", run, ms));
            }
        }
        let slower = found_all(&observations);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld.len(), 1);
        assert_eq!(slower.withheld[0].tests, 1);
    }

    #[test]
    fn the_runs_read_end_where_the_next_commit_begins() {
        // Over the test's three runs of `bbb` the others took 40, 40 and
        // 30 ms against 30 before: a third longer, and the clock moved.
        // The first run of `ccc`, at 30 again, is not one of them — with
        // it, the others would be read at 35 and the clock would have held.
        let own: Commits = &[
            ("aaa", &[10, 10, 10]),
            ("bbb", &[100, 100, 100]),
            ("ccc", &[100, 100, 100]),
        ];
        let others: Commits = &[
            ("aaa", &[30, 30, 30]),
            ("bbb", &[40, 40, 30]),
            ("ccc", &[30, 30, 30]),
        ];
        let slower = among_five(own, others);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld[0].times_per_mille, 1333);
    }

    #[test]
    fn a_commit_with_fewer_than_three_runs_is_read_over_three() {
        // One run on `bbb`, where the test got slower, and four on `ccc`.
        // The others took 2.5 times as long from `bbb` on: read over `bbb`'s
        // one run there would be nothing to read, and over three the clock
        // moved.
        let own: Commits = &[
            ("aaa", &[30, 30, 30]),
            ("bbb", &[100]),
            ("ccc", &[100, 100, 100, 100]),
        ];
        let moved: Commits = &[
            ("aaa", &[30, 30, 30]),
            ("bbb", &[75]),
            ("ccc", &[75, 75, 75, 75]),
        ];
        let slower = among_five(own, moved);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld.len(), 1);
        assert_eq!(slower.withheld[0].commit, "bbb");
        // Three, and not four: a fourth run on another clock is not read.
        let fourth_moved: Commits = &[
            ("aaa", &[30, 30, 30]),
            ("bbb", &[30]),
            ("ccc", &[90, 30, 90, 30]),
        ];
        let slower = among_five(own, fourth_moved);
        assert_eq!(slower.withheld, Vec::new());
        assert_eq!(slower.tests[0].commit, "bbb");
    }

    #[test]
    fn a_test_steps_past_its_own_place_in_the_reading() {
        // Of the five others, two took three times as long — each with one
        // run as fast as before, so neither is slower by the rule — and
        // three kept their time. The test itself took twice as long. A
        // quarter from the top of the others is three times, not the
        // test's own two.
        let own: Commits = &[
            ("aaa", &[100, 100, 100, 100, 100]),
            ("bbb", &[200, 200, 200]),
        ];
        let thrice: Commits = &[("aaa", &[30, 30, 30, 30, 30]), ("bbb", &[90, 90, 30])];
        let steady: Commits = &[("aaa", &[30, 30, 30, 30, 30]), ("bbb", &[30, 30, 30])];
        let slower = found_all(&suite(&[
            ("c::t", own),
            ("c::1", thrice),
            ("c::2", thrice),
            ("c::3", steady),
            ("c::4", steady),
            ("c::5", steady),
        ]));
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld.len(), 1);
        assert_eq!(
            (slower.withheld[0].tests, slower.withheld[0].times_per_mille),
            (1, 3000)
        );
    }

    #[test]
    fn a_commit_is_read_from_its_first_run_whichever_test_passed_in_it() {
        // `c::t` failed the first three runs of `bbb` and passed the next
        // three, on a machine that took 2.5 times as long from the first
        // of them on. Its own first pass is no start for the reading: the
        // others' runs of `bbb` in front of it are on the same machine.
        let mut observations = Vec::new();
        for run in 1..=5 {
            observations.push(timed("c::t", "aaa", run, 30));
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "aaa", run, 30));
            }
        }
        for run in 6..=11 {
            let mut own = timed("c::t", "bbb", run, 75);
            if run <= 8 {
                own.status = TestStatus::Failed;
            }
            observations.push(own);
            for other in ["c::1", "c::2", "c::3", "c::4", "c::5"] {
                observations.push(timed(other, "bbb", run, 75));
            }
        }
        let slower = found_all(&observations);
        assert_eq!(slower.tests, Vec::new());
        assert_eq!(slower.withheld.len(), 1);
        assert_eq!(slower.withheld[0].tests, 6);
    }

    #[test]
    fn a_test_is_counted_with_the_tests_of_its_own_commit_and_clock() {
        // Another test got much slower at the same commit: it hides
        // nothing, and each of the two is one of two.
        let far_slower: Commits = &[("aaa", &[10, 10, 10]), ("bbb", &[1000, 1000, 1000])];
        let steady: Commits = &[("aaa", &[50, 50, 50]), ("bbb", &[50, 50, 50])];
        let findings = beside(&[("c::far", far_slower), ("c::steady", steady)]);
        assert_eq!(findings.len(), 2);
        assert_eq!(company(&findings), (2, 3, false));
        // A test that got slower at a later commit is not company: `c::t`
        // is one of the three timed since `bbb`, `c::late` one of the two
        // that take 20 ms or more since `ccc`.
        let history = suite(&[
            (
                "c::t",
                &[
                    ("aaa", &[100, 100, 100]),
                    ("bbb", &[300, 300, 300]),
                    ("ccc", &[300, 300, 300]),
                ],
            ),
            (
                "c::late",
                &[
                    ("aaa", &[10, 10, 10]),
                    ("bbb", &[10, 10, 10]),
                    ("ccc", &[90, 90, 90]),
                ],
            ),
            (
                "c::steady",
                &[
                    ("aaa", &[50, 50, 50]),
                    ("bbb", &[50, 50, 50]),
                    ("ccc", &[50, 50, 50]),
                ],
            ),
        ]);
        let found = found(&history);
        let of = |id: &str| {
            let finding = found.iter().find(|finding| finding.id == id).expect(id);
            (
                finding.commit.as_str(),
                finding.tests_slower,
                finding.tests_timed,
            )
        };
        assert_eq!(of("c::t"), ("bbb", 1, 3));
        assert_eq!(of("c::late"), ("ccc", 1, 3));
    }

    #[test]
    fn a_duration_no_test_has_is_neither_a_crash_nor_a_finding() {
        // A report may say `time="1e16"`; the history keeps what fits.
        let huge = 9_223_372_036_854_775_808;
        assert_eq!(
            named(&[
                ("aaa", &[huge, huge, huge]),
                ("bbb", &[huge + 92, huge + 92, huge + 92])
            ]),
            None
        );
        assert_eq!(
            named(&[
                ("aaa", &[1, 1, 1]),
                ("bbb", &[u64::MAX, u64::MAX, u64::MAX])
            ]),
            Some(at("bbb", 1, u64::MAX))
        );
    }
}
