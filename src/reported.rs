//! One run of the test command together with the report it leaves behind:
//! where the report comes from, how sooth knows it is this run's, and
//! reading it. The run loop, `--verify` and the passes to come each decide
//! how many runs to make and in what order; what a run *is* lives here.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::junit::{self, JunitError, JunitReport};
use crate::preset::{self, Spawn};
use crate::runner::{self, RunOutcome};

/// Where the report comes from — the user's own file (`--junit`, freshness
/// checked, never deleted) or a file sooth manages itself (fresh by
/// construction, removed afterwards).
pub enum ReportSource {
    User(PathBuf),
    Preset(TempReport),
}

impl ReportSource {
    /// A report file of sooth's own, for a preset to write to.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if no private directory can be created.
    pub fn temp() -> std::io::Result<Self> {
        TempReport::create().map(Self::Preset)
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::User(path) => path,
            Self::Preset(report) => report.path(),
        }
    }

    /// Make sure that what is read after the run is that run's: a managed
    /// report is deleted, so a runner that stops writing fails loudly
    /// instead of re-serving the previous run's truth; a user's file is
    /// never deleted, so its state is remembered instead — "did this run
    /// touch it" is then a fact, not a clock comparison.
    fn before_run(&self) -> Option<(SystemTime, u64)> {
        match self {
            Self::Preset(report) => {
                let _ = std::fs::remove_file(report.path());
                None
            }
            Self::User(path) => file_state(path),
        }
    }

    /// Read the report the run wrote.
    fn after_run(&self, before: Option<(SystemTime, u64)>) -> Result<JunitReport, ReportError> {
        if let Self::User(path) = self {
            if before.is_some() && file_state(path) == before {
                return Err(ReportError::Untouched);
            }
        }
        junit::parse_file(self.path()).map_err(ReportError::Unreadable)
    }
}

/// A report file sooth manages itself: in a fresh private directory (see
/// `preset::report_path`), and gone again when this value is dropped —
/// after a normal run, on an early return, and when a panic unwinds. The
/// one owner of that cleanup; nothing else removes the directory.
pub struct TempReport {
    path: PathBuf,
}

impl TempReport {
    fn create() -> std::io::Result<Self> {
        preset::report_path().map(|path| Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempReport {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Why a run left no report sooth can use. The wording is the caller's: it
/// knows the command, and whether a failure is fatal or a skipped pass.
#[derive(Debug)]
pub enum ReportError {
    /// The user's `--junit` file is exactly as it was before the run.
    Untouched,
    /// Missing, or not a report: the parser's own account.
    Unreadable(JunitError),
}

/// What one run produced: how the runner exited, and — when the run had a
/// report source — the report, or why there is none.
pub struct Ran {
    pub outcome: RunOutcome,
    pub report: Option<Result<JunitReport, ReportError>>,
}

/// Run `spawn` once and read the report it left in `source`. With `output`
/// the command's stdout and stderr go to that file instead of the terminal.
///
/// # Errors
///
/// Returns the I/O error if the command cannot be started. A run that
/// started and wrote no usable report is not an error here: it is in
/// [`Ran::report`].
pub fn run(
    (command, envs): &Spawn,
    source: Option<&ReportSource>,
    output: Option<std::fs::File>,
) -> std::io::Result<Ran> {
    let before = source.and_then(ReportSource::before_run);
    let outcome = match output {
        Some(file) => runner::run_captured(command, envs, file)?,
        None => runner::run_once(command, envs)?,
    };
    Ok(Ran {
        outcome,
        report: source.map(|source| source.after_run(before)),
    })
}

/// A file's identity-for-freshness: mtime plus size. Comparing states
/// before and after a run answers "did this run touch the report" as an
/// observed fact — no wall clock, no tolerance window, immune to clock skew
/// between sooth and a network filesystem. `None` when the file is missing
/// or the filesystem has no mtimes (the parse step owns the missing-file
/// message).
pub fn file_state(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[cfg(test)]
mod tests {
    use super::{file_state, run, ReportError, ReportSource};

    fn command(script: &str) -> (Vec<String>, Vec<(String, String)>) {
        (
            vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            Vec::new(),
        )
    }

    const GREEN: &str = r#"<testsuite><testcase classname="c" name="t"/></testsuite>"#;

    // `sh` only exists on Unix; the module itself is portable.
    #[cfg(unix)]
    #[test]
    fn a_run_without_a_report_source_is_just_its_outcome() {
        let ran = run(&command("exit 3"), None, None).expect("sh starts");
        assert_eq!(ran.outcome.exit_code, Some(3));
        assert!(ran.report.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_managed_report_is_read_per_run_and_never_served_twice() {
        let source = ReportSource::temp().expect("temp report");
        let path = source.path().to_path_buf();
        let write = format!("printf '%s' '{GREEN}' > '{}'", path.display());

        let ran = run(&command(&write), Some(&source), None).expect("sh starts");
        assert_eq!(
            ran.report
                .expect("a source")
                .expect("a report")
                .test_cases
                .len(),
            1
        );

        // The next run writes nothing: the report of the run before is gone,
        // not read again.
        let ran = run(&command("true"), Some(&source), None).expect("sh starts");
        assert!(matches!(ran.report, Some(Err(ReportError::Unreadable(_)))));
        assert!(!path.exists());

        // Dropping the source takes the file and its private directory along.
        run(&command(&write), Some(&source), None).expect("sh starts");
        let dir = path.parent().expect("a private dir").to_path_buf();
        drop(source);
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_users_report_must_be_touched_by_the_run_and_is_never_deleted() {
        let dir = crate::preset::private_dir().expect("scratch");
        let path = dir.join("report.xml");
        std::fs::write(&path, GREEN).expect("write");
        let source = ReportSource::User(path.clone());

        // The file predates the run and the run leaves it alone.
        let ran = run(&command("true"), Some(&source), None).expect("sh starts");
        assert!(matches!(ran.report, Some(Err(ReportError::Untouched))));
        assert!(
            file_state(&path).is_some(),
            "a user's file is never removed"
        );

        // A run that rewrites it is read. The size changes with it, so the
        // check does not hang on the filesystem's mtime resolution.
        let rewrite = format!("printf '%s\\n' '{GREEN}' > '{}'", path.display());
        let ran = run(&command(&rewrite), Some(&source), None).expect("sh starts");
        assert!(matches!(ran.report, Some(Ok(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn captured_output_goes_to_the_file_not_the_terminal() {
        let dir = crate::preset::private_dir().expect("scratch");
        let log = dir.join("out.log");
        let file = std::fs::File::create(&log).expect("create");
        let ran = run(&command("echo out; echo err >&2"), None, Some(file)).expect("sh starts");
        assert!(ran.outcome.success);
        assert_eq!(std::fs::read_to_string(&log).expect("read"), "out\nerr\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_state_changes_when_the_file_is_rewritten() {
        let path =
            std::env::temp_dir().join(format!("sooth-state-test-{}.xml", std::process::id()));
        std::fs::write(&path, "one").unwrap();
        let before = file_state(&path);
        assert!(before.is_some());
        // Same state when untouched — the "runner wrote nothing" signal.
        assert_eq!(file_state(&path), before);
        std::fs::write(&path, "two-longer").unwrap();
        assert_ne!(file_state(&path), before);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_has_no_state() {
        // Missing files take the parse-error path, which has a better message.
        let missing = std::env::temp_dir().join("sooth-state-test-missing.xml");
        assert_eq!(file_state(&missing), None);
    }

    #[test]
    fn a_command_that_cannot_start_is_the_only_error() {
        let missing = (vec!["sooth-no-such-program".to_owned()], Vec::new());
        assert!(run(&missing, None, None).is_err());
    }
}
