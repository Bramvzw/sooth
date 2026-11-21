//! `sooth import --github`: the CI reruns GitHub keeps, read through the
//! user's own `gh`, brought into the local history (see `DECISIONS.md`).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

use super::{append_history, load_history, Outcome, SoothError};
use crate::github::{self, Gh, Log};
use crate::junit::TestStatus;
use crate::report::{self, Analyses, GithubImport, RerunPair, UnnamedPair};
use crate::reruns::{self, Judged};
use crate::{analyzers, cli, history};

/// How far back `--github` reads unless told: GitHub's own default for
/// keeping logs, so a longer look finds only logs that are gone.
const DEFAULT_DAYS: i64 = 90;

/// Handle `sooth import --github`. Nothing is written before every pair is
/// read: a failure on the way — `gh` missing, logged out, a network error —
/// leaves the history as it was. Exit 0, or 2 when GitHub could not be read
/// or the history could not be written.
pub fn run(args: &cli::ImportArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let today = i64::try_from(history::now_epoch_secs() / 86_400).unwrap_or(0);
    let since = match args.since.as_deref() {
        Some(since) if github::days(since).is_some() => since.to_owned(),
        Some(since) => {
            return Err(SoothError::new(format!(
                "`--since {since}` is not a date — pass it as YYYY-MM-DD"
            )))
        }
        None => github::date(today - DEFAULT_DAYS),
    };
    let gh = Gh::resolve(args.repo.as_deref()).map_err(SoothError::new)?;
    let environment = args.env.clone().unwrap_or_else(|| "ci".to_owned());
    let history_path = Path::new(history::HISTORY_PATH);
    let ledger_path = Path::new(history::IMPORTED_PATH);
    let ledger = history::imported_hashes(ledger_path);
    // A Collision log names a failure only against the tests the history
    // knows by name.
    let known: BTreeSet<String> = load_history(history_path)
        .observations
        .into_iter()
        .map(|observation| observation.id)
        .collect();

    let runs = gh
        .reruns_since(&since, &github::date(today))
        .map_err(SoothError::new)?;
    let mut reading = Reading {
        gh: &gh,
        ledger: &ledger,
        known: &known,
        environment: &environment,
        found: GithubImport {
            repo: gh.repo.clone(),
            since: since.clone(),
            dry_run: args.dry_run,
            reruns: runs.len(),
            ..GithubImport::default()
        },
        observations: Vec::new(),
        entries: Vec::new(),
    };
    for run in &runs {
        reading.read_run(run)?;
    }
    let Reading {
        found,
        mut observations,
        entries,
        ..
    } = reading;

    if !args.dry_run && !observations.is_empty() {
        observations.sort_by_key(|observation| observation.at_epoch_secs);
        append_history(history_path, &observations).map_err(|err| {
            SoothError::new(format!("could not write {}: {err}", history_path.display()))
        })?;
        if let Err(err) = history::record_imported(ledger_path, &entries) {
            // The observations are recorded; only the dedupe bookkeeping failed.
            report::warn(&format!(
                "could not write {}: {err} — importing these reruns again will double-count",
                ledger_path.display()
            ));
        }
    }
    report::print_github_import(&found, style);

    let loaded = load_history(history_path);
    let analysis = analyzers::history::analyze(&loaded.observations);
    report::print_history(
        &Analyses {
            history: Some(&analysis),
            ..Analyses::default()
        },
        style,
    );
    report::print_history_size(loaded.observations.len(), loaded.truncated);
    Ok(ExitCode::SUCCESS)
}

/// What the ledger and the invocation ids call one job: GitHub's job ids
/// are unique across repositories, so however the repository is spelled,
/// one job is one key.
fn job_key(job: u64) -> String {
    format!("github:job:{job}")
}

/// The reruns read so far, and what they come to; nothing is written
/// until every run is read.
struct Reading<'a> {
    gh: &'a Gh,
    ledger: &'a BTreeSet<u64>,
    known: &'a BTreeSet<String>,
    environment: &'a str,
    found: GithubImport,
    observations: Vec<history::Observation>,
    entries: Vec<(u64, String)>,
}

impl Reading<'_> {
    /// Read the rerun pairs of one run.
    fn read_run(&mut self, run: &github::Run) -> Result<(), SoothError> {
        if run.status != "completed" {
            self.found.unfinished += 1;
            return Ok(());
        }
        if run.event != "push" {
            self.found.other_events += 1;
            return Ok(());
        }
        let jobs = self.gh.jobs(run.id).map_err(SoothError::new)?;
        for pair in github::candidates(&jobs) {
            self.read_pair(run, &pair)?;
        }
        Ok(())
    }

    /// Read one red attempt and its green rerun.
    fn read_pair(&mut self, run: &github::Run, pair: &github::Candidate) -> Result<(), SoothError> {
        let key = job_key(pair.red.id);
        let hash = history::fnv1a64(key.as_bytes());
        if self.ledger.contains(&hash) {
            self.found.already += 1;
            return Ok(());
        }
        let red = self.gh.log(pair.red.id).map_err(SoothError::new)?;
        let green = self.gh.log(pair.green.id).map_err(SoothError::new)?;
        let (Log::Text(red), Log::Text(green)) = (red, green) else {
            self.found.gone += 1;
            return Ok(());
        };
        match reruns::judge(&run.head_sha, &red, &green, self.known) {
            Judged::Named(failures) => {
                let green_key = job_key(pair.green.id);
                let red_attempt = Attempt {
                    invocation: history::import_invocation(hash),
                    started_at: pair.red.started_at.as_deref(),
                    inferred: false,
                };
                let green_attempt = Attempt {
                    invocation: history::import_invocation(history::fnv1a64(green_key.as_bytes())),
                    started_at: pair.green.started_at.as_deref(),
                    inferred: true,
                };
                for (id, status) in &failures {
                    self.observations
                        .push(self.observation(run, id, *status, &red_attempt));
                    self.observations.push(self.observation(
                        run,
                        id,
                        TestStatus::Passed,
                        &green_attempt,
                    ));
                }
                self.entries.push((hash, key));
                self.found.recorded.push(RerunPair {
                    job: pair.red.name.clone(),
                    commit: run.head_sha.clone(),
                    tests: failures.into_keys().collect(),
                });
            }
            Judged::Unnamed { causes, why } => self.found.unnamed.push(UnnamedPair {
                job: pair.red.name.clone(),
                commit: run.head_sha.clone(),
                causes,
                why,
            }),
            Judged::NotAPair(why) => *self.found.not_pairs.entry(why).or_default() += 1,
        }
        Ok(())
    }

    /// What one attempt saw of test `id`: on a clean checkout of the run's
    /// commit, in the import's environment.
    fn observation(
        &self,
        run: &github::Run,
        id: &str,
        status: TestStatus,
        attempt: &Attempt,
    ) -> history::Observation {
        history::Observation {
            id: id.to_owned(),
            status,
            commit: Some(run.head_sha.clone()),
            dirty: Some(false),
            tree: None,
            environment: Some(self.environment.to_owned()),
            invocation: Some(attempt.invocation.clone()),
            duration_ms: None,
            at_epoch_secs: attempt
                .started_at
                .and_then(history::iso8601_to_epoch)
                .unwrap_or_else(history::now_epoch_secs),
            inferred: attempt.inferred,
        }
    }
}

/// One attempt of a rerun pair, as its observations record it.
struct Attempt<'a> {
    invocation: String,
    started_at: Option<&'a str>,
    /// A pass read from the attempt's counts, not seen by name.
    inferred: bool,
}
