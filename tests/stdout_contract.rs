//! What `sooth run` puts on stdout is a public contract (see the `--json`
//! entry in `DECISIONS.md`), so it is pinned against the real binary.
#![cfg(unix)] // the wrapped commands are `true` and `sh`, which are Unix-only

use std::path::PathBuf;
use std::process::Command;

fn sooth() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sooth"))
}

fn fixture() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/pytest_testsuites.xml"
    )
}

/// A per-test temp path for the report. The wrapped command copies the
/// fixture into place *during the run*, because a `--junit` file that
/// predates the run is rejected as stale.
fn fresh_report(tag: &str) -> (PathBuf, String) {
    let path =
        std::env::temp_dir().join(format!("sooth-contract-{tag}-{}.xml", std::process::id()));
    let write_during_run = format!("cp '{}' '{}'", fixture(), path.display());
    (path, write_during_run)
}

#[test]
fn bare_json_prints_exactly_one_stdout_line_of_json() {
    let (report, write_report) = fresh_report("bare-json");
    let output = sooth()
        .args([
            "run",
            "--junit",
            &report.display().to_string(),
            "--json",
            "--color",
            "never",
            "--",
            "sh",
            "-c",
            &write_report,
        ])
        .output()
        .expect("sooth should run");
    let _ = std::fs::remove_file(&report);

    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "bare --json must print exactly one line, got: {stdout:?}"
    );
    assert!(lines[0].starts_with(r#"{"schema_version":1,"#));
    assert!(lines[0].ends_with('}'));
    // The fixture contains a failure while the runner exits 0: the report wins.
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_plain_run_ends_with_a_verdict_line() {
    let output = sooth()
        .args(["run", "--color", "never", "--", "true"])
        .output()
        .expect("sooth should run");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("run 1/1: runner exit=0")),
        "expected a labeled per-run line, got: {stdout:?}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("result: PASSED")),
        "expected a closing verdict line, got: {stdout:?}"
    );
}

#[test]
fn json_to_a_file_keeps_the_human_report_on_stdout() {
    let (report, write_report) = fresh_report("json-file");
    let json_path =
        std::env::temp_dir().join(format!("sooth-contract-{}.json", std::process::id()));
    let output = sooth()
        .args([
            "run",
            "--junit",
            &report.display().to_string(),
            &format!("--json={}", json_path.display()),
            "--color",
            "never",
            "--",
            "sh",
            "-c",
            &write_report,
        ])
        .output()
        .expect("sooth should run");

    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let written = std::fs::read_to_string(&json_path).expect("the JSON file should exist");
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_file(&json_path);

    assert!(stdout.contains("tests: 2 total"), "got: {stdout:?}");
    assert!(stdout.contains("result: FAILED"), "got: {stdout:?}");
    assert!(written.starts_with(r#"{"schema_version":1,"#));
}

#[test]
fn a_junit_report_that_predates_the_run_is_rejected_as_stale() {
    // Write the report BEFORE the run; the wrapped command touches nothing.
    let report =
        std::env::temp_dir().join(format!("sooth-contract-stale-{}.xml", std::process::id()));
    std::fs::copy(fixture(), &report).expect("fixture should copy");
    // Age it well past the freshness tolerance.
    let old = filetime_from_secs_ago(&report, 3600);
    assert!(old.is_ok(), "could not age the file: {old:?}");

    let output = sooth()
        .args([
            "run",
            "--junit",
            &report.display().to_string(),
            "--color",
            "never",
            "--",
            "true",
        ])
        .output()
        .expect("sooth should run");
    let _ = std::fs::remove_file(&report);

    assert_eq!(
        output.status.code(),
        Some(2),
        "stale report is sooth's error"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("predates the test command"),
        "got: {stderr:?}"
    );
}

/// Set the file's mtime `secs` into the past using `touch -t` — std has no
/// stable set-mtime API and a dev-dependency for one test is not worth it.
/// Covers BSD (`date -v`) and GNU (`date -d`); busybox `date` is not
/// supported and fails this test's `is_ok` assert loudly rather than
/// passing silently.
fn filetime_from_secs_ago(path: &std::path::Path, secs: u64) -> std::io::Result<()> {
    let status = Command::new("sh")
        .args([
            "-c",
            &format!(
                "touch -m -t \"$(date -v-{secs}S '+%Y%m%d%H%M.%S' 2>/dev/null || date -d '-{secs} seconds' '+%Y%m%d%H%M.%S' 2>/dev/null)\" '{}'",
                path.display()
            ),
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("touch failed"))
    }
}
