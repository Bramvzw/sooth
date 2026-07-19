//! The local run history: one observation per test per observed run,
//! appended to `.sooth/history.jsonl` in the directory sooth runs from.
//! This is the passive layer of flaky detection — evidence accumulates from
//! runs that happen anyway, at zero extra wall-time (see `DECISIONS.md`).

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::junit::TestStatus;
use crate::report::json_escape;

/// The history file, relative to the directory sooth runs from.
pub const HISTORY_PATH: &str = ".sooth/history.jsonl";

/// How many of a test's most recent observations the analysis considers.
pub const WINDOW_PER_TEST: usize = 50;

/// How much of the history file's tail one load reads. The file is
/// append-only and never pruned, so reads must be bounded or every run pays
/// for the entire past: 64 MiB is roughly half a million observations —
/// window-filling even for a ten-thousand-test suite.
pub const MAX_LOAD_BYTES: u64 = 64 * 1024 * 1024;

/// The code state observations were made on. `None` means unknowable (no
/// git binary, not a repository): such observations count in totals but can
/// never be identity-bound evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeIdentity {
    pub commit: Option<String>,
    pub dirty: Option<bool>,
}

/// One test's collapsed outcome in one observed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    pub status: TestStatus,
    pub commit: Option<String>,
    pub dirty: Option<bool>,
    pub at_epoch_secs: u64,
}

/// The loaded history, plus how many lines were unreadable — the file is
/// user-managed, so a corrupt line loses one observation, never the run.
pub struct Loaded {
    pub observations: Vec<Observation>,
    pub skipped_lines: usize,
}

/// Seconds since the Unix epoch; zero when the clock predates it.
pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Read the code identity from git. Every failure mode degrades to unknown
/// instead of erroring: history must never make a run fail.
pub fn code_identity(dir: &Path) -> CodeIdentity {
    let Some(commit) = git(dir, &["rev-parse", "HEAD"]) else {
        return CodeIdentity::default();
    };
    // Untracked files count as dirty: a new test file is code the commit
    // does not describe. `.sooth/` itself must be gitignored (see README).
    let dirty = git(dir, &["status", "--porcelain"]).map(|out| !out.is_empty());
    CodeIdentity {
        commit: Some(commit),
        dirty,
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_owned())
}

/// Append observations as JSON lines, creating `.sooth/` when missing.
pub fn append(path: &Path, observations: &[Observation]) -> std::io::Result<()> {
    if observations.is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut buffer = String::new();
    for observation in observations {
        buffer.push_str(&to_line(observation));
        buffer.push('\n');
    }
    file.write_all(buffer.as_bytes())
}

fn to_line(observation: &Observation) -> String {
    let commit = observation
        .commit
        .as_deref()
        .map_or_else(|| "null".to_owned(), |c| format!("\"{}\"", json_escape(c)));
    let dirty = observation
        .dirty
        .map_or_else(|| "null".to_owned(), |d| d.to_string());
    format!(
        r#"{{"at":{},"commit":{commit},"dirty":{dirty},"status":"{}","id":"{}"}}"#,
        observation.at_epoch_secs,
        status_str(observation.status),
        json_escape(&observation.id)
    )
}

/// Every status with its wire name — the one table both directions use.
const STATUS_NAMES: [(TestStatus, &str); 4] = [
    (TestStatus::Passed, "passed"),
    (TestStatus::Failed, "failed"),
    (TestStatus::Error, "error"),
    (TestStatus::Skipped, "skipped"),
];

fn status_str(status: TestStatus) -> &'static str {
    STATUS_NAMES
        .iter()
        .find(|(candidate, _)| *candidate == status)
        .map_or("", |(_, name)| name)
}

fn status_from_str(name: &str) -> Option<TestStatus> {
    STATUS_NAMES
        .iter()
        .find(|(_, candidate)| *candidate == name)
        .map(|(status, _)| *status)
}

/// Load the history at `path`; a missing file is an empty history.
pub fn load(path: &Path) -> Loaded {
    load_tail(path, MAX_LOAD_BYTES)
}

/// Read at most the last `max_bytes` of the file. A truncated first line is
/// expected when the tail cuts mid-line and is dropped silently — it is not
/// corruption, so it must not trip the unreadable-lines warning every run.
fn load_tail(path: &Path, max_bytes: u64) -> Loaded {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let empty = Loaded {
        observations: Vec::new(),
        skipped_lines: 0,
    };
    let Ok(mut file) = fs::File::open(path) else {
        return empty;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return empty;
    };
    let mut bytes = Vec::new();
    let mut truncated = false;
    if len > max_bytes {
        truncated = true;
        if file
            .seek(SeekFrom::End(-i64::try_from(max_bytes).unwrap_or(0)))
            .is_err()
        {
            return empty;
        }
    }
    if file.read_to_end(&mut bytes).is_err() {
        return empty;
    }
    let text = String::from_utf8_lossy(&bytes);
    let text: &str = if truncated {
        text.find('\n').map_or("", |cut| &text[cut + 1..])
    } else {
        &text
    };
    let mut observations = Vec::new();
    let mut skipped_lines = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Some(observation) => observations.push(observation),
            None => skipped_lines += 1,
        }
    }
    Loaded {
        observations,
        skipped_lines,
    }
}

/// Parse one line of the flat schema `to_line` writes. Key lookup takes the
/// first occurrence, which is unambiguous because `id` — the only value that
/// can contain arbitrary text — is written last.
fn parse_line(line: &str) -> Option<Observation> {
    let at_epoch_secs = extract_u64(line, "at")?;
    let commit = extract_string_or_null(line, "commit")?;
    let dirty = extract_bool_or_null(line, "dirty")?;
    let status = status_from_str(&extract_string_or_null(line, "status")??)?;
    let id = extract_string_or_null(line, "id")??;
    Some(Observation {
        id,
        status,
        commit,
        dirty,
        at_epoch_secs,
    })
}

fn value_after<'line>(line: &'line str, key: &str) -> Option<&'line str> {
    let marker = format!("\"{key}\":");
    let start = line.find(&marker)? + marker.len();
    Some(&line[start..])
}

fn extract_u64(line: &str, key: &str) -> Option<u64> {
    let rest = value_after(line, key)?;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// Outer Option is parse success, inner is JSON null: both layers carry
// meaning, so the clippy default does not apply.
#[allow(clippy::option_option)]
fn extract_bool_or_null(line: &str, key: &str) -> Option<Option<bool>> {
    let rest = value_after(line, key)?;
    if rest.starts_with("true") {
        Some(Some(true))
    } else if rest.starts_with("false") {
        Some(Some(false))
    } else if rest.starts_with("null") {
        Some(None)
    } else {
        None
    }
}

#[allow(clippy::option_option)] // same two-layer meaning as above
fn extract_string_or_null(line: &str, key: &str) -> Option<Option<String>> {
    let rest = value_after(line, key)?;
    if rest.starts_with("null") {
        return Some(None);
    }
    let rest = rest.strip_prefix('"')?;
    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(Some(value)),
            '\\' => match chars.next()? {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() != 4 {
                        return None;
                    }
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    value.push(char::from_u32(code)?);
                }
                _ => return None,
            },
            other => value.push(other),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{append, code_identity, load, Observation};
    use crate::junit::TestStatus;
    use std::path::PathBuf;
    use std::process::Command;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sooth-history-{tag}-{}/history.jsonl",
            std::process::id()
        ))
    }

    fn observation(id: &str, status: TestStatus) -> Observation {
        Observation {
            id: id.to_owned(),
            status,
            commit: Some("abc123".to_owned()),
            dirty: Some(false),
            at_epoch_secs: 1_700_000_000,
        }
    }

    #[test]
    fn observations_survive_an_append_load_roundtrip() {
        let path = temp_path("roundtrip");
        let written = [
            observation("c::a", TestStatus::Passed),
            observation("c::b", TestStatus::Failed),
            Observation {
                commit: None,
                dirty: None,
                ..observation("c::no-git", TestStatus::Error)
            },
        ];
        append(&path, &written).expect("append should create dir and file");
        append(&path, &written[..1]).expect("second append should extend");

        let loaded = load(&path);
        assert_eq!(loaded.skipped_lines, 0);
        assert_eq!(loaded.observations.len(), 4);
        assert_eq!(loaded.observations[..3], written);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ids_with_quotes_backslashes_and_newlines_roundtrip() {
        let path = temp_path("escaping");
        let tricky = Observation {
            id: "c::says \"hi\"\\\n".to_owned(),
            ..observation("", TestStatus::Passed)
        };
        append(&path, std::slice::from_ref(&tricky)).expect("append");
        let loaded = load(&path);
        assert_eq!(loaded.observations, [tricky]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unreadable_lines_are_counted_and_skipped_not_fatal() {
        let path = temp_path("corrupt");
        append(&path, &[observation("c::ok", TestStatus::Passed)]).expect("append");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not json at all\n");
        text.push_str(
            "{\"at\":9,\"commit\":null,\"dirty\":null,\"status\":\"weird\",\"id\":\"x\"}\n",
        );
        std::fs::write(&path, text).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.observations.len(), 1);
        assert_eq!(loaded.skipped_lines, 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn loading_reads_only_the_tail_of_a_large_file() {
        let path = temp_path("tail");
        let written: Vec<Observation> = (0..10)
            .map(|i| observation(&format!("c::t{i}"), TestStatus::Passed))
            .collect();
        append(&path, &written).expect("append");
        let line_len = (std::fs::metadata(&path).unwrap().len() / 10) + 1;

        let loaded = super::load_tail(&path, line_len * 3);
        assert!(loaded.observations.len() < 10, "tail read the whole file");
        assert!(!loaded.observations.is_empty());
        assert_eq!(
            loaded.observations.last().unwrap().id,
            "c::t9",
            "the newest observations must survive"
        );
        // The cut-off first line is expected truncation, not corruption.
        assert_eq!(loaded.skipped_lines, 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let loaded = load(&temp_path("missing"));
        assert!(loaded.observations.is_empty());
        assert_eq!(loaded.skipped_lines, 0);
    }

    #[test]
    fn code_identity_reads_commit_and_dirtiness_from_a_real_repo() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // no git on this machine: identity degrades to unknown
        }
        let dir = std::env::temp_dir().join(format!("sooth-history-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        let clean = code_identity(&dir);
        assert!(clean.commit.is_some());
        assert_eq!(clean.dirty, Some(false));

        std::fs::write(dir.join("b.txt"), "untracked").unwrap();
        let dirty = code_identity(&dir);
        assert_eq!(dirty.commit, clean.commit);
        assert_eq!(dirty.dirty, Some(true));

        let nowhere = code_identity(&std::env::temp_dir().join("sooth-no-such-dir"));
        assert_eq!(nowhere.commit, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
