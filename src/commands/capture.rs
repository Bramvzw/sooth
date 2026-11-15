//! The output of quiet runs. A command that makes the same suite run
//! several times keeps the runner off the screen — N copies of one output
//! bury what sooth has to say — and shows afterwards what is worth reading:
//! the first run that failed, the run sooth stopped at, the run that was
//! cut short.

use std::path::PathBuf;

use crate::{preset, report};

/// Every quiet run writes to one file in a private directory, and the
/// first failing run's is set aside. Dropping this removes the directory,
/// on every way out of the command. A run is named the way its line names
/// it: `run 3/20`, `the shuffled run`.
pub struct Capture {
    dir: PathBuf,
    /// The first run that failed, whether or not its output could be kept.
    first_failure: Option<String>,
    kept: bool,
}

impl Capture {
    pub fn start() -> std::io::Result<Self> {
        Ok(Self {
            dir: preset::private_dir()?,
            first_failure: None,
            kept: false,
        })
    }

    fn current_path(&self) -> PathBuf {
        self.dir.join("run.log")
    }

    fn kept_path(&self) -> PathBuf {
        self.dir.join("first-failure.log")
    }

    /// An empty file for the run about to start.
    pub fn open(&self) -> std::io::Result<std::fs::File> {
        std::fs::File::create(self.current_path())
    }

    /// What the run that just ended wrote.
    fn current(&self) -> Vec<u8> {
        std::fs::read(self.current_path()).unwrap_or_default()
    }

    /// `run` failed: set its output aside if it is the first.
    pub fn note_failure(&mut self, run: &str) {
        if self.first_failure.is_none() {
            self.first_failure = Some(run.to_owned());
            self.kept = std::fs::rename(self.current_path(), self.kept_path()).is_ok();
        }
    }

    /// Show the first failing run's output, if one failed and it was kept.
    pub fn replay_first_failure(&self, to_stderr: bool, style: report::Style) {
        let Some(run) = self.first_failure.as_deref().filter(|_| self.kept) else {
            return;
        };
        let replay = report::Replay {
            run,
            why: "the first that failed",
            output: &std::fs::read(self.kept_path()).unwrap_or_default(),
        };
        report::print_replay(&replay, to_stderr, style);
    }

    /// Show the output of `run`, the one sooth stops at: it is what the
    /// error that follows points to. An earlier failure goes first.
    pub fn replay_before_stopping(&self, run: &str, to_stderr: bool, style: report::Style) {
        self.replay_first_failure(to_stderr, style);
        let replay = report::Replay {
            run,
            why: "where sooth stopped",
            output: &self.current(),
        };
        report::print_replay(&replay, to_stderr, style);
    }

    /// Show what `run` had written when it was cut short. For a run that
    /// hung, this is the only trace of where: the run itself gets no line
    /// and no place in the report.
    pub fn replay_cut_short(&self, run: &str, to_stderr: bool, style: report::Style) {
        let output = self.current();
        if output.is_empty() {
            return;
        }
        let replay = report::Replay {
            run,
            why: "the one that was cut short",
            output: &output,
        };
        report::print_replay(&replay, to_stderr, style);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.current_path());
        let _ = std::fs::remove_file(self.kept_path());
        let _ = std::fs::remove_dir(&self.dir);
    }
}
