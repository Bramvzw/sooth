//! Tolerant JUnit-XML parsing.
//!
//! There is no real JUnit-XML standard (see `DECISIONS.md`), so this parser
//! reads a deliberately permissive union of what pytest, `PHPUnit`, jest-junit
//! and gotestsum actually emit: it accepts either a `<testsuites>` or a bare
//! `<testsuite>` root, ignores attributes and elements it does not know
//! about, and treats a missing `time` attribute as a zero duration. It never
//! panics on malformed input — every failure mode comes back as a
//! [`JunitError`].

use std::fmt;
use std::fs;
use std::path::Path;
use std::time::Duration;

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

/// A parsed JUnit-XML report: a flat list of test cases, regardless of how
/// deeply the source XML nests `<testsuite>` elements.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JunitReport {
    pub test_cases: Vec<TestCase>,
}

impl JunitReport {
    /// How many test cases failed or errored.
    pub fn failing_count(&self) -> usize {
        self.test_cases
            .iter()
            .filter(|case| matches!(case.status, TestStatus::Failed | TestStatus::Error))
            .count()
    }

    /// Whether any test case failed or errored — the report-side half of the
    /// suite verdict.
    pub fn has_failures(&self) -> bool {
        self.failing_count() > 0
    }
}

/// A single `<testcase>` element.
#[derive(Debug, Clone, PartialEq)]
pub struct TestCase {
    pub name: String,
    pub classname: Option<String>,
    pub duration: Duration,
    pub status: TestStatus,
}

impl TestCase {
    /// The test's identity: `classname::name` when a classname is present,
    /// bare `name` otherwise. This is a domain concept, not presentation —
    /// it is the frozen `--json` `name` contract, and the v0.2 history file
    /// and quarantine matching key on the same string. One definition only.
    pub fn qualified_name(&self) -> String {
        match &self.classname {
            Some(classname) => format!("{classname}::{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// The outcome of a test case, derived from its child elements: an `<error>`
/// child wins over `<failure>`, which wins over `<skipped>`; no matching
/// child means the case passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Passed,
    Failed,
    Error,
    Skipped,
}

impl TestStatus {
    /// Higher-severity statuses win when a testcase has more than one child
    /// element that would otherwise set the status (rare, but tolerated).
    pub(crate) const fn severity(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::Skipped => 1,
            Self::Failed => 2,
            Self::Error => 3,
        }
    }
}

/// Everything that can go wrong while parsing a JUnit-XML report.
#[derive(Debug)]
pub enum JunitError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The input was not well-formed XML, or an attribute value could not be
    /// decoded. Carries the underlying message rather than quick-xml's error
    /// type directly, so this enum's shape does not depend on quick-xml's
    /// internals.
    Xml(String),
    /// The input never opened a `<testsuite>` or `<testsuites>` root element
    /// (covers empty input and non-XML text).
    MissingRoot,
    /// The input ended with elements still open (truncated XML).
    UnexpectedEof,
    /// The input file is larger than [`MAX_INPUT_BYTES`] and was refused before
    /// reading, so a huge or hostile report cannot exhaust memory.
    TooLarge { bytes: u64, max: u64 },
}

impl fmt::Display for JunitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "could not read JUnit-XML report: {err}"),
            Self::Xml(message) => write!(f, "could not parse JUnit-XML report: {message}"),
            Self::MissingRoot => {
                write!(f, "input has no <testsuite> or <testsuites> root element")
            }
            Self::UnexpectedEof => write!(f, "input ended with elements still open"),
            Self::TooLarge { bytes, max } => {
                write!(
                    f,
                    "JUnit-XML report is too large ({bytes} bytes; limit is {max})"
                )
            }
        }
    }
}

impl std::error::Error for JunitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Xml(_) | Self::MissingRoot | Self::UnexpectedEof | Self::TooLarge { .. } => None,
        }
    }
}

impl From<std::io::Error> for JunitError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Read `path` and parse it as a JUnit-XML report.
///
/// # Errors
///
/// Returns [`JunitError::Io`] if the file cannot be read, or any other
/// [`JunitError`] variant the content triggers (see [`parse_str`]).
pub fn parse_file(path: &Path) -> Result<JunitReport, JunitError> {
    ensure_within_limit(fs::metadata(path)?.len(), MAX_INPUT_BYTES)?;
    let xml = fs::read_to_string(path)?;
    parse_str(&xml)
}

/// Largest JUnit-XML report `sooth` will read. Real reports are kilobytes to a
/// few megabytes; 256 MiB is far above any legitimate report and refuses a
/// pathological or hostile file before it is read into memory.
const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Refuse inputs larger than `max` bytes.
fn ensure_within_limit(bytes: u64, max: u64) -> Result<(), JunitError> {
    if bytes > max {
        Err(JunitError::TooLarge { bytes, max })
    } else {
        Ok(())
    }
}

/// Parse a JUnit-XML report from a string.
///
/// Tolerates either a `<testsuites>` or a bare `<testsuite>` root, unknown
/// attributes and elements, and a missing `time` attribute (zero duration).
///
/// # Errors
///
/// Returns [`JunitError::MissingRoot`] if the input never opens a
/// `<testsuite>`/`<testsuites>` element (covers empty input and non-XML
/// text), [`JunitError::UnexpectedEof`] if the input is truncated (elements
/// left open), or [`JunitError::Xml`] for any other malformed-XML condition.
/// Never panics.
pub fn parse_str(xml: &str) -> Result<JunitReport, JunitError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut test_cases = Vec::new();
    let mut open_case: Option<OpenTestCase> = None;
    let mut depth: i64 = 0;
    let mut seen_root = false;

    loop {
        let event = reader
            .read_event()
            .map_err(|err| JunitError::Xml(err.to_string()))?;
        match event {
            Event::Start(tag) => {
                depth += 1;
                match tag.local_name().as_ref() {
                    b"testsuite" | b"testsuites" => seen_root = true,
                    b"testcase" => {
                        open_case = Some(OpenTestCase::new(&tag, depth - 1)?);
                    }
                    other => mark_status(open_case.as_mut(), other),
                }
            }
            Event::Empty(tag) => match tag.local_name().as_ref() {
                b"testsuite" | b"testsuites" => seen_root = true,
                b"testcase" => test_cases.push(OpenTestCase::new(&tag, depth)?.finish()),
                other => mark_status(open_case.as_mut(), other),
            },
            Event::End(_) => {
                depth -= 1;
                if open_case
                    .as_ref()
                    .is_some_and(|case| case.close_depth == depth)
                {
                    // Safety of the expect: guarded by the is_some_and above.
                    test_cases.push(open_case.take().expect("checked above").finish());
                }
            }
            Event::Eof => break,
            // Text, CData, comments, the XML declaration, processing
            // instructions, DOCTYPE and entity references carry no status or
            // structural information this parser needs.
            _ => {}
        }
    }

    if depth != 0 {
        return Err(JunitError::UnexpectedEof);
    }
    if !seen_root {
        return Err(JunitError::MissingRoot);
    }

    Ok(JunitReport { test_cases })
}

/// A `<testcase>` currently being scanned for a `<failure>`/`<error>`/
/// `<skipped>` child, tracked until the `Event::End` that closes it.
struct OpenTestCase {
    name: String,
    classname: Option<String>,
    duration: Duration,
    status: TestStatus,
    /// The `depth` value the matching `Event::End` brings the reader back to.
    close_depth: i64,
}

impl OpenTestCase {
    fn new(tag: &BytesStart, close_depth: i64) -> Result<Self, JunitError> {
        let mut name = String::new();
        let mut classname = None;
        let mut duration = Duration::ZERO;

        for attribute in tag.attributes() {
            let attribute = attribute.map_err(|err| JunitError::Xml(err.to_string()))?;
            let value = attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|err| JunitError::Xml(err.to_string()))?;
            match attribute.key.local_name().as_ref() {
                b"name" => name = value.into_owned(),
                b"classname" => classname = Some(value.into_owned()),
                b"time" => duration = parse_seconds(&value),
                _ => {}
            }
        }

        Ok(Self {
            name,
            classname,
            duration,
            status: TestStatus::Passed,
            close_depth,
        })
    }

    fn finish(self) -> TestCase {
        TestCase {
            name: self.name,
            classname: self.classname,
            duration: self.duration,
            status: self.status,
        }
    }
}

/// Upgrade `case`'s status if `local_tag_name` names a status-bearing child
/// element and outranks the status already recorded. A no-op outside a
/// `<testcase>` (`case` is `None`) or for an unrecognised tag.
fn mark_status(case: Option<&mut OpenTestCase>, local_tag_name: &[u8]) {
    let Some(case) = case else {
        return;
    };
    let candidate = match local_tag_name {
        b"error" => TestStatus::Error,
        b"failure" => TestStatus::Failed,
        b"skipped" => TestStatus::Skipped,
        _ => return,
    };
    if candidate.severity() > case.status.severity() {
        case.status = candidate;
    }
}

/// Parse a `time` attribute value (seconds, possibly fractional) into a
/// [`Duration`], defaulting to zero for anything that is not a finite,
/// non-negative number. `Duration::from_secs_f64` panics on negative,
/// infinite or NaN input, so this guard is load-bearing, not defensive
/// styling: a `time="-1"` or `time="nan"` in the wild must not crash `sooth`.
fn parse_seconds(value: &str) -> Duration {
    // Some non-English locales emit a decimal comma ("12,5"); normalize it so
    // those durations are not silently dropped to zero and lost from rankings.
    let normalized = value.trim().replace(',', ".");
    match normalized.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && seconds >= 0.0 => Duration::from_secs_f64(seconds),
        _ => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::{ensure_within_limit, parse_file, parse_str, JunitError, TestStatus};
    use std::path::Path;
    use std::time::Duration;

    /// Resolves a fixture path relative to the crate root, independent of
    /// the working directory `cargo test` is invoked from.
    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn qualified_name_is_classname_qualified_or_bare() {
        let report = parse_str(
            r#"<testsuite><testcase classname="A.B" name="t"/><testcase name="loose"/></testsuite>"#,
        )
        .unwrap();
        assert_eq!(report.test_cases[0].qualified_name(), "A.B::t");
        assert_eq!(report.test_cases[1].qualified_name(), "loose");
    }

    #[test]
    fn parses_the_empty_testsuite_fixture_as_a_valid_empty_report() {
        let report = super::parse_str(include_str!("../tests/fixtures/empty_testsuite.xml"))
            .expect("an empty suite is a valid report, not an error");
        assert!(report.test_cases.is_empty());
    }

    #[test]
    fn parses_the_missing_durations_fixture_defaulting_times_to_zero() {
        let report = super::parse_str(include_str!("../tests/fixtures/missing_durations.xml"))
            .expect("missing time attributes are tolerated");
        assert_eq!(report.test_cases.len(), 3);
        assert_eq!(report.test_cases[0].duration, std::time::Duration::ZERO);
        assert_eq!(report.test_cases[1].status, super::TestStatus::Skipped);
        assert_eq!(report.test_cases[2].status, super::TestStatus::Error);
    }

    #[test]
    fn parses_the_pytest_testsuites_fixture() {
        let report = parse_file(&fixture("pytest_testsuites.xml")).unwrap();
        assert_eq!(report.test_cases.len(), 2);
        assert_eq!(report.test_cases[0].name, "test_addition");
        assert_eq!(report.test_cases[0].status, TestStatus::Passed);
        assert_eq!(report.test_cases[1].name, "test_subtraction");
        assert_eq!(report.test_cases[1].status, TestStatus::Failed);
    }

    #[test]
    fn parses_the_phpunit_nested_testsuites_fixture() {
        let report = parse_file(&fixture("phpunit_nested_testsuites.xml")).unwrap();
        assert_eq!(report.test_cases.len(), 2);
        assert_eq!(report.test_cases[0].name, "testAddsTwoNumbers");
        assert_eq!(report.test_cases[0].status, TestStatus::Passed);
        assert_eq!(report.test_cases[1].name, "testDivisionByZeroThrows");
        assert_eq!(report.test_cases[1].status, TestStatus::Error);
        assert_eq!(
            report.test_cases[1].classname,
            Some("Tests.Unit.CalculatorTest".to_owned())
        );
    }

    #[test]
    fn parses_the_jest_junit_bare_testsuite_fixture() {
        let report = parse_file(&fixture("jest_junit_bare_testsuite.xml")).unwrap();
        assert_eq!(report.test_cases.len(), 2);
        assert_eq!(report.test_cases[0].status, TestStatus::Passed);
        assert_eq!(report.test_cases[1].status, TestStatus::Skipped);
    }

    #[test]
    fn parses_the_gotestsum_bare_testsuite_fixture() {
        let report = parse_file(&fixture("gotestsum_bare_testsuite.xml")).unwrap();
        assert_eq!(report.test_cases.len(), 3);
        assert_eq!(report.test_cases[0].status, TestStatus::Passed);
        assert_eq!(report.test_cases[1].status, TestStatus::Passed);
        assert_eq!(report.test_cases[2].status, TestStatus::Failed);
    }

    #[test]
    fn a_missing_fixture_file_is_an_error_not_a_panic() {
        assert!(parse_file(&fixture("does-not-exist.xml")).is_err());
    }

    #[test]
    fn parses_a_testsuites_root_with_a_passed_case() {
        let report = parse_str(
            r#"<testsuites>
                <testsuite name="suite">
                    <testcase name="test_ok" classname="pkg.Mod" time="0.5" />
                </testsuite>
            </testsuites>"#,
        )
        .unwrap();

        assert_eq!(report.test_cases.len(), 1);
        let case = &report.test_cases[0];
        assert_eq!(case.name, "test_ok");
        assert_eq!(case.classname, Some("pkg.Mod".to_owned()));
        assert_eq!(case.duration, Duration::from_secs_f64(0.5));
        assert_eq!(case.status, TestStatus::Passed);
    }

    #[test]
    fn parses_a_bare_testsuite_root() {
        let report =
            parse_str(r#"<testsuite name="suite"><testcase name="a" /></testsuite>"#).unwrap();
        assert_eq!(report.test_cases.len(), 1);
        assert_eq!(report.test_cases[0].status, TestStatus::Passed);
    }

    #[test]
    fn a_failure_child_marks_the_case_failed() {
        let report = parse_str(
            r#"<testsuite>
                <testcase name="a" time="1.5">
                    <failure message="boom">trace</failure>
                </testcase>
            </testsuite>"#,
        )
        .unwrap();
        assert_eq!(report.test_cases[0].status, TestStatus::Failed);
        assert_eq!(report.test_cases[0].duration, Duration::from_secs_f64(1.5));
    }

    #[test]
    fn an_error_child_marks_the_case_error() {
        let report = parse_str(
            r#"<testsuite><testcase name="a"><error message="oops"/></testcase></testsuite>"#,
        )
        .unwrap();
        assert_eq!(report.test_cases[0].status, TestStatus::Error);
    }

    #[test]
    fn a_skipped_child_marks_the_case_skipped() {
        let report =
            parse_str(r#"<testsuite><testcase name="a"><skipped/></testcase></testsuite>"#)
                .unwrap();
        assert_eq!(report.test_cases[0].status, TestStatus::Skipped);
    }

    #[test]
    fn error_outranks_failure_and_skipped_on_the_same_case() {
        let report = parse_str(
            r#"<testsuite>
                <testcase name="a">
                    <skipped/>
                    <failure/>
                    <error/>
                </testcase>
            </testsuite>"#,
        )
        .unwrap();
        assert_eq!(report.test_cases[0].status, TestStatus::Error);
    }

    #[test]
    fn a_missing_time_attribute_is_zero_duration() {
        let report = parse_str(r#"<testsuite><testcase name="a"/></testsuite>"#).unwrap();
        assert_eq!(report.test_cases[0].duration, Duration::ZERO);
    }

    #[test]
    fn a_negative_or_non_numeric_time_defaults_to_zero_instead_of_panicking() {
        for time in ["-1", "nan", "inf", "-inf", "not-a-number", ""] {
            let xml = format!(r#"<testsuite><testcase name="a" time="{time}"/></testsuite>"#);
            let report = parse_str(&xml).unwrap();
            assert_eq!(report.test_cases[0].duration, Duration::ZERO, "time={time}");
        }
    }

    #[test]
    fn a_decimal_comma_time_is_parsed_not_dropped() {
        let report =
            parse_str(r#"<testsuite><testcase name="a" time="12,5"/></testsuite>"#).unwrap();
        assert_eq!(report.test_cases[0].duration, Duration::from_secs_f64(12.5));
    }

    #[test]
    fn oversized_input_is_refused_before_reading() {
        assert!(ensure_within_limit(10, 10).is_ok());
        assert!(matches!(
            ensure_within_limit(11, 10),
            Err(JunitError::TooLarge { bytes: 11, max: 10 })
        ));
    }

    #[test]
    fn unknown_attributes_and_elements_are_ignored() {
        let report = parse_str(
            r#"<testsuites tests="1" unknown-attr="x">
                <testsuite name="s" some-vendor-flag="true">
                    <properties><property name="os" value="linux"/></properties>
                    <testcase name="a" time="0.1" some-vendor-attr="y">
                        <system-out>noise</system-out>
                    </testcase>
                </testsuite>
            </testsuites>"#,
        )
        .unwrap();
        assert_eq!(report.test_cases.len(), 1);
        assert_eq!(report.test_cases[0].status, TestStatus::Passed);
    }

    #[test]
    fn empty_input_is_an_error_not_a_panic() {
        assert!(matches!(parse_str(""), Err(JunitError::MissingRoot)));
    }

    #[test]
    fn non_xml_text_is_an_error_not_a_panic() {
        assert!(matches!(
            parse_str("this is not xml at all"),
            Err(JunitError::MissingRoot)
        ));
    }

    #[test]
    fn truncated_xml_is_an_error_not_a_panic() {
        let truncated = r#"<testsuites><testsuite><testcase name="a" time="0.1">"#;
        assert!(matches!(
            parse_str(truncated),
            Err(JunitError::UnexpectedEof)
        ));
    }

    #[test]
    fn truncated_mid_attribute_is_an_error_not_a_panic() {
        let truncated = r#"<testsuites><testsuite><testcase name="a"#;
        assert!(parse_str(truncated).is_err());
    }

    #[test]
    fn garbage_bytes_reinterpreted_as_text_never_panic() {
        let samples: [&[u8]; 5] = [
            &[0xFF, 0xFE, 0x00, 0x01, 0x02],
            &[0x00; 32],
            b"<<<>>>&&&;;;",
            b"<testsuite><testcase name=\"a\"",
            &[0x3C, 0xE2, 0x9C, 0x93, 0x3E],
        ];
        for sample in samples {
            let text = String::from_utf8_lossy(sample);
            // The only contract under test is "never panics"; both Ok and Err
            // are acceptable outcomes for arbitrary bytes.
            let _ = parse_str(&text);
        }
    }

    #[test]
    fn xml_invalid_chars_in_test_output_do_not_break_an_otherwise_valid_report() {
        // Real runners write these when a test leaks raw bytes into its output.
        for junk in ['\u{FFFF}', '\0'] {
            let xml = format!(
                r#"<testsuites><testsuite name="s" tests="1"><testcase classname="c" name="t"><system-out>garbage: {junk} here</system-out></testcase></testsuite></testsuites>"#
            );
            let report = parse_str(&xml).expect("XML-invalid chars in output must not reject");
            assert_eq!(report.test_cases.len(), 1);
            assert_eq!(report.test_cases[0].status, TestStatus::Passed);
        }
    }

    #[test]
    fn deeply_nested_junk_does_not_panic_or_overflow_the_stack() {
        let depth = 50_000;
        let mut xml = String::new();
        for _ in 0..depth {
            xml.push_str("<a>");
        }
        for _ in 0..depth {
            xml.push_str("</a>");
        }
        // Well-formed but has no testsuite/testcase anywhere: MissingRoot.
        assert!(matches!(parse_str(&xml), Err(JunitError::MissingRoot)));
    }

    #[test]
    fn many_malformed_inputs_never_panic() {
        let malformed = [
            "",
            " ",
            "\0",
            "<",
            ">",
            "</>",
            "<testsuite",
            "<testsuite>",
            "<testsuite><testcase",
            "<testsuite><testcase name=",
            "<testsuite><testcase name=\"a\"",
            "<testsuite><testcase name=\"a\">",
            "<testsuite><testcase name=\"a\"></testcase>",
            "<testsuite></testsuite><testsuite>",
            "&amp&amp;",
            "<testsuite time=\"nan\"><testcase name=\"a\" time=\"nan\"/></testsuite>",
            "not xml, just words, 12345, !@#$%^&*()",
            "<?xml version=\"1.0\"?>",
            "<?xml version=\"1.0\"?><testsuite",
        ];
        for input in malformed {
            let _ = parse_str(input);
        }
    }
}
