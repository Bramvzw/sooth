//! Pre-push gate selection: which test files changed against a base, and
//! how to hand exactly those to the runner (see `DECISIONS.md`).

use std::path::Path;

use crate::cli::Preset;
use crate::history;

/// What the gate will run: the resolved base, the changed test files, and
/// the arguments that select them.
#[derive(Debug, PartialEq, Eq)]
pub struct Selection {
    pub base: String,
    pub files: Vec<String>,
    pub args: Vec<String>,
}

/// Resolve the gate against `explicit` or the branch's natural base. Every
/// failure is a reason the caller prints verbatim — the gate never guesses.
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

    let mut changed: Vec<String> = Vec::new();
    if let Some(diffed) = history::git(dir, &["diff", "--name-only", &merge_base]) {
        changed.extend(diffed.lines().map(str::to_owned));
    }
    if let Some(untracked) = history::git(dir, &["ls-files", "--others", "--exclude-standard"]) {
        changed.extend(untracked.lines().map(str::to_owned));
    }
    changed.sort_unstable();
    changed.dedup();

    let files: Vec<String> = changed
        .into_iter()
        .filter(|path| is_test_file(preset, path))
        .collect();
    let args = selection_args(preset, &files);
    Ok(Selection { base, files, args })
}

/// Whether `path` is a test file under this preset's conventions. Changed
/// source files are out of scope on purpose: the gate is about tests being
/// born, not test-impact analysis.
fn is_test_file(preset: Preset, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    match preset {
        Preset::Phpunit => name.ends_with("Test.php"),
        Preset::Pytest => match name.strip_suffix(".py") {
            Some(stem) => stem.starts_with("test_") || stem.ends_with("_test"),
            None => false,
        },
        Preset::Jest => name.contains(".test.") || name.contains(".spec."),
        Preset::Go => name.ends_with("_test.go"),
    }
}

/// The runner arguments selecting exactly `files` — paths, not name filters:
/// every supported runner takes them, go included (as its packages).
fn selection_args(preset: Preset, files: &[String]) -> Vec<String> {
    match preset {
        Preset::Go => {
            let mut dirs: Vec<String> = files
                .iter()
                .map(|file| match file.rsplit_once('/') {
                    Some((dir, _)) => format!("./{dir}"),
                    None => "./.".to_owned(),
                })
                .collect();
            dirs.sort_unstable();
            dirs.dedup();
            dirs
        }
        Preset::Phpunit | Preset::Pytest | Preset::Jest => files.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_test_file, selection_args};
    use crate::cli::Preset;

    #[test]
    fn each_preset_recognizes_its_own_test_files() {
        assert!(is_test_file(Preset::Phpunit, "Modules/Hub/tests/ATest.php"));
        assert!(!is_test_file(Preset::Phpunit, "Modules/Hub/src/A.php"));
        assert!(is_test_file(Preset::Pytest, "tests/test_orders.py"));
        assert!(is_test_file(Preset::Pytest, "tests/orders_test.py"));
        assert!(!is_test_file(Preset::Pytest, "tests/conftest.py"));
        assert!(is_test_file(Preset::Jest, "src/cart.test.tsx"));
        assert!(is_test_file(Preset::Jest, "src/cart.spec.js"));
        assert!(!is_test_file(Preset::Jest, "src/cart.ts"));
        assert!(is_test_file(Preset::Go, "pkg/cart/cart_test.go"));
        assert!(!is_test_file(Preset::Go, "pkg/cart/cart.go"));
    }

    #[test]
    fn most_runners_take_the_paths_verbatim() {
        let files = vec!["a/BTest.php".to_owned(), "c/DTest.php".to_owned()];
        assert_eq!(selection_args(Preset::Phpunit, &files), files);
    }

    #[test]
    fn go_selects_deduplicated_package_dirs() {
        let files = vec![
            "pkg/cart/cart_test.go".to_owned(),
            "pkg/cart/totals_test.go".to_owned(),
            "root_test.go".to_owned(),
        ];
        assert_eq!(
            selection_args(Preset::Go, &files),
            vec!["./.".to_owned(), "./pkg/cart".to_owned()]
        );
    }
}
