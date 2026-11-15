//! The commands: `run`, `explain`, `import` and `history`. A command wires
//! modules together and hands what it found to `report.rs`; what a thing
//! means is decided in the module that owns it (`AGENTS.md`). This file
//! holds what the commands share: how one fails, and how each reads and
//! extends sooth's local files.

pub mod explain;
pub mod history;
pub mod import;
pub mod run;

use std::process::ExitCode;

use crate::{quarantine, report};

/// Sooth could not do what it was asked — not the tests failing, sooth
/// itself. The lines say why, in the words of the place that knows; `main`
/// prints each as `sooth: …` on stderr and exits 2. Every command returns
/// this instead of exiting, so the exit-code contract is decided in one
/// place and whatever a command holds (a temp report, a capture directory)
/// is dropped, and so cleaned up, on the way out.
#[derive(Debug)]
pub struct SoothError {
    pub lines: Vec<String>,
}

impl SoothError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            lines: vec![message.into()],
        }
    }
}

/// What a command comes back with: the suite's exit (0 or 1), or why sooth
/// could not get that far.
pub type Outcome = Result<ExitCode, SoothError>;

/// The quarantine list, degrading to empty: an unreadable file warns and
/// leaves every failure unrecognized (and, with `--fail-on-flaky`,
/// unpardoned).
fn load_quarantine() -> std::collections::BTreeSet<String> {
    let path = std::path::Path::new(quarantine::FILE_NAME);
    quarantine::load(path).unwrap_or_else(|err| {
        report::warn(&format!(
            "could not read `{}`: {err} — no failure will be recognized as a known flake",
            path.display()
        ));
        std::collections::BTreeSet::new()
    })
}

/// Append to the history. The first write creates `.sooth/`; that is the
/// moment sooth keeps it out of git's view, and the one time it says so.
fn append_history(
    path: &std::path::Path,
    observations: &[crate::history::Observation],
) -> std::io::Result<()> {
    let created = path.parent().is_some_and(|dir| !dir.exists());
    crate::history::append(path, observations)?;
    if created
        && !observations.is_empty()
        && crate::history::exclude_from_git(std::path::Path::new("."))
    {
        report::note(
            "added `.sooth/` to .git/info/exclude — the history is local and yours, \
             and this keeps it out of `git status` and `git add -A`",
        );
    }
    Ok(())
}

/// Load the history file, warning once about lines it could not read.
fn load_history(path: &std::path::Path) -> crate::history::Loaded {
    let loaded = crate::history::load(path);
    if loaded.skipped_lines > 0 {
        report::warn(&format!(
            "ignored {} unreadable line(s) in {}",
            loaded.skipped_lines,
            path.display()
        ));
    }
    loaded
}
