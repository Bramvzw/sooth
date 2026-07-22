//! The committed quarantine list: known-flaky test ids that
//! `--fail-on-flaky` pardons (see `DECISIONS.md`).

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

/// The file sooth reads from the working directory — committed, unlike
/// the gitignored `.sooth/` history.
pub const FILE_NAME: &str = ".sooth-quarantine";

/// The quarantined ids, degrading to an empty set: a missing file is the
/// normal day-one state; an unreadable one warns and pardons nothing.
pub fn load_or_empty(path: &Path) -> BTreeSet<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => parse(&content),
        Err(err) if err.kind() == io::ErrorKind::NotFound => BTreeSet::new(),
        Err(err) => {
            eprintln!(
                "sooth: could not read `{}`: {err} — no failures will be pardoned",
                path.display()
            );
            BTreeSet::new()
        }
    }
}

/// One id per line, exactly as reports write them; `#` comments, blank
/// lines, and a leading BOM are ignored, surrounding whitespace is trimmed.
fn parse(content: &str) -> BTreeSet<String> {
    content
        .strip_prefix('\u{feff}')
        .unwrap_or(content)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{load_or_empty, parse};
    use std::path::Path;

    #[test]
    fn parse_skips_comments_and_blank_lines_and_trims() {
        let set = parse(
            "# known flakes\n\n  tests.test_math::test_subtraction  \nApp.FooTest::test_a with data set \"x\"\n",
        );
        assert_eq!(set.len(), 2);
        assert!(set.contains("tests.test_math::test_subtraction"));
        assert!(set.contains(r#"App.FooTest::test_a with data set "x""#));
    }

    #[test]
    fn a_leading_bom_does_not_hide_the_first_entry() {
        let set = parse("\u{feff}tests.test_math::test_subtraction\n");
        assert!(set.contains("tests.test_math::test_subtraction"));
    }

    #[test]
    fn a_missing_file_is_an_empty_quarantine() {
        assert!(load_or_empty(Path::new("/nonexistent/sooth-quarantine")).is_empty());
    }
}
