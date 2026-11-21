//! The GitHub Actions history `import --github` reads, through the user's
//! own `gh` and its login: sooth opens no connection of its own (see
//! `DECISIONS.md`). What is read here is runs, their jobs per attempt, and
//! job logs; what they prove is [`crate::reruns`]'s to say.

use std::collections::BTreeMap;
use std::process::Command;

use serde_json::Value;

/// One workflow run that was run more than once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: u64,
    pub attempts: u32,
    /// What started it: `push`, `pull_request`, …
    pub event: String,
    pub status: String,
    /// The commit a `push` run checked out, and every attempt of it again.
    pub head_sha: String,
}

/// One job of one attempt of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub attempt: u32,
    /// `success`, `failure`, `cancelled`, `skipped`, … — empty while running.
    pub conclusion: String,
    pub started_at: Option<String>,
}

/// A job's log, or the fact that GitHub no longer keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Log {
    Text(String),
    Gone,
}

/// The repository `gh` reads, by `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gh {
    pub repo: String,
}

impl Gh {
    /// The repository `--repo` names, or the one `gh` resolves for the
    /// directory sooth runs in.
    pub fn resolve(repo: Option<&str>) -> Result<Self, String> {
        if let Some(repo) = repo {
            return Ok(Self {
                repo: repo.to_owned(),
            });
        }
        let found = gh(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ])?;
        let repo = found.trim();
        if repo.is_empty() {
            return Err("`gh` named no repository here — pass `--repo OWNER/NAME`".to_owned());
        }
        Ok(Self {
            repo: repo.to_owned(),
        })
    }

    /// The runs created on or after `since` (`YYYY-MM-DD`) that had more
    /// than one attempt, read a week at a time: GitHub returns at most 1,000
    /// runs for one filtered listing.
    pub fn reruns_since(&self, since: &str, today: &str) -> Result<Vec<Run>, String> {
        let mut runs = Vec::new();
        for (from, to) in weeks(since, today) {
            let path = format!(
                "repos/{}/actions/runs?per_page=100&created={from}..{to}",
                self.repo
            );
            let lines = gh(&[
                "api",
                "--paginate",
                &path,
                "--jq",
                ".workflow_runs[] | select(.run_attempt > 1) \
                 | {id, run_attempt, event, status, head_sha}",
            ])?;
            for value in json_lines(&lines)? {
                runs.push(Run {
                    id: number(&value, "id")?,
                    attempts: u32::try_from(number(&value, "run_attempt")?).unwrap_or(u32::MAX),
                    event: text(&value, "event"),
                    status: text(&value, "status"),
                    head_sha: text(&value, "head_sha"),
                });
            }
        }
        // A run created while a week is paged shifts another onto two pages.
        runs.sort_by_key(|run| run.id);
        runs.dedup_by_key(|run| run.id);
        Ok(runs)
    }

    /// Every job of every attempt of run `run`.
    pub fn jobs(&self, run: u64) -> Result<Vec<Job>, String> {
        let path = format!(
            "repos/{}/actions/runs/{run}/jobs?filter=all&per_page=100",
            self.repo
        );
        let lines = gh(&[
            "api",
            "--paginate",
            &path,
            "--jq",
            ".jobs[] | {id, name, run_attempt, conclusion, started_at}",
        ])?;
        json_lines(&lines)?
            .into_iter()
            .map(|value| {
                Ok(Job {
                    id: number(&value, "id")?,
                    name: text(&value, "name"),
                    attempt: u32::try_from(number(&value, "run_attempt")?).unwrap_or(u32::MAX),
                    conclusion: text(&value, "conclusion"),
                    started_at: Some(text(&value, "started_at")).filter(|at| !at.is_empty()),
                })
            })
            .collect()
    }

    /// The raw log of job `job`, or [`Log::Gone`] when GitHub answers that
    /// it keeps it no longer — logs outlive neither their retention nor a
    /// deleted run, while the run's record does.
    pub fn log(&self, job: u64) -> Result<Log, String> {
        let path = format!("repos/{}/actions/jobs/{job}/logs", self.repo);
        match gh(&["api", &path]) {
            Ok(text) => Ok(Log::Text(text)),
            Err(why) if why.contains("(HTTP 410)") || why.contains("(HTTP 404)") => Ok(Log::Gone),
            Err(why) => Err(why),
        }
    }
}

/// One job name's red attempt and the first later attempt that passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub red: Job,
    pub green: Job,
}

/// The rerun pairs among a run's jobs: per job name, each attempt that
/// failed and the first later attempt that succeeded. A name that occurs
/// twice in one attempt is left out — two jobs cannot be told apart by it
/// — and so are attempts that were cancelled or skipped.
pub fn candidates(jobs: &[Job]) -> Vec<Candidate> {
    let mut by_name: BTreeMap<&str, Vec<&Job>> = BTreeMap::new();
    for job in jobs {
        by_name.entry(&job.name).or_default().push(job);
    }
    let mut found = Vec::new();
    for mut attempts in by_name.into_values() {
        attempts.sort_by_key(|job| job.attempt);
        if attempts
            .windows(2)
            .any(|two| two[0].attempt == two[1].attempt)
        {
            continue;
        }
        for (at, red) in attempts.iter().enumerate() {
            if red.conclusion != "failure" {
                continue;
            }
            if let Some(green) = attempts[at + 1..]
                .iter()
                .find(|later| later.conclusion == "success")
            {
                found.push(Candidate {
                    red: (*red).clone(),
                    green: (*green).clone(),
                });
            }
        }
    }
    found
}

/// Run `gh` with `args`: its stdout, or what it said went wrong.
#[expect(
    clippy::disallowed_methods,
    reason = "Reading GitHub through the user's own `gh` is what this module is for."
)]
fn gh(args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh").args(args).output().map_err(|err| {
        format!(
            "could not run `gh` ({err}) — `import --github` reads GitHub through the GitHub \
             CLI and your login: install it (https://cli.github.com) and run `gh auth login`"
        )
    })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "`gh {}` failed: {}",
        args.first().copied().unwrap_or_default(),
        said.trim()
    ))
}

/// One JSON value per line, as `gh --jq` prints objects.
fn json_lines(text: &str) -> Result<Vec<Value>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|err| format!("`gh` printed what is no JSON: {err}"))
        })
        .collect()
}

fn number(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`gh` gave no `{key}`"))
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// `since` to `today` (both `YYYY-MM-DD`) in windows of a week, each as
/// GitHub's inclusive `from..to` dates.
fn weeks(since: &str, today: &str) -> Vec<(String, String)> {
    let (Some(start), Some(end)) = (days(since), days(today)) else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    let mut from = start;
    while from <= end {
        let to = (from + 6).min(end);
        windows.push((date(from), date(to)));
        from = to + 1;
    }
    windows
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date.
pub fn days(text: &str) -> Option<i64> {
    let epoch = crate::history::iso8601_to_epoch(&format!("{text}T00:00:00Z"))?;
    i64::try_from(epoch / 86_400).ok()
}

/// The `YYYY-MM-DD` date `days` after 1970-01-01 (Howard Hinnant's
/// civil-from-days).
pub fn date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::{candidates, date, weeks, Job};

    fn job(id: u64, name: &str, attempt: u32, conclusion: &str) -> Job {
        Job {
            id,
            name: name.to_owned(),
            attempt,
            conclusion: conclusion.to_owned(),
            started_at: None,
        }
    }

    #[test]
    fn a_red_attempt_pairs_with_the_first_later_green_one_of_its_name() {
        let jobs = [
            job(1, "unit", 1, "failure"),
            job(2, "lint", 1, "success"),
            job(3, "unit", 2, "failure"),
            job(4, "unit", 3, "success"),
            job(5, "unit", 4, "success"),
            job(6, "feature", 1, "cancelled"),
            job(7, "feature", 2, "success"),
        ];
        let pairs: Vec<(u64, u64)> = candidates(&jobs)
            .iter()
            .map(|pair| (pair.red.id, pair.green.id))
            .collect();
        // Both red attempts of `unit` against attempt 3; a cancelled
        // attempt is no failure; nothing went green after the last red.
        assert_eq!(pairs, [(1, 4), (3, 4)]);
        assert_eq!(candidates(&[job(1, "unit", 2, "failure")]), []);
    }

    #[test]
    fn a_name_two_jobs_share_in_one_attempt_pairs_with_nothing() {
        let jobs = [
            job(1, "test", 1, "failure"),
            job(2, "test", 1, "success"),
            job(3, "test", 2, "success"),
            job(4, "test", 2, "success"),
        ];
        assert_eq!(candidates(&jobs), []);
    }

    #[test]
    fn dates_are_counted_and_cut_into_weeks() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(20_513), "2026-03-01");
        assert_eq!(date(20_512), "2026-02-28");
        assert_eq!(
            weeks("2026-11-01", "2026-11-16"),
            [
                ("2026-11-01".to_owned(), "2026-11-07".to_owned()),
                ("2026-11-08".to_owned(), "2026-11-14".to_owned()),
                ("2026-11-15".to_owned(), "2026-11-16".to_owned()),
            ]
        );
        assert_eq!(weeks("2026-11-16", "2026-11-01"), []);
        assert_eq!(weeks("yesterday", "2026-11-01"), []);
    }
}
