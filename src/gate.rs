//! Pre-push gate selection: which test files changed against a base (see
//! `DECISIONS.md`). Which files are tests and how to hand them to the
//! runner is preset knowledge (`preset::is_test_file`, `preset::selected_paths`).

use std::path::Path;

use crate::cli::Preset;
use crate::history;
use crate::preset;

/// What the gate will run: the resolved base and the changed test files.
#[derive(Debug)]
pub struct Selection {
    pub base: String,
    pub files: Vec<String>,
}

/// Resolve the gate against `explicit` or the branch's natural base. Every
/// failure is a reason the caller prints verbatim — the gate never guesses,
/// and a failed git listing is an error, never an empty selection.
pub fn resolve(explicit: Option<&str>, preset: Preset, dir: &Path) -> Result<Selection, String> {
    let base = match explicit {
        Some(base) => base.to_owned(),
        None => history::git(dir, &["rev-parse", "--abbrev-ref", "@{upstream}"])
            .or_else(|| {
                history::git(
                    dir,
                    &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
                )
            })
            .ok_or(
                "`--changed` found no base: no upstream and no origin/HEAD — \
                 pass one explicitly (`--changed=origin/main`)",
            )?,
    };
    let merge_base = history::git(dir, &["merge-base", &base, "HEAD"])
        .ok_or_else(|| format!("`--changed={base}` is not a ref git knows here"))?;

    // quotepath off, or a non-ASCII path comes back C-quoted and silently
    // drops out of the selection; --relative keeps tracked paths in the same
    // cwd-relative form `ls-files` uses, so the runner (spawned in the cwd)
    // can open what the diff names; deleted files select nothing — there is
    // nothing left to run.
    let diffed = history::git(
        dir,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--relative",
            "--name-only",
            "--diff-filter=d",
            &merge_base,
        ],
    )
    .ok_or_else(|| format!("`--changed` could not diff against {base}: git failed here"))?;
    let untracked = history::git(
        dir,
        &[
            "-c",
            "core.quotepath=false",
            "ls-files",
            "--others",
            "--exclude-standard",
        ],
    )
    .ok_or("`--changed` could not list untracked files: git failed here")?;

    let mut changed: Vec<String> = diffed
        .lines()
        .chain(untracked.lines())
        .map(str::to_owned)
        .collect();
    changed.sort_unstable();
    changed.dedup();

    let files: Vec<String> = changed
        .into_iter()
        .filter(|path| preset::is_test_file(preset, path))
        .collect();
    Ok(Selection { base, files })
}
