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
#[test]
fn a_failing_wrapped_command_exits_one() {
    let output = sooth()
        .args(["run", "--color", "never", "--", "false"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("runner exit=1"), "got: {stdout:?}");
    assert!(stdout.contains("result: FAILED"), "got: {stdout:?}");
}

#[test]
fn an_unspawnable_command_is_sooths_error() {
    let output = sooth()
        .args(["run", "--", "sooth-no-such-binary-xyzzy"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("failed to run"), "got: {stderr:?}");
}

#[test]
fn reportless_json_is_rejected_with_exit_two() {
    let output = sooth()
        .args(["run", "--json", "--", "true"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("requires a report"), "got: {stderr:?}");
}

#[test]
fn a_signal_killed_run_reports_the_signal_and_exits_one() {
    let output = sooth()
        .args(["run", "--color", "never", "--", "sh", "-c", "kill -TERM $$"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("runner signal 15"), "got: {stdout:?}");
}

#[test]
fn the_runner_report_mismatch_is_called_out_on_stderr() {
    // The wrapped command writes a failing report but exits 0: the report
    // wins (exit 1) and the mismatch note lands on stderr, not stdout.
    let (report, write_report) = fresh_report("mismatch");
    let output = sooth()
        .args([
            "run",
            "--junit",
            &report.display().to_string(),
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

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("the runner exited 0 but the report shows"),
        "got: {stderr:?}"
    );
}

#[test]
fn an_unusable_report_after_a_crashed_runner_keeps_the_run_facts() {
    // The runner writes garbage instead of XML and exits nonzero; sooth
    // must point at the crash instead of only naming an unparsable file.
    let report =
        std::env::temp_dir().join(format!("sooth-contract-crash-{}.xml", std::process::id()));
    let write_garbage = format!("echo 'PHP Fatal error' > '{}'; exit 255", report.display());
    let output = sooth()
        .args([
            "run",
            "--junit",
            &report.display().to_string(),
            "--color",
            "never",
            "--",
            "sh",
            "-c",
            &write_garbage,
        ])
        .output()
        .expect("sooth should run");
    let _ = std::fs::remove_file(&report);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("failed to parse"), "got: {stderr:?}");
    assert!(stderr.contains("runner exit=255"), "got: {stderr:?}");
    assert!(stderr.contains("output above"), "got: {stderr:?}");
}

#[test]
fn repeated_runs_report_mixed_outcomes_as_flaky() {
    let dir = std::env::temp_dir();
    let report = dir.join(format!("sooth-contract-flaky-{}.xml", std::process::id()));
    let marker = dir.join(format!(
        "sooth-contract-flaky-marker-{}",
        std::process::id()
    ));
    // Run 1: the test fails (runner exits 1). Run 2: it passes. Mixed = flaky.
    let script = format!(
        "if [ -f '{marker}' ]; then printf '<testsuite><testcase classname=\"c\" name=\"wobbly\"/></testsuite>' > '{report}'; else printf '<testsuite><testcase classname=\"c\" name=\"wobbly\"><failure/></testcase></testsuite>' > '{report}'; touch '{marker}'; exit 1; fi",
        marker = marker.display(),
        report = report.display()
    );
    let output = sooth()
        .args([
            "run",
            "--runs",
            "2",
            "--junit",
            &report.display().to_string(),
            "--color",
            "never",
            "--",
            "sh",
            "-c",
            &script,
        ])
        .output()
        .expect("sooth should run");
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_file(&marker);

    assert_eq!(output.status.code(), Some(1), "a flaky run failed run 1");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(
        stdout.contains("flaky tests (mixed outcomes):"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("c::wobbly failed 1 of 2 observed runs (50%)"),
        "got: {stdout:?}"
    );
}

#[test]
fn a_preset_runner_that_stops_writing_reports_fails_loudly() {
    use std::os::unix::fs::PermissionsExt;
    // Run 1 writes a report; run 2 writes nothing. Because sooth deletes the
    // preset report before every run, run 2 must fail loudly instead of
    // silently re-serving run 1's truth.
    let dir = std::env::temp_dir().join(format!("sooth-fakebin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("fake bin dir");
    let marker = dir.join("ran-once");
    let fake = dir.join("pytest");
    let script = format!(
        "#!/bin/sh\nout=\"\"\nfor a in \"$@\"; do case \"$a\" in --junit-xml=*) out=\"${{a#--junit-xml=}}\";; esac; done\nif [ ! -f '{marker}' ]; then printf '<testsuite><testcase name=\"ok\"/></testsuite>' > \"$out\"; touch '{marker}'; fi\nexit 0\n",
        marker = marker.display()
    );
    std::fs::write(&fake, script).expect("fake pytest");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let path_env = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = sooth()
        .env("PATH", path_env)
        .args([
            "run", "--runs", "2", "--preset", "pytest", "--color", "never", "--", "pytest",
        ])
        .output()
        .expect("sooth should run");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        output.status.code(),
        Some(2),
        "silent run 2 is sooth's error"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("wrote no JUnit-XML report"),
        "got: {stderr:?}"
    );
}
