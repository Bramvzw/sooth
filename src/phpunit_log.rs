//! Failures from a `PHPUnit` console log — the per-test identities a CI
//! keeps when no `JUnit` report was written (see `DECISIONS.md`).

use std::collections::BTreeMap;

use crate::junit::TestStatus;

/// What a console log yields. A log names only its failures; passes are
/// anonymous dots and can never become observations.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedLog {
    pub failures: BTreeMap<String, TestStatus>,
    /// The first ISO-8601 stamp found on a line, when the log carries them.
    pub at: Option<String>,
}

/// Parse a `PHPUnit` console log, tolerating ANSI colors, CRLF line ends,
/// the `job\tstep\t` columns of `gh run view --log`, and per-line
/// timestamps. A log without a recognizable `PHPUnit` summary is refused,
/// never guessed at.
pub fn parse_str(content: &str) -> Result<ParsedLog, String> {
    let mut parsed = ParsedLog {
        failures: BTreeMap::new(),
        at: None,
    };
    let mut anchored = false;
    let mut block: Option<TestStatus> = None;
    for raw in content.lines() {
        let line = clean(raw, &mut parsed.at);
        if let Some(listed) = block_header(&line) {
            block = match listed {
                Listing::Counted(status) => Some(status),
                Listing::Ignored => None,
            };
            continue;
        }
        if is_summary(&line) {
            anchored = true;
            block = None;
            continue;
        }
        if let Some(status) = block {
            if let Some(id) = numbered_identity(&line) {
                parsed.failures.insert(id, status);
            }
        }
    }
    if !anchored {
        return Err("no PHPUnit summary line found — is this a PHPUnit console log?".to_owned());
    }
    Ok(parsed)
}

/// The message half of a log line: past the `gh` columns, ANSI, and the
/// timestamp — which is kept as the log's own `at` the first time one shows.
fn clean(raw: &str, at: &mut Option<String>) -> String {
    let message = raw.rsplit('\t').next().unwrap_or(raw);
    let message = strip_ansi(message);
    let message = message.trim_end_matches('\r');
    match leading_timestamp(message) {
        Some((stamp, rest)) => {
            if at.is_none() {
                *at = Some(stamp.to_owned());
            }
            rest.to_owned()
        }
        None => message.to_owned(),
    }
}

fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for follow in chars.by_ref() {
                if follow.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(ch);
        }
    }
    plain
}

/// `2026-08-10T08:34:58.6133316Z the message` → the stamp and the message.
fn leading_timestamp(line: &str) -> Option<(&str, &str)> {
    let (stamp, rest) = line.split_once(' ')?;
    let bytes = stamp.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[10] != b'T' {
        return None;
    }
    bytes[..4]
        .iter()
        .all(u8::is_ascii_digit)
        .then_some((stamp, rest))
}

/// What a listing header opens: entries counted under a status, or entries
/// that carry no run signal (risky, warnings, deprecations).
enum Listing {
    Counted(TestStatus),
    Ignored,
}

fn block_header(line: &str) -> Option<Listing> {
    let listed = line
        .strip_prefix("There was 1 ")
        .or_else(|| line.strip_prefix("There were "))?;
    let listed = listed
        .trim_start_matches(|ch: char| ch.is_ascii_digit())
        .trim_start();
    if !listed.ends_with(':') {
        return None;
    }
    if listed.starts_with("failure") {
        return Some(Listing::Counted(TestStatus::Failed));
    }
    if listed.starts_with("error") {
        return Some(Listing::Counted(TestStatus::Error));
    }
    Some(Listing::Ignored)
}

fn is_summary(line: &str) -> bool {
    line.starts_with("OK (")
        || line.starts_with("OK, but ")
        || line == "FAILURES!"
        || line == "ERRORS!"
        || line == "WARNINGS!"
        || (line.starts_with("Tests: ") && line.contains("Assertions: "))
}

/// `1) Fully\Qualified\ClassTest::method[ with data set "x"]` → the `JUnit`
/// identity. Only the class half is dotted; the name half stays verbatim.
fn numbered_identity(line: &str) -> Option<String> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let entry = line.get(digits..)?.strip_prefix(") ")?;
    let (class, name) = entry.split_once("::")?;
    if class.is_empty() || class.contains(' ') {
        return None;
    }
    Some(format!("{}::{name}", class.replace('\\', ".")))
}

#[cfg(test)]
mod tests {
    use super::parse_str;
    use crate::junit::TestStatus;

    #[test]
    fn a_failure_block_yields_junit_identities() {
        let log = "There was 1 failure:\n\n\
                   1) Modules\\A\\Tests\\BTest::test_x\n\
                   Failed asserting that 2 matches expected 1.\n\n\
                   FAILURES!\nTests: 10, Assertions: 30, Failures: 1.\n";
        let parsed = parse_str(log).expect("parses");
        assert_eq!(
            parsed.failures.get("Modules.A.Tests.BTest::test_x"),
            Some(&TestStatus::Failed)
        );
        assert_eq!(parsed.failures.len(), 1);
    }

    #[test]
    fn errors_and_failures_keep_their_own_status() {
        let log = "There were 2 errors:\n\n\
                   1) C\\DTest::test_a\nboom\n\n\
                   2) C\\DTest::test_b\nboom\n\n\
                   --\n\
                   There was 1 failure:\n\n\
                   1) C\\DTest::test_c\nnope\n\n\
                   ERRORS!\n";
        let parsed = parse_str(log).expect("parses");
        assert_eq!(
            parsed.failures.get("C.DTest::test_a"),
            Some(&TestStatus::Error)
        );
        assert_eq!(
            parsed.failures.get("C.DTest::test_c"),
            Some(&TestStatus::Failed)
        );
        assert_eq!(parsed.failures.len(), 3);
    }

    #[test]
    fn a_data_set_suffix_survives_verbatim() {
        let log = "There was 1 failure:\n\n\
                   1) C\\DTest::test_a with data set \"staging\"\n\n\
                   FAILURES!\n";
        let parsed = parse_str(log).expect("parses");
        assert!(parsed
            .failures
            .contains_key("C.DTest::test_a with data set \"staging\""));
    }

    #[test]
    fn risky_and_warning_listings_carry_no_signal() {
        let log = "There was 1 risky test:\n\n\
                   1) C\\DTest::test_quiet\nno assertions\n\n\
                   OK, but there were issues!\nTests: 5, Assertions: 9.\n\
                   OK (5 tests, 9 assertions)\n";
        let parsed = parse_str(log).expect("parses");
        assert!(parsed.failures.is_empty());
    }

    #[test]
    fn gh_columns_ansi_and_timestamps_are_peeled_off() {
        let log = "Job / Name\tUNKNOWN STEP\t2026-08-10T08:34:58.6133316Z There was 1 failure:\n\
                   Job / Name\tUNKNOWN STEP\t2026-08-10T08:34:58.6134329Z 1) C\\DTest::test_x\n\
                   Job / Name\tUNKNOWN STEP\t2026-08-10T08:34:58.6135340Z \u{1b}[37;41mFAILURES!\u{1b}[0m\r\n";
        let parsed = parse_str(log).expect("parses");
        assert!(parsed.failures.contains_key("C.DTest::test_x"));
        assert_eq!(parsed.at.as_deref(), Some("2026-08-10T08:34:58.6133316Z"));
    }

    #[test]
    fn a_green_log_is_valid_and_names_nothing() {
        let parsed = parse_str("....\nOK (4 tests, 8 assertions)\n").expect("parses");
        assert!(parsed.failures.is_empty());
    }

    #[test]
    fn without_a_phpunit_summary_the_file_is_refused() {
        let err = parse_str("collected 3 items\n3 passed in 0.12s\n").unwrap_err();
        assert!(err.contains("PHPUnit"), "got: {err}");
    }

    #[test]
    fn message_lines_inside_a_block_are_not_identities() {
        let log = "There was 1 failure:\n\n\
                   1) C\\DTest::test_x\n\
                   Failed asserting that 'a::b' is 'c'.\n\
                   1 is not 2\n\n\
                   FAILURES!\n";
        let parsed = parse_str(log).expect("parses");
        assert_eq!(parsed.failures.len(), 1);
    }
}
