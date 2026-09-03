//! What `sooth run` puts on stdout is a public contract (see the `--json`
//! entry in `DECISIONS.md`), so it is pinned against the real binary.
#![cfg(unix)] // the wrapped commands are `true` and `sh`, which are Unix-only

use std::path::PathBuf;
use std::process::Command;

fn sooth() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sooth"));
    // History writes `.sooth/` into the working directory; the contract
    // suite must not seed the repo's own history. Every test passes
    // absolute paths, so the cwd itself is free to be scratch.
    let cwd = std::env::temp_dir().join(format!("sooth-contract-cwd-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&cwd);
    command.current_dir(cwd);
    command
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
            .is_some_and(|line| line.starts_with("result: ✓ PASSED")),
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
    assert!(stdout.contains("result: ✗ FAILED"), "got: {stdout:?}");
    assert!(written.starts_with(r#"{"schema_version":1,"#));
}

#[test]
fn a_report_with_zero_tests_passes_but_says_it_proved_nothing() {
    let (cwd, mut command) = sooth_in("zero-tests");
    let report = cwd.join("empty.xml");
    let script = format!("printf '<testsuite></testsuite>' > '{}'", report.display());
    let output = command
        .args([
            "run",
            "--no-history",
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
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    // Runner and report agree nothing failed: the exit contract holds …
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    // … but an empty suite, a filter matching nothing, or a wrong directory
    // must never read as bold green proof.
    assert!(
        stdout.contains("but the report shows 0 tests, so this run proved nothing"),
        "got: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
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
    assert!(stdout.contains("result: ✗ FAILED"), "got: {stdout:?}");
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

/// Copy the fake-phpunit fixture (`tests/fixtures/verify_runner.sh`) into
/// `dir`. Each test picks the verify re-run's outcome by setting the
/// `SOOTH_TEST_VERIFY_CASE` environment variable, which sooth passes
/// through to the runner it spawns.
fn write_verify_runner(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/verify_runner.sh"
    );
    let runner = dir.join("runner.sh");
    std::fs::copy(fixture, &runner).expect("copy runner");
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn a_verified_failure_that_passes_on_re_run_reads_flaky_or_order_dependent() {
    let (cwd, mut command) = sooth_in("verify-flake");
    write_verify_runner(&cwd);
    command.env(
        "SOOTH_TEST_VERIFY_CASE",
        r#"<testcase classname="c" name="wob"/>"#,
    );
    let output = command
        .args([
            "run",
            "--verify",
            "--no-history",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(
        output.status.code(),
        Some(1),
        "verification classifies, it never absorbs the failure: {stdout:?}"
    );
    assert!(
        stdout.contains("~ c::wob — flaky or order-dependent (passed on re-run in isolation)"),
        "the verify pass's verdict must reach the per-test line: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn a_verified_failure_that_fails_differently_is_not_called_real() {
    let (cwd, mut command) = sooth_in("verify-different");
    write_verify_runner(&cwd);
    // The suite saw an AssertionError (the fixture's initial run); the
    // re-run dies on a RuntimeException instead — isolation broke the
    // bootstrap, nothing was reproduced.
    command.env(
        "SOOTH_TEST_VERIFY_CASE",
        r#"<testcase classname="c" name="wob"><error type="RuntimeException"/></testcase>"#,
    );
    let output = command
        .args([
            "run",
            "--verify",
            "--no-history",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains(
            "✗ c::wob — failed differently on re-run (AssertionError in the suite, \
             RuntimeException in isolation — not a reproduction)"
        ),
        "a different failure must never read as reproduced: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn a_verified_failure_that_reproduces_reads_real() {
    let (cwd, mut command) = sooth_in("verify-real");
    write_verify_runner(&cwd);
    command.env(
        "SOOTH_TEST_VERIFY_CASE",
        r#"<testcase classname="c" name="wob"><failure/></testcase>"#,
    );
    let output = command
        .args([
            "run",
            "--verify",
            "--no-history",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains("✗ c::wob — real (reproduced on re-run)"),
        "a reproduced failure must be named real: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn verify_with_an_unselectable_preset_is_rejected_up_front_with_exit_two() {
    let output = sooth()
        .args(["run", "--verify", "--preset", "go", "--", "true"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("not supported for this preset"),
        "got: {stderr:?}"
    );
}

/// A sooth invocation in its own scratch cwd, so a `.sooth-quarantine`
/// written for one test never leaks into another.
fn sooth_in(tag: &str) -> (PathBuf, Command) {
    let cwd = std::env::temp_dir().join(format!("sooth-contract-{tag}-cwd-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&cwd);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sooth"));
    command.current_dir(&cwd);
    (cwd, command)
}

#[test]
fn a_quarantined_failure_is_pardoned_with_fail_on_flaky() {
    let (cwd, mut command) = sooth_in("quarantine-hit");
    std::fs::write(
        cwd.join(".sooth-quarantine"),
        "# known flakes\ntests.test_math::test_subtraction\n",
    )
    .expect("quarantine file should write");
    let (report, write_report) = fresh_report("quarantine-hit");
    let output = command
        .args([
            "run",
            "--junit",
            &report.display().to_string(),
            "--fail-on-flaky",
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
    assert_eq!(
        output.status.code(),
        Some(0),
        "a fully quarantined failure must be pardoned, got: {stdout:?}"
    );
    assert!(
        stdout.contains("tests.test_math::test_subtraction"),
        "the pardoned id must be reported, got: {stdout:?}"
    );
    assert!(
        stdout.contains("result: ✓ PASSED — only quarantined flakes failed"),
        "got: {stdout:?}"
    );
}

#[test]
fn an_unlisted_failure_still_fails_with_fail_on_flaky() {
    let (cwd, mut command) = sooth_in("quarantine-miss");
    std::fs::write(cwd.join(".sooth-quarantine"), "some.other::test_flake\n")
        .expect("quarantine file should write");
    let (report, write_report) = fresh_report("quarantine-miss");
    let output = command
        .args([
            "run",
            "--junit",
            &report.display().to_string(),
            "--fail-on-flaky",
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
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("result: ✗ FAILED"), "got: {stdout:?}");
}

#[test]
fn reportless_fail_on_flaky_is_rejected_with_exit_two() {
    let output = sooth()
        .args(["run", "--fail-on-flaky", "--", "true"])
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
fn a_failing_runner_with_a_green_report_is_called_out_on_stderr() {
    // The wrapped command writes an all-passing report but exits nonzero:
    // the failure wins (exit 1) and the disagreement lands on stderr.
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-green-mismatch-{}.xml",
        std::process::id()
    ));
    let write_green_then_fail = format!(
        "printf '<testsuite><testcase classname=\"c\" name=\"ok\"/></testsuite>' > '{}'; exit 3",
        report.display()
    );
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
            &write_green_then_fail,
        ])
        .output()
        .expect("sooth should run");
    let _ = std::fs::remove_file(&report);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("the runner failed but the report shows 0 failing tests"),
        "got: {stderr:?}"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("result: ✗ FAILED"), "got: {stdout:?}");
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
        stdout.contains("~ c::wobbly — flaky (1 of 2 runs now), new (nothing in history)"),
        "the repeat pass and the history must share one line per test: {stdout:?}"
    );
}

#[test]
fn a_monotone_flip_and_a_lone_failure_are_named_for_what_they_are() {
    // Run 1: "polluted" passes and "drift-1" fails. Runs 2 and 3: "polluted"
    // fails and a differently-named drift passes — the #113 and #137 shapes
    // in one invocation.
    let (cwd, mut command) = sooth_in("sequence-shapes");
    let report = cwd.join("report.xml");
    let script = format!(
        r#"n=$(cat runcount 2>/dev/null || echo 0); n=$((n+1)); echo $n > runcount
if [ "$n" = "1" ]; then
  printf '<testsuite><testcase classname="c" name="polluted"/><testcase classname="c" name="drift-1"><failure/></testcase></testsuite>' > '{report}'
else
  printf '<testsuite><testcase classname="c" name="polluted"><failure/></testcase><testcase classname="c" name="drift-%s"/></testsuite>' "$n" > '{report}'
fi"#,
        report = report.display()
    );
    let output = command
        .args([
            "run",
            "--runs",
            "3",
            "--no-history",
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
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(
        output.status.code(),
        Some(1),
        "failures stay red: {stdout:?}"
    );
    assert!(
        stdout.contains(
            "~ c::polluted — green up to run 1, then red for every later run \
             (the environment may have changed between runs)"
        ),
        "a flip that never returns must not be called flaky: {stdout:?}"
    );
    assert!(
        stdout.contains(
            "✗ c::drift-1 — failed its only observed run (absent from the other 2 \
             — a name that changes per run hides flakiness)"
        ),
        "one sighting must not be called broken: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
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

#[test]
fn runs_in_a_different_order_weaken_the_flaky_label() {
    // The wrapped script lists the same two tests in opposite order per run,
    // with one of them mixed — what --order-by=defects does to a repeat.
    let report =
        std::env::temp_dir().join(format!("sooth-contract-reorder-{}.xml", std::process::id()));
    let marker = std::env::temp_dir().join(format!("sooth-reorder-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let script = format!(
        "if [ -f '{m}' ]; then rm '{m}'; \
         printf '<testsuite><testcase classname=\"c\" name=\"steady\"/><testcase classname=\"c\" name=\"wobbly\"/></testsuite>' > '{r}'; \
         else touch '{m}'; \
         printf '<testsuite><testcase classname=\"c\" name=\"wobbly\"><failure/></testcase><testcase classname=\"c\" name=\"steady\"/></testsuite>' > '{r}'; fi",
        m = marker.display(),
        r = report.display()
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
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "run 1 failed: {stdout:?}");
    assert!(
        stdout.contains(
            "flaky or order-dependent (1 of 2 runs now; run 2 did not share run 1's order)"
        ),
        "a reordered repeat must not claim plain flakiness: {stdout:?}"
    );
}

/// A scratch git repo (one commit, `.sooth/` ignored) for history tests;
/// returns `None` when git is unavailable.
fn scratch_repo(tag: &str) -> Option<PathBuf> {
    Command::new("git").arg("--version").output().ok()?;
    let dir = std::env::temp_dir().join(format!("sooth-contract-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(".gitignore"), ".sooth/\n").ok()?;
    for args in [
        &["init", "-q"][..],
        &["add", "."][..],
        &["commit", "-q", "-m", "init"][..],
    ] {
        git_in(&dir, args);
    }
    Some(dir)
}

/// Run git in `dir`, asserting success.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "git {args:?} failed");
}

/// A fake phpunit at `dir/runner.sh` that writes a green report to whatever
/// `--log-junit` path the preset injects.
fn write_green_runner(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let runner = dir.join("runner.sh");
    std::fs::write(
        &runner,
        concat!(
            "#!/bin/sh\n",
            "report=\"\"; prev=\"\"\n",
            "for a in \"$@\"; do\n",
            "  if [ \"$prev\" = \"--log-junit\" ]; then report=\"$a\"; fi\n",
            "  prev=\"$a\"\n",
            "done\n",
            "printf '<testsuite><testcase classname=\"gate\" name=\"ok\"/></testsuite>' > \"$report\"\n",
        ),
    )
    .expect("write runner");
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn history_accumulates_across_invocations_and_reports_proven_flakes() {
    let Some(dir) = scratch_repo("history") else {
        return; // no git: identity degrades to unknown, covered by unit tests
    };
    // The report lives outside the repo so the working tree stays clean.
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-history-report-{}.xml",
        std::process::id()
    ));
    let run = |cases: &str| {
        let script = format!(
            "printf '<testsuite>{cases}</testsuite>' > '{}'",
            report.display()
        );
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args([
                "run",
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
            .expect("sooth should run")
    };

    let first = run(r#"<testcase classname="c" name="wob"/>"#);
    let stdout = String::from_utf8(first.stdout).expect("stdout should be UTF-8");
    assert_eq!(first.status.code(), Some(0));
    assert!(
        !stdout.contains("flaky per history"),
        "an all-green history reported flakes: {stdout:?}"
    );

    // The run this test failed in gets its verdict from the explanation; the
    // history section would only repeat those counts.
    let second = run(r#"<testcase classname="c" name="wob"><failure/></testcase>"#);
    let stdout = String::from_utf8(second.stdout).expect("stdout should be UTF-8");
    assert_eq!(second.status.code(), Some(1));
    assert!(
        !stdout.contains("flaky per history"),
        "the history section repeated a failure the explanation already covers: {stdout:?}"
    );
    assert!(
        stdout.contains("~ c::wob — known flake (1 of 2 in history, 50%)"),
        "got: {stdout:?}"
    );

    // Green again: nothing failed, so the accumulated verdict is the news.
    let third = run(r#"<testcase classname="c" name="wob"/>"#);
    let stdout = String::from_utf8(third.stdout).expect("stdout should be UTF-8");
    assert_eq!(third.status.code(), Some(0));
    assert!(
        stdout.contains("flaky per history (mixed outcomes on one commit):"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("c::wob failed 1 of 3 observed runs (33%)"),
        "got: {stdout:?}"
    );

    let history = std::fs::read_to_string(dir.join(".sooth/history.jsonl"))
        .expect("history should have been written");
    assert_eq!(history.lines().count(), 3);
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_red_run_labels_its_failures_against_the_accumulated_history() {
    let Some(dir) = scratch_repo("explain-run") else {
        return; // no git: identity degrades to unknown, covered by unit tests
    };
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-explain-run-{}.xml",
        std::process::id()
    ));
    let run = |cases: &str| {
        let script = format!(
            "printf '<testsuite>{cases}</testsuite>' > '{}'",
            report.display()
        );
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args([
                "run",
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
            .expect("sooth should run")
    };

    // Both tests end up proven flaky; only `wob` fails in the second run.
    run(
        r#"<testcase classname="c" name="wob"/><testcase classname="c" name="other"><failure/></testcase>"#,
    );
    let red = run(
        r#"<testcase classname="c" name="wob"><failure/></testcase><testcase classname="c" name="other"/>"#,
    );
    let stdout = String::from_utf8(red.stdout).expect("stdout should be UTF-8");

    assert_eq!(
        red.status.code(),
        Some(1),
        "the diagnosis must not absorb the failure"
    );
    assert!(
        stdout.contains("1 failure — all known flakes, nothing new"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("~ c::wob — known flake (1 of 2 in history, 50%)"),
        "got: {stdout:?}"
    );
    // The other flake did not fail here, so it stays in the history section.
    assert!(
        stdout.contains("also flaky per history (these did not fail this run):"),
        "got: {stdout:?}"
    );
    assert!(stdout.contains("c::other"), "got: {stdout:?}");
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_very_first_run_says_its_failures_read_as_new_for_lack_of_history() {
    let Some(dir) = scratch_repo("explain-first") else {
        return;
    };
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-explain-first-{}.xml",
        std::process::id()
    ));
    let script = format!(
        "printf '<testsuite><testcase classname=\"c\" name=\"t\"><failure/></testcase></testsuite>' > '{}'",
        report.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
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
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    // This run's own observations are in the file by now; only earlier ones
    // could have made the failure read as anything but new.
    assert!(
        stdout.contains("no observations from earlier runs yet"),
        "a first run let \"new\" stand without saying there was nothing to compare against: \
         {stdout:?}"
    );
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explain_classifies_a_report_without_running_or_recording_anything() {
    let Some(dir) = scratch_repo("explain-cmd") else {
        return;
    };
    // Outside the repo: an untracked report makes every run dirty, and a
    // dirty run is never evidence.
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-explain-cmd-{}.xml",
        std::process::id()
    ));
    let run = |cases: &str| {
        let script = format!(
            "printf '<testsuite>{cases}</testsuite>' > '{}'",
            report.display()
        );
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args([
                "run",
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
            .expect("sooth should run")
    };
    run(r#"<testcase classname="c" name="wob"/>"#);
    run(r#"<testcase classname="c" name="wob"><failure/></testcase>"#);
    let history = dir.join(".sooth/history.jsonl");
    let before = std::fs::read_to_string(&history).expect("history should exist");

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "explain",
            "--junit",
            &report.display().to_string(),
            "--color",
            "never",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(
        output.status.code(),
        Some(0),
        "explain diagnoses, it never steers an exit: {stdout:?}"
    );
    assert!(
        stdout.contains("~ c::wob — known flake (1 of 2 in history, 50%)"),
        "got: {stdout:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&history).expect("history should still exist"),
        before,
        "explain recorded observations for a run it never made"
    );
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explain_on_a_green_report_has_nothing_to_explain() {
    let (cwd, mut command) = sooth_in("explain-green");
    let report = cwd.join("green.xml");
    std::fs::write(
        &report,
        r#"<testsuite><testcase classname="c" name="ok"/></testsuite>"#,
    )
    .expect("report should write");
    let output = command
        .args(["explain", "--junit", &report.display().to_string()])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("nothing to explain"), "got: {stdout:?}");
    let _ = std::fs::remove_file(&report);
}

#[test]
fn explain_on_a_red_report_without_history_says_the_evidence_is_empty() {
    let (cwd, mut command) = sooth_in("explain-empty-history");
    let _ = std::fs::remove_dir_all(cwd.join(".sooth"));
    let report = cwd.join("red.xml");
    std::fs::write(
        &report,
        r#"<testsuite><testcase classname="c" name="t"><failure/></testcase></testsuite>"#,
    )
    .expect("report should write");

    let output = command
        .args([
            "explain",
            "--junit",
            &report.display().to_string(),
            "--color",
            "never",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout.contains("✗ c::t — new (nothing in history)"),
        "got: {stdout:?}"
    );
    // "new" against an empty history must say the comparison was vacuous —
    // and an empty history is not the same as one that was not consulted.
    assert!(
        stdout.contains("no observations from earlier runs yet"),
        "got: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn a_preset_run_cleans_its_private_report_dir_up() {
    let (cwd, mut command) = sooth_in("preset-cleanup");
    write_gate_runner(&cwd);
    // sooth reads TMPDIR for its private report dirs: pointing it at a
    // scratch dir makes "everything cleaned up" a checkable fact.
    let scratch_tmp = cwd.join("tmp");
    std::fs::create_dir_all(&scratch_tmp).expect("mkdir");
    command.env("TMPDIR", &scratch_tmp);

    let output = command
        .args([
            "run",
            "--no-history",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");

    assert_eq!(output.status.code(), Some(0));
    let leftovers: Vec<String> = std::fs::read_dir(&scratch_tmp)
        .expect("scratch tmp should exist")
        .filter_map(|entry| {
            entry
                .ok()
                .map(|e| e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "a finished run must remove its private report dir, left: {leftovers:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn the_quarantine_labels_a_failure_without_the_flag_but_never_steers_the_exit() {
    let (cwd, mut command) = sooth_in("quarantine-label");
    std::fs::write(
        cwd.join(".sooth-quarantine"),
        "tests.test_math::test_subtraction\n",
    )
    .expect("quarantine file should write");
    let (report, write_report) = fresh_report("quarantine-label");
    let output = command
        .args([
            "run",
            "--no-history",
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
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(
        output.status.code(),
        Some(1),
        "the list alone must not pardon anything: {stdout:?}"
    );
    assert!(
        stdout.contains("⊘ tests.test_math::test_subtraction — quarantined"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("the run history was not consulted"),
        "a --no-history run must not let \"new\" imply there is no evidence: {stdout:?}"
    );
}

#[test]
fn a_flake_that_only_breaks_in_ci_says_so() {
    let Some(dir) = scratch_repo("environment") else {
        return; // no git: identity degrades to unknown, covered by unit tests
    };
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-environment-{}.xml",
        std::process::id()
    ));
    // Same commit, same order; only the environment and the outcome differ.
    let run = |cases: &str, ci: bool| {
        let script = format!(
            "printf '<testsuite>{cases}</testsuite>' > '{}'",
            report.display()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_sooth"));
        command.current_dir(&dir);
        if ci {
            command.env("CI", "true");
        } else {
            command.env_remove("CI");
        }
        command
            .args([
                "run",
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
            .expect("sooth should run")
    };

    let green = r#"<testcase classname="c" name="wob"/>"#;
    let red = r#"<testcase classname="c" name="wob"><failure/></testcase>"#;
    run(green, false);
    run(green, false);
    run(green, true);
    let output = run(red, true);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert!(
        stdout.contains("known flake (1 of 4 in history, 25%; every failure in ci)"),
        "the environment is the first thing to check, so it belongs on the line: {stdout:?}"
    );
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_history_made_entirely_on_a_dirty_tree_says_why_it_proves_nothing() {
    let Some(dir) = scratch_repo("dirty-history") else {
        return;
    };
    // An untracked file makes every run dirty — the ordinary state of a
    // machine someone is working on.
    std::fs::write(dir.join("scratch.txt"), "work in progress").expect("write");
    let report =
        std::env::temp_dir().join(format!("sooth-contract-dirty-{}.xml", std::process::id()));
    let run = |cases: &str| {
        let script = format!(
            "printf '<testsuite>{cases}</testsuite>' > '{}'",
            report.display()
        );
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args([
                "run",
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
            .expect("sooth should run")
    };

    run(r#"<testcase classname="c" name="wob"/>"#);
    let output = run(r#"<testcase classname="c" name="wob"><failure/></testcase>"#);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert!(
        stdout.contains("✗ c::wob — new (nothing in history)"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("were made on a dirty tree and cannot be evidence"),
        "a dirty history reads exactly like an empty one unless sooth says so: {stdout:?}"
    );
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_ci_evidence_completes_the_local_green_ci_red_proof() {
    let Some(dir) = scratch_repo("import") else {
        return; // no git: identity degrades to unknown, covered by unit tests
    };
    let sha = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git");
    let sha = String::from_utf8(sha.stdout)
        .expect("utf8")
        .trim()
        .to_owned();
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-import-run-{}.xml",
        std::process::id()
    ));

    // Two clean local greens; CI removed so they record env "local" even on
    // a CI runner.
    for _ in 0..2 {
        let script = format!(
            "printf '<testsuite><testcase classname=\"c\" name=\"wob\"/></testsuite>' > '{}'",
            report.display()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .env_remove("CI")
            .args([
                "run",
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
        assert_eq!(output.status.code(), Some(0));
    }

    // The "CI artifact": a red report for the same test, same commit.
    let artifact = dir.join("ci-report.xml");
    std::fs::write(
        &artifact,
        r#"<testsuite><testcase classname="c" name="wob"><failure/></testcase></testsuite>"#,
    )
    .expect("write artifact");

    let import = |tag: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .env_remove("CI")
            .args([
                "import",
                "--env",
                "ci",
                "--commit",
                &sha,
                "--color",
                "never",
                "ci-report.xml",
            ])
            .output()
            .unwrap_or_else(|err| panic!("{tag}: {err}"));
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("stdout should be UTF-8"),
        )
    };

    let (code, stdout) = import("first import");
    assert_eq!(code, Some(0), "import judges nothing: {stdout:?}");
    assert!(
        stdout.contains("ci-report.xml: 1 observation"),
        "got: {stdout:?}"
    );
    // Local greens + an imported ci red on one clean commit: the proof the
    // two environments could never produce separately.
    assert!(stdout.contains("every failure in ci"), "got: {stdout:?}");
    assert!(
        stdout.contains("history now holds 3 observations"),
        "got: {stdout:?}"
    );

    // The same file again is bookkeeping, not evidence.
    let (code, stdout) = import("second import");
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("skipped (already imported)"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("history now holds 3 observations"),
        "a re-import grew the history: {stdout:?}"
    );

    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_file_fails_the_whole_import_before_anything_is_written() {
    let (cwd, mut command) = sooth_in("import-atomic");
    std::fs::write(
        cwd.join("good.xml"),
        r#"<testsuite><testcase classname="c" name="ok"/></testsuite>"#,
    )
    .expect("write");
    std::fs::write(cwd.join("bad.xml"), "not xml at all").expect("write");

    let output = command
        .args(["import", "--env", "ci", "good.xml", "bad.xml"])
        .output()
        .expect("sooth should run");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        !cwd.join(".sooth/history.jsonl").exists(),
        "a failed import wrote a partial batch"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn a_red_phpunit_log_is_ci_evidence_like_any_report() {
    let Some(dir) = scratch_repo("log-import") else {
        return; // no git: identity degrades to unknown, covered by unit tests
    };
    let sha = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git");
    let sha = String::from_utf8(sha.stdout)
        .expect("utf8")
        .trim()
        .to_owned();
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-log-import-run-{}.xml",
        std::process::id()
    ));
    for _ in 0..2 {
        let script = format!(
            "printf '<testsuite><testcase classname=\"c\" name=\"wob\"/></testsuite>' > '{}'",
            report.display()
        );
        let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .env_remove("CI")
            .args([
                "run",
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
        assert_eq!(output.status.code(), Some(0));
    }

    // The "CI log" in the exact shape `gh run view --log` hands over: job
    // and step columns, per-line timestamps, ANSI on the banner.
    std::fs::write(
        dir.join("ci.log"),
        "Run Tests / PHPUnit\tUNKNOWN STEP\t2026-08-10T08:34:58.61Z There was 1 failure:\n\
         Run Tests / PHPUnit\tUNKNOWN STEP\t2026-08-10T08:34:58.61Z \n\
         Run Tests / PHPUnit\tUNKNOWN STEP\t2026-08-10T08:34:58.61Z 1) c::wob\n\
         Run Tests / PHPUnit\tUNKNOWN STEP\t2026-08-10T08:34:58.61Z Failed asserting that false is true.\n\
         Run Tests / PHPUnit\tUNKNOWN STEP\t2026-08-10T08:34:58.61Z \n\
         Run Tests / PHPUnit\tUNKNOWN STEP\t2026-08-10T08:34:58.62Z \u{1b}[37;41mFAILURES!\u{1b}[0m\n\
         Run Tests / PHPUnit\tUNKNOWN STEP\t2026-08-10T08:34:58.62Z Tests: 405, Assertions: 1474, Failures: 1.\n",
    )
    .expect("write log");

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .env_remove("CI")
        .args([
            "import", "--log", "phpunit", "--env", "ci", "--commit", &sha, "--color", "never",
            "ci.log",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "import judges nothing");
    assert!(stdout.contains("ci.log: 1 observation"), "got: {stdout:?}");
    // Local greens + the log's witnessed failure on one clean commit: the
    // same proof a JUnit artifact carries, mined from what CI already keeps.
    assert!(stdout.contains("every failure in ci"), "got: {stdout:?}");
    assert!(
        stdout.contains("history now holds 3 observations"),
        "got: {stdout:?}"
    );
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_green_log_records_nothing_and_a_foreign_log_is_refused() {
    let (cwd, mut command) = sooth_in("log-green");
    std::fs::write(cwd.join("green.log"), "....\nOK (4 tests, 8 assertions)\n").expect("write");

    let output = command
        .args(["import", "--log", "phpunit", "--env", "ci", "green.log"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout.contains("green.log: no failures to record — a green log names no tests"),
        "got: {stdout:?}"
    );

    let (cwd_bad, mut command) = sooth_in("log-foreign");
    std::fs::write(cwd_bad.join("pytest.log"), "collected 3 items\n3 passed\n").expect("write");
    let output = command
        .args(["import", "--log", "phpunit", "--env", "ci", "pytest.log"])
        .output()
        .expect("sooth should run");
    assert_eq!(
        output.status.code(),
        Some(2),
        "sooth guessed at a foreign log"
    );
    assert!(
        !cwd_bad.join(".sooth/history.jsonl").exists(),
        "a refused log still wrote history"
    );
    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&cwd_bad);
}

#[test]
fn sooth_history_reads_the_evidence_without_touching_it() {
    let (cwd, mut command) = sooth_in("history-view");
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    std::fs::write(
        cwd.join(".sooth/history.jsonl"),
        concat!(
            r#"{"at":1,"commit":"aaa","dirty":false,"env":"local","status":"passed","id":"c::wob"}"#,
            "\n",
            r#"{"at":2,"commit":"aaa","dirty":false,"env":"ci","status":"failed","id":"c::wob"}"#,
            "\n",
            r#"{"at":3,"commit":"aaa","dirty":false,"env":"ci","status":"passed","id":"c::wob"}"#,
            "\n",
        ),
    )
    .expect("write");
    let before = std::fs::read_to_string(cwd.join(".sooth/history.jsonl")).expect("read");

    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("flaky per history"), "got: {stdout:?}");
    assert!(stdout.contains("every failure in ci"), "got: {stdout:?}");
    assert!(
        stdout.contains("history now holds 3 observations"),
        "got: {stdout:?}"
    );
    let after = std::fs::read_to_string(cwd.join(".sooth/history.jsonl")).expect("read");
    assert_eq!(before, after, "a look at the history changed it");
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn a_regression_prints_its_mark_and_the_commit_it_started_at() {
    let (cwd, mut command) = sooth_in("history-failing-since");
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    // Passed on commit aaa, then a two-red trailing streak on bbb…: the
    // shape that anchors a failing-since pointer (and never reads as flaky —
    // no clean commit saw both outcomes).
    std::fs::write(
        cwd.join(".sooth/history.jsonl"),
        concat!(
            r#"{"at":1,"commit":"aaa","dirty":false,"env":"local","status":"passed","id":"c::reg"}"#,
            "\n",
            r#"{"at":2,"commit":"bbb1234def","dirty":false,"env":"local","status":"failed","id":"c::reg"}"#,
            "\n",
            r#"{"at":3,"commit":"bbb1234def","dirty":false,"env":"local","status":"failed","id":"c::reg"}"#,
            "\n",
        ),
    )
    .expect("write");

    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout.contains("failing since a commit boundary:"),
        "got: {stdout:?}"
    );
    // The mark, the git-style short commit, and the streak length are the
    // whole regression line — each mutable without this pin.
    assert!(
        stdout.contains("▼ c::reg (since bbb1234, failed the last 2 observed runs)"),
        "got: {stdout:?}"
    );
    assert!(
        !stdout.contains("flaky per history"),
        "an empty flaky section must stay silent: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn an_unlisted_known_flake_under_fail_on_flaky_explains_the_gap() {
    let Some(dir) = scratch_repo("pardon-gap") else {
        return;
    };
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-pardon-gap-{}.xml",
        std::process::id()
    ));
    let run = |cases: &str, flags: &[&str]| {
        let script = format!(
            "printf '<testsuite>{cases}</testsuite>' > '{}'",
            report.display()
        );
        let report_path = report.display().to_string();
        let mut args = vec!["run"];
        args.extend_from_slice(flags);
        args.extend_from_slice(&[
            "--junit",
            &report_path,
            "--color",
            "never",
            "--",
            "sh",
            "-c",
            &script,
        ]);
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args(&args)
            .output()
            .expect("sooth should run")
    };

    // Green then red on one clean commit: c::wob becomes a proven flake.
    run(r#"<testcase classname="c" name="wob"/>"#, &[]);
    run(
        r#"<testcase classname="c" name="wob"><failure/></testcase>"#,
        &[],
    );
    // Red again under --fail-on-flaky, with no quarantine file: nothing new
    // failed, yet nothing was pardoned — the note must explain that gap.
    let output = run(
        r#"<testcase classname="c" name="wob"><failure/></testcase>"#,
        &["--fail-on-flaky"],
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(
        output.status.code(),
        Some(1),
        "the list alone pardons; sooth's own evidence never does: {stdout:?}"
    );
    assert!(
        stdout.contains("all known flakes, nothing new"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains(
            "note: not every failure above is in .sooth-quarantine, so --fail-on-flaky \
             pardoned nothing — add the ids to pardon them"
        ),
        "\"nothing new\" plus exit 1 without this note reads as a contradiction: {stdout:?}"
    );
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_history_says_so_instead_of_printing_nothing() {
    let (cwd, mut command) = sooth_in("history-empty");
    let _ = std::fs::remove_dir_all(cwd.join(".sooth"));
    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("the history is empty"), "got: {stdout:?}");
    let _ = std::fs::remove_dir_all(&cwd);
}

#[test]
fn the_gate_needs_a_preset_and_a_real_runs_count() {
    let (cwd, mut command) = sooth_in("gate-reject");
    let output = command
        .args(["run", "--changed", "--runs", "5", "--", "true"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("needs `--preset"), "got: {stderr:?}");

    let (cwd_single, mut command) = sooth_in("gate-one-run");
    let output = command
        .args(["run", "--changed", "--preset", "phpunit", "--", "true"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("20 is a good start"), "got: {stderr:?}");
    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&cwd_single);
}

#[test]
fn a_gate_with_nothing_changed_proves_nothing_and_spawns_nothing() {
    let Some(dir) = scratch_repo("gate-clean") else {
        return;
    };
    // The wrapped command is `false`: if the gate spawned it anyway, the
    // run would fail and so would this test.
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "5",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "false",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert!(stdout.contains("nothing to prove"), "got: {stdout:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gate_skips_a_deleted_test_file_instead_of_handing_it_to_the_runner() {
    let Some(dir) = scratch_repo("gate-deleted") else {
        return;
    };
    std::fs::write(dir.join("GoneTest.php"), "<?php\n").expect("write");
    git_in(&dir, &["add", "."]);
    git_in(&dir, &["commit", "-q", "-m", "add test"]);
    std::fs::remove_file(dir.join("GoneTest.php")).expect("remove");

    // The wrapped command is `false`: if the gate handed the deleted path to
    // a runner anyway, the run would fail and so would this test.
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "5",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "false",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        output.status.code(),
        Some(0),
        "a deleted test selects nothing: {stdout:?}"
    );
    assert!(stdout.contains("nothing to prove"), "got: {stdout:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gate_selects_a_test_file_with_a_non_ascii_name() {
    let Some(dir) = scratch_repo("gate-quotepath") else {
        return;
    };
    write_green_runner(&dir);
    // git's default core.quotepath would C-quote this name; quoted, it would
    // silently drop out of the selection — a false green.
    std::fs::write(dir.join("CaféTest.php"), "<?php\n").expect("write");

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "2",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert!(
        stdout.contains("gate: 1 changed test file against HEAD"),
        "got: {stdout:?}"
    );
    assert!(stdout.contains("CaféTest.php"), "got: {stdout:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gate_run_from_a_subdirectory_selects_paths_the_runner_can_open() {
    let Some(dir) = scratch_repo("gate-subdir") else {
        return;
    };
    let backend = dir.join("backend");
    std::fs::create_dir_all(&backend).expect("mkdir");
    std::fs::write(backend.join("SubTest.php"), "<?php committed\n").expect("write");
    git_in(&dir, &["add", "."]);
    git_in(&dir, &["commit", "-q", "-m", "add test"]);
    std::fs::write(backend.join("SubTest.php"), "<?php changed\n").expect("write");
    write_green_runner(&backend);

    // The runner is spawned in backend/, so the selection must be relative
    // to backend/ — `backend/SubTest.php` would not exist there.
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&backend)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "2",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert!(
        stdout.contains("gate: 1 changed test file against HEAD"),
        "got: {stdout:?}"
    );
    assert!(stdout.contains("SubTest.php"), "got: {stdout:?}");
    assert!(
        !stdout.contains("backend/SubTest.php"),
        "the selection must be cwd-relative: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_gate_under_bare_json_still_emits_the_one_json_line() {
    let Some(dir) = scratch_repo("gate-empty-json") else {
        return;
    };
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "5",
            "--preset",
            "phpunit",
            "--json",
            "--color",
            "never",
            "--",
            "false",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "bare --json must print exactly one line, gate or no gate: {stdout:?}"
    );
    assert!(lines[0].starts_with(r#"{"schema_version":1,"#));
    assert!(lines[0].contains(r#""runs":[]"#), "got: {stdout:?}");
    assert!(
        lines[0].contains(r#""gate":{"base":"HEAD","files":[]}"#),
        "got: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_gate_with_json_to_a_file_still_writes_the_document() {
    let Some(dir) = scratch_repo("gate-empty-json-file") else {
        return;
    };
    let json_path = dir.join("out.json");
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "5",
            "--preset",
            "phpunit",
            &format!("--json={}", json_path.display()),
            "--color",
            "never",
            "--",
            "false",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert!(stdout.contains("nothing to prove"), "got: {stdout:?}");
    let written = std::fs::read_to_string(&json_path)
        .expect("the JSON file must be written even when the gate is empty");
    assert!(written.starts_with(r#"{"schema_version":1,"#));
    assert!(written.contains(r#""runs":[]"#), "got: {written:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_gated_run_under_bare_json_keeps_the_one_line_contract_and_carries_the_gate() {
    let Some(dir) = scratch_repo("gate-bare-json") else {
        return;
    };
    write_green_runner(&dir);
    std::fs::write(dir.join("WobTest.php"), "<?php\n").expect("write");

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "2",
            "--preset",
            "phpunit",
            "--json",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "the gate's human lines must not break bare --json: {stdout:?}"
    );
    assert!(
        lines[0].contains(r#""gate":{"base":"HEAD","files":["WobTest.php"]}"#),
        "got: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Copy the fake-phpunit gate fixture (`tests/fixtures/gate_runner.sh`)
/// into `dir`. Its `--version` banner comes from the
/// `SOOTH_TEST_PHPUNIT_VERSION` environment variable; a real run leaves a
/// `ran-tests` marker.
fn write_gate_runner(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gate_runner.sh");
    let runner = dir.join("runner.sh");
    std::fs::copy(fixture, &runner).expect("copy runner");
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A gate invocation of the fixture runner in `dir`, with two new test
/// files and the given `--version` banner.
fn gate_two_files_under(dir: &std::path::Path, banner: &str) -> std::process::Output {
    write_gate_runner(dir);
    std::fs::write(dir.join("ATest.php"), "<?php\n").expect("write");
    std::fs::write(dir.join("BTest.php"), "<?php\n").expect("write");
    Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(dir)
        .env("SOOTH_TEST_PHPUNIT_VERSION", banner)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "2",
            "--no-history",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run")
}

#[test]
fn the_gate_refuses_a_multi_file_selection_on_phpunit_before_ten() {
    let Some(dir) = scratch_repo("gate-old-phpunit") else {
        return;
    };
    let output = gate_two_files_under(
        &dir,
        "PHPUnit 9.6.11 by Sebastian Bergmann and contributors.",
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(
        output.status.code(),
        Some(2),
        "a gate that would silently prove nothing must refuse: {stderr:?}"
    );
    assert!(
        stderr.contains("PHPUnit 9") && stderr.contains("first path"),
        "got: {stderr:?}"
    );
    assert!(
        !dir.join("ran-tests").exists(),
        "nothing may run once the gate knows it cannot gate everything"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gate_gates_several_files_on_phpunit_ten_and_later() {
    let Some(dir) = scratch_repo("gate-new-phpunit") else {
        return;
    };
    let output = gate_two_files_under(
        &dir,
        "PHPUnit 13.2.4 by Sebastian Bergmann and contributors.",
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert!(
        stdout.contains("gate: 2 changed test files against HEAD"),
        "got: {stdout:?}"
    );
    assert!(dir.join("ran-tests").exists(), "the gate must have run");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_phpunit_version_warns_but_does_not_block_the_gate() {
    let Some(dir) = scratch_repo("gate-foreign-version") else {
        return;
    };
    let output = gate_two_files_under(&dir, "Pest 2.34.1");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stderr:?}");
    assert!(
        stderr.contains("could not read a PHPUnit version"),
        "not knowing must be said out loud: {stderr:?}"
    );
    assert!(dir.join("ran-tests").exists(), "the gate must have run");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_single_file_gate_on_old_phpunit_proceeds_without_a_word() {
    let Some(dir) = scratch_repo("gate-old-single") else {
        return;
    };
    write_gate_runner(&dir);
    std::fs::write(dir.join("OnlyTest.php"), "<?php\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .env(
            "SOOTH_TEST_PHPUNIT_VERSION",
            "PHPUnit 9.6.11 by Sebastian Bergmann and contributors.",
        )
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "2",
            "--no-history",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stderr:?}");
    assert!(
        !stderr.contains("PHPUnit"),
        "one path is exactly what old PHPUnit handles — no noise: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gate_repeats_only_the_changed_tests_and_catches_a_born_flake() {
    let Some(dir) = scratch_repo("gate-e2e") else {
        return;
    };
    // A brand-new test file: exactly what the gate exists to interrogate.
    std::fs::write(dir.join("WobTest.php"), "<?php // new test\n").expect("write");
    // The fake runner echoes its selection into the report and flips the
    // outcome per run: a flake being born.
    let runner = dir.join("runner.sh");
    std::fs::write(
        &runner,
        concat!(
            "#!/bin/sh\n",
            "report=\"\"; prev=\"\"; files=\"\"\n",
            "for a in \"$@\"; do\n",
            "  if [ \"$prev\" = \"--log-junit\" ]; then report=\"$a\"; fi\n",
            "  case \"$a\" in *Test.php) files=\"$files$a\";; esac\n",
            "  prev=\"$a\"\n",
            "done\n",
            "n=$(cat gate-count 2>/dev/null || echo 0); n=$((n+1)); echo $n > gate-count\n",
            "status=\"\"\n",
            "if [ $((n % 2)) -eq 0 ]; then status=\"<failure/>\"; fi\n",
            "printf '<testsuite><testcase classname=\"gate\" name=\"%s\">%s</testcase></testsuite>' \"$files\" \"$status\" > \"$report\"\n",
        ),
    )
    .expect("write runner");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--changed=HEAD",
            "--runs",
            "4",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./runner.sh",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a flaky gate must fail: {stdout:?}"
    );
    assert!(
        stdout.contains("gate: 1 changed test file against HEAD"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("WobTest.php"),
        "the selection must name the file: {stdout:?}"
    );
    assert!(
        stdout.contains("flaky (2 of 4 runs now)"),
        "got: {stdout:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_history_neither_writes_nor_reports() {
    let Some(dir) = scratch_repo("nohistory") else {
        return;
    };
    let report = std::env::temp_dir().join(format!(
        "sooth-contract-nohistory-report-{}.xml",
        std::process::id()
    ));
    let script = format!(
        "printf '<testsuite><testcase classname=\"c\" name=\"t\"/></testsuite>' > '{}'",
        report.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--no-history",
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
    assert_eq!(output.status.code(), Some(0));
    assert!(
        !dir.join(".sooth").exists(),
        "--no-history still wrote a history"
    );
    let _ = std::fs::remove_file(&report);
    let _ = std::fs::remove_dir_all(&dir);
}
