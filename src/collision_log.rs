//! Failures from a Collision console log: what `php artisan test` (and Pest)
//! print, through nunomaduro/collision, where `vendor/bin/phpunit` prints
//! `PHPUnit`'s own format (see [`crate::phpunit_log`]).
//!
//! Collision names a failing test the way it prints it, not the way a
//! `JUnit` report does: `FAILED  Tests\Feature\ShopTest > it adds a line`,
//! where the report says `Tests.Feature.ShopTest::test_it_adds_a_line`. The
//! description is the test's name made readable (`makeDescription` in
//! Collision 8: underscores and capitals become spaces, a leading `test`
//! goes, all lower case — the data set included, as `PHPUnit` 10 and later
//! name a test); Pest prints its own names, and its datasets its own way;
//! and the whole line is cut to the terminal's width, which on CI often
//! leaves only part of the class. So a failure is named only when it fits
//! exactly one test the history already knows — never guessed.

use std::collections::{BTreeMap, BTreeSet};

use crate::junit::TestStatus;
use crate::phpunit_log::clean;

/// One `FAILED` line of a Collision log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The test case's class, as printed (`Tests\Feature\ShopTest`).
    pub class: String,
    /// The test as Collision describes it (`it adds a line`).
    pub description: String,
    pub status: TestStatus,
    /// The line ends where the terminal did: the class or the description
    /// ends in `…`, or the description is missing.
    pub cut: bool,
    /// The class failed before its first test (`setUpBeforeClass`), and the
    /// line names no test at all.
    pub before_any_test: bool,
    /// What the log printed, for saying which line sooth could not name.
    pub printed: String,
}

/// What a Collision log yields before its failures are named.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedLog {
    pub failures: Vec<Failure>,
    /// The first ISO-8601 stamp found on a line, when the log carries them.
    pub at: Option<String>,
}

/// Whether `content` looks like a Collision log: it has Collision's summary
/// line. For saying which `--log` a file wants.
pub fn looks_like(content: &str) -> bool {
    let mut at = None;
    content
        .lines()
        .any(|raw| summary_line(clean(raw, &mut at).trim()).is_some())
}

/// Parse a Collision console log, tolerating what [`crate::phpunit_log`]
/// tolerates (ANSI colors, `gh` columns, timestamps). A log without
/// Collision's summary is refused; one whose summary counts more failures
/// than it shows `FAILED` lines for is refused as incomplete.
pub fn parse_str(content: &str) -> Result<ParsedLog, String> {
    let mut at = None;
    let mut failures = Vec::new();
    let mut since_summary = 0;
    let mut summarised = false;
    for raw in content.lines() {
        let line = clean(raw, &mut at);
        let line = line.trim();
        if let Some(failed) = summary_line(line) {
            summarised = true;
            if since_summary < failed {
                return Err(format!(
                    "the summary reports {failed} failed but the log shows {since_summary} — \
                     is it truncated? sooth records only failures it can name"
                ));
            }
            since_summary = 0;
            continue;
        }
        if let Some(failure) = failed_line(line) {
            failures.push(failure);
            since_summary += 1;
        }
    }
    if !summarised {
        return Err(
            "no Collision summary line found (`Tests: … passed`) — is this the output of \
             `php artisan test`?"
                .to_owned(),
        );
    }
    Ok(ParsedLog { failures, at })
}

/// `Tests:    1 failed, 644 passed (2395 assertions)` → how many failed.
fn summary_line(line: &str) -> Option<usize> {
    let counts = line.strip_prefix("Tests:")?.trim();
    let counts = counts.split(" (").next().unwrap_or(counts);
    let mut failed = 0;
    let mut known = false;
    for field in counts.split(", ") {
        let (count, label) = field.trim().split_once(' ')?;
        let count: usize = count.parse().ok()?;
        match label {
            "failed" => failed = count,
            "passed" | "skipped" | "incomplete" | "todo" | "todos" | "risky" | "warnings"
            | "warning" | "deprecated" | "notices" | "notice" | "pending" => {}
            _ => return None,
        }
        known = true;
    }
    known.then_some(failed)
}

/// `FAILED  Tests\Feature\ShopTest > it adds a line   QueryException` →
/// the failure. The exception's short name stands only where it is no
/// failed expectation's: Collision leaves it out for those, and prints it
/// for `AssertionFailedError` (`$this->fail()`), which `PHPUnit` counts as a
/// failure too — anything else is an error.
fn failed_line(line: &str) -> Option<Failure> {
    let rest = line.strip_prefix("FAILED")?;
    if !rest.starts_with(' ') {
        return None;
    }
    let printed = line.to_owned();
    let rest = rest.trim();
    // The exception stands apart, right-aligned: a class's short name, one
    // word that starts with a capital — a description Collision made from a
    // method is all lower case.
    let (name, exception) = match rest.rfind("  ").map(|gap| rest.split_at(gap)) {
        Some((name, exception)) if is_class_name(exception.trim()) => {
            (name.trim_end(), Some(exception.trim()))
        }
        _ => (rest, None),
    };
    let status = match exception {
        None | Some("AssertionFailedError" | "ExpectationFailedException") => TestStatus::Failed,
        Some(_) => TestStatus::Error,
    };
    // A failure before the class's first test: `Class >` and no test.
    if let Some(class) = name.strip_suffix(" >") {
        return Some(Failure {
            class: class.trim().to_owned(),
            description: String::new(),
            status,
            cut: false,
            before_any_test: true,
            printed,
        });
    }
    let (class, description, cut) = match name.split_once(" > ") {
        Some((class, description)) => {
            (class.trim(), description.trim(), description.ends_with('…'))
        }
        None => (name, "", true),
    };
    Some(Failure {
        class: class.to_owned(),
        description: description.to_owned(),
        status,
        cut,
        before_any_test: false,
        printed,
    })
}

/// Whether `word` can be a PHP class's short name: a capital first, then
/// letters, digits and underscores.
fn is_class_name(word: &str) -> bool {
    word.starts_with(|first: char| first.is_ascii_uppercase())
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The description Collision prints for a test: its name — with the data
/// set, as `PHPUnit` 10 and later name it — where underscores and capitals
/// become spaces, a leading `test` goes, and all is lower case.
pub fn describe(method: &str) -> String {
    let mut spaced = String::with_capacity(method.len() + 8);
    for character in method.chars() {
        match character {
            '_' => spaced.push(' '),
            upper if upper.is_ascii_uppercase() => {
                spaced.push(' ');
                spaced.push(upper);
            }
            other => spaced.push(other),
        }
    }
    let spaced = spaced.strip_prefix("test").unwrap_or(&spaced);
    spaced.trim().to_lowercase()
}

/// The `JUnit` identities of `failures`, read against `known`, the
/// identities the history holds: a failure is the one known test of its
/// class it describes. A known name is compared as Collision would print
/// it — as it is (Pest's own names), through [`describe`] (a `PHPUnit`
/// method, data set included), and in Pest's own dataset form — with runs
/// of spaces closed up, as the terminal prints them. Every failure named,
/// or why not.
pub fn name(
    failures: &[Failure],
    known: &BTreeSet<String>,
) -> Result<BTreeMap<String, TestStatus>, String> {
    let mut by_description: BTreeMap<(String, String), BTreeSet<&str>> = BTreeMap::new();
    for id in known {
        let Some((class, test)) = id.split_once("::") else {
            continue;
        };
        let forms = [
            Some(test.to_owned()),
            Some(describe(test)),
            pest_dataset(test),
        ];
        for form in forms.into_iter().flatten() {
            by_description
                .entry((class.to_owned(), closed_up(&form)))
                .or_default()
                .insert(id);
        }
    }
    let mut named = BTreeMap::new();
    let (mut before, mut cut, mut unknown, mut ambiguous) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for failure in failures {
        let key = (
            failure.class.replace('\\', "."),
            closed_up(&failure.description),
        );
        let ids = by_description
            .get(&key)
            .map(|ids| ids.iter().collect::<Vec<_>>());
        match ids {
            // A name that ends in `…` of its own is still a name.
            Some(ids) if ids.len() == 1 && !failure.before_any_test => {
                named.insert((*ids[0]).to_owned(), failure.status);
            }
            _ if failure.before_any_test => before.push(failure.printed.as_str()),
            _ if failure.cut => cut.push(failure.printed.as_str()),
            Some(_) => ambiguous.push(failure.printed.as_str()),
            None => unknown.push(failure.printed.as_str()),
        }
    }
    if before.is_empty() && cut.is_empty() && unknown.is_empty() && ambiguous.is_empty() {
        return Ok(named);
    }
    let mut why = Vec::new();
    if !before.is_empty() {
        why.push(format!(
            "{} before {} first test (`setUpBeforeClass`), which names no test ({})",
            count(before.len(), "class failed", "classes failed"),
            if before.len() == 1 { "its" } else { "their" },
            quoted(&before)
        ));
    }
    if !cut.is_empty() {
        why.push(format!(
            "the log cut {} to the terminal's width ({}) — give the test step a wide \
             terminal (`COLUMNS=250`), or have it write a JUnit report (`--log-junit`), so \
             names are written in full",
            count(cut.len(), "failing test's name", "failing tests' names"),
            quoted(&cut)
        ));
    }
    if !unknown.is_empty() {
        why.push(format!(
            "{} named the way Collision prints {} ({}), and no run in the history knows a \
             test of that class by that description — import a JUnit report of the suite, or \
             run it through sooth once, and import the log again",
            count(unknown.len(), "failing test is", "failing tests are"),
            if unknown.len() == 1 { "it" } else { "them" },
            quoted(&unknown)
        ));
    }
    if !ambiguous.is_empty() {
        why.push(format!(
            "{} more than one test the history knows — two names Collision prints alike ({})",
            count(ambiguous.len(), "failing test fits", "failing tests fit"),
            quoted(&ambiguous)
        ));
    }
    Err(format!(
        "{}; sooth records only failures it can name",
        why.join("; ")
    ))
}

/// Pest's own dataset form of a `JUnit` name: its report writes `it adds
/// with data set "(2)"` and `… with data set "dataset "two""` where Collision
/// prints `it adds with (2)` and `… with dataset "two"`.
fn pest_dataset(test: &str) -> Option<String> {
    let (base, set) = test.split_once(" with data set \"")?;
    let set = set.strip_suffix('"')?;
    (set.starts_with('(') || set.starts_with("dataset \"")).then(|| format!("{base} with {set}"))
}

/// `text` with every run of whitespace one space, as a terminal prints it.
fn closed_up(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn quoted(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|line| format!("`{line}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{describe, is_class_name, looks_like, name, parse_str, Failure};
    use crate::junit::TestStatus;

    /// The tail of a compact Collision log on GitHub Actions, timestamps
    /// and colors as they come.
    const CUT: &str = "2026-11-20T16:05:02.1Z   \u{1b}[90m.\u{1b}[39m⨯..\n\
        2026-11-20T16:05:02.2Z   \u{1b}[41;1m FAILED \u{1b}[49;22m \u{1b}[1mModules\\Shop\\Test…\u{1b}[22m  \u{1b}[41;1m QueryException \u{1b}[49;22m  \n\
        2026-11-20T16:05:02.3Z   Tests:    1 failed, 644 passed (2395 assertions)\n";

    const WHOLE: &str = "  ......⨯.\n\
        \n\
          FAILED  Tests\\Feature\\ShopTest > it adds a line                \n\
          Failed asserting that false is true.\n\
        \n\
          FAILED  Tests\\Feature\\CartTest > totals                QueryException   \n\
        \n\
          Tests:    2 failed, 7 passed (12 assertions)\n\
          Duration: 0.42s\n";

    fn known(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn collision_describes_a_method_as_it_prints_it() {
        assert_eq!(describe("test_it_adds_a_line"), "it adds a line");
        assert_eq!(describe("testItAddsALine"), "it adds a line");
        assert_eq!(describe("it_adds_a_line"), "it adds a line");
        assert_eq!(describe("totals"), "totals");
        assert_eq!(describe("test_HTML_is_escaped"), "h t m l is escaped");
    }

    #[test]
    fn a_failed_line_gives_class_description_and_status() {
        let parsed = parse_str(WHOLE).expect("parses");
        let pick = |failure: &Failure| {
            (
                failure.class.clone(),
                failure.description.clone(),
                failure.status,
                failure.cut,
            )
        };
        assert_eq!(
            parsed.failures.iter().map(pick).collect::<Vec<_>>(),
            [
                (
                    "Tests\\Feature\\ShopTest".to_owned(),
                    "it adds a line".to_owned(),
                    TestStatus::Failed,
                    false
                ),
                (
                    "Tests\\Feature\\CartTest".to_owned(),
                    "totals".to_owned(),
                    TestStatus::Error,
                    false
                ),
            ]
        );
    }

    #[test]
    fn an_exception_is_a_class_short_name() {
        for class in ["QueryException", "PHPUnit_Framework_Exception", "E2"] {
            assert!(is_class_name(class), "{class}");
        }
        for other in ["adds", "Bad-Name", "", "Two Words"] {
            assert!(!is_class_name(other), "{other}");
        }
    }

    #[test]
    fn a_description_with_two_spaces_in_it_is_no_exception() {
        let log =
            "  FAILED  Tests\\Feature\\ShopTest > it  adds\n  Tests:    1 failed (1 assertions)\n";
        let parsed = parse_str(log).expect("parses");
        assert_eq!(parsed.failures[0].description, "it  adds");
        assert_eq!(parsed.failures[0].status, TestStatus::Failed);
    }

    #[test]
    fn a_line_cut_in_its_description_is_cut_too() {
        let log = "  FAILED  Tests\\Feature\\ShopTest > it adds a li…\n  Tests:    1 failed (1 assertions)\n";
        let parsed = parse_str(log).expect("parses");
        assert!(parsed.failures[0].cut);
        assert_eq!(parsed.failures[0].class, "Tests\\Feature\\ShopTest");
    }

    #[test]
    fn a_line_cut_to_the_terminal_is_known_to_be_cut() {
        let parsed = parse_str(CUT).expect("parses");
        assert_eq!(parsed.at.as_deref(), Some("2026-11-20T16:05:02.1Z"));
        assert_eq!(parsed.failures.len(), 1);
        assert!(parsed.failures[0].cut);
        assert_eq!(parsed.failures[0].status, TestStatus::Error);
        let refused = name(&parsed.failures, &known(&[])).expect_err("cut");
        assert!(
            refused.contains("the log cut 1 failing test's name to the terminal's width"),
            "{refused}"
        );
        assert!(refused.contains("(`COLUMNS=250`)"), "{refused}");
    }

    #[test]
    fn a_green_log_names_nothing_and_a_short_one_is_refused() {
        let green = "  ........\n  Tests:    8 passed (20 assertions)\n";
        assert!(parse_str(green).expect("parses").failures.is_empty());
        let short = "  ..⨯.\n  Tests:    1 failed, 3 passed (6 assertions)\n";
        let refused = parse_str(short).expect_err("no FAILED line");
        assert!(
            refused.contains("reports 1 failed but the log shows 0"),
            "{refused}"
        );
        let other = "OK (4 tests, 8 assertions)\n";
        assert!(parse_str(other).is_err());
        assert!(!looks_like(other));
        assert!(looks_like(WHOLE));
        assert!(looks_like(CUT), "indented, behind a timestamp");
    }

    #[test]
    fn a_failure_is_named_by_the_one_known_test_it_describes() {
        let parsed = parse_str(WHOLE).expect("parses");
        let history = known(&[
            "Tests.Feature.ShopTest::test_it_adds_a_line",
            "Tests.Feature.ShopTest::test_it_removes_a_line",
            "Tests.Feature.CartTest::totals",
        ]);
        let named = name(&parsed.failures, &history).expect("both known");
        assert_eq!(
            named.into_iter().collect::<Vec<_>>(),
            [
                (
                    "Tests.Feature.CartTest::totals".to_owned(),
                    TestStatus::Error
                ),
                (
                    "Tests.Feature.ShopTest::test_it_adds_a_line".to_owned(),
                    TestStatus::Failed
                ),
            ]
        );
        // A Pest name is the description itself.
        let pest = known(&[
            "Tests.Feature.ShopTest::it adds a line",
            "Tests.Feature.CartTest::totals",
        ]);
        assert!(name(&parsed.failures, &pest).is_ok());
    }

    #[test]
    fn a_failure_no_known_test_fits_or_several_fit_is_not_guessed() {
        let parsed = parse_str(WHOLE).expect("parses");
        let refused = name(
            &parsed.failures,
            &known(&["Tests.Feature.CartTest::totals"]),
        )
        .expect_err("ShopTest unknown");
        assert!(
            refused.contains(
                "1 failing test is named the way Collision prints it (`FAILED  \
                 Tests\\Feature\\ShopTest > it adds a line`)"
            ),
            "{refused}"
        );
        // Two methods Collision prints alike: which one failed is not said.
        let alike = known(&[
            "Tests.Feature.ShopTest::test_it_adds_a_line",
            "Tests.Feature.ShopTest::testItAddsALine",
            "Tests.Feature.CartTest::totals",
        ]);
        let refused = name(&parsed.failures, &alike).expect_err("which method");
        assert!(
            refused.contains("two names Collision prints alike"),
            "{refused}"
        );
    }

    /// The failures of a log of `lines`, closed by a summary of as many.
    fn failing(lines: &[&str]) -> Vec<Failure> {
        let log = format!(
            "{}\n  Tests:    {} failed, 1 pending (3 assertions)\n",
            lines.join("\n"),
            lines.len()
        );
        parse_str(&log).expect("parses").failures
    }

    #[test]
    fn a_data_set_is_named_as_collision_prints_it() {
        let history = known(&[
            "Tests.Unit.OrderTest::test_with_amount with data set \"one\"",
            "Tests.Unit.OrderTest::test_with_amount with data set \"two\"",
            "Tests.Unit.OrderTest::test_order with data set \"Big_Order\"",
            "Tests.Feature.PestTest::it handles datasets with data set \"(2)\"",
            "Tests.Feature.PestTest::it handles named with data set \"dataset \"second\"\"",
        ]);
        let failures = failing(&[
            "  FAILED  Tests\\Unit\\OrderTest > with amount with data set \"two\"",
            // The terminal closes up the double space Collision makes.
            "  FAILED  Tests\\Unit\\OrderTest > order with data set \" big order\"",
            "  FAILED  Tests\\Feature\\PestTest > it handles datasets with (2)",
            "  FAILED  Tests\\Feature\\PestTest > it handles named with dataset \"second\"",
        ]);
        let named = name(&failures, &history).expect("every one named");
        assert_eq!(
            named.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "Tests.Feature.PestTest::it handles datasets with data set \"(2)\"",
                "Tests.Feature.PestTest::it handles named with data set \"dataset \"second\"\"",
                "Tests.Unit.OrderTest::test_order with data set \"Big_Order\"",
                "Tests.Unit.OrderTest::test_with_amount with data set \"two\"",
            ]
        );
    }

    #[test]
    fn a_run_stopped_early_counts_its_tests_pending() {
        let log = "  FAILED  Tests\\Unit\\OrderTest > totals\n  Tests:    1 failed, 1 passed, 5 pending (2 assertions)\n";
        assert_eq!(parse_str(log).expect("parses").failures.len(), 1);
        assert!(looks_like(log));
    }

    #[test]
    fn a_failed_assertion_by_hand_is_a_failure_not_an_error() {
        let failures = failing(&[
            "  FAILED  Tests\\Unit\\OrderTest > calls fail          AssertionFailedError",
            "  FAILED  Tests\\Unit\\OrderTest > boots          RuntimeException",
        ]);
        assert_eq!(failures[0].status, TestStatus::Failed);
        assert_eq!(failures[1].status, TestStatus::Error);
    }

    #[test]
    fn a_failure_before_the_first_test_names_no_test() {
        let failures = failing(&["  FAILED  Tests\\Feature\\BootTest >  RuntimeException"]);
        assert!(failures[0].before_any_test && !failures[0].cut);
        let refused = name(&failures, &known(&["Tests.Feature.BootTest::test_boots"]))
            .expect_err("no test named");
        assert!(
            refused.contains("1 class failed before its first test (`setUpBeforeClass`)"),
            "{refused}"
        );
    }

    #[test]
    fn a_name_that_ends_in_an_ellipsis_of_its_own_is_still_a_name() {
        let failures = failing(&["  FAILED  Tests\\Feature\\WaitTest > waits…"]);
        assert!(failures[0].cut);
        let named =
            name(&failures, &known(&["Tests.Feature.WaitTest::waits…"])).expect("known as it is");
        assert!(named.contains_key("Tests.Feature.WaitTest::waits…"));
    }
}
