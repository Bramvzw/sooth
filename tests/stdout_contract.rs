//! What `sooth run` puts on stdout is a public contract (see the `--json`
//! entry in `DECISIONS.md`), so it is pinned against the real binary.
#![expect(
    clippy::disallowed_methods,
    reason = "A contract test drives the real binary and git."
)]
#![cfg(unix)] // the wrapped commands are `true` and `sh`, which are Unix-only

use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory holding everything one test writes, removed when the
/// guard drops. Drop also runs while a failed assertion unwinds, which a
/// cleanup line at the end of a test body never reaches.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("sooth-contract-{tag}-{}", std::process::id()));
        // A previous run of this same test (same pid is possible after a
        // crash) must not hand its leftovers to this one.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir should be creatable");
        Scratch(path)
    }
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for Scratch {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A sooth invocation in its own scratch cwd: history writes `.sooth/` into
/// the working directory and a `.sooth-quarantine` is read from it, so the
/// suite must never run in the repo's own tree or in a cwd another test owns.
fn sooth_in(tag: &str) -> (Scratch, Command) {
    let cwd = Scratch::new(tag);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sooth"));
    command.current_dir(&cwd);
    (cwd, command)
}

fn fixture() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/pytest_testsuites.xml"
    )
}

/// A report path in `dir` and the command that fills it. The wrapped command
/// copies the fixture into place *during the run*, because a `--junit` file
/// that predates the run is rejected as stale.
fn fresh_report(dir: &Path) -> (PathBuf, String) {
    let path = dir.join("report.xml");
    let write_during_run = format!("cp '{}' '{}'", fixture(), path.display());
    (path, write_during_run)
}

#[test]
fn a_scratch_dir_is_gone_once_its_guard_drops() {
    let path = {
        let scratch = Scratch::new("guard");
        std::fs::write(scratch.join("leftover.txt"), "x").expect("write");
        scratch.to_path_buf()
    };
    assert!(
        !path.exists(),
        "the guard must take its directory with it: {path:?}"
    );
}

#[test]
fn bare_json_prints_exactly_one_stdout_line_of_json() {
    let (cwd, mut command) = sooth_in("bare-json");
    let (report, write_report) = fresh_report(&cwd);
    let output = command
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
    let (_cwd, mut command) = sooth_in("plain-run");
    let output = command
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

/// A fake runner called `name` in `dir`: it writes a one-test report to the
/// path its `flag` carries (`--flag PATH` or `--flag=PATH`), leaves a
/// `ran-with` note of its arguments, and refuses any argument in `refused`.
fn write_named_runner(dir: &Path, name: &str, flag: &str, refused: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' \"$@\" > ran-with\n",
            "report=\"\"; prev=\"\"\n",
            "for a in \"$@\"; do\n",
            "  case \"$a\" in {refused}) echo \"unknown option $a\" >&2; exit 64 ;; esac\n",
            "  case \"$a\" in {flag}=*) report=\"${{a#{flag}=}}\" ;; esac\n",
            "  if [ \"$prev\" = \"{flag}\" ]; then report=\"$a\"; fi\n",
            "  prev=\"$a\"\n",
            "done\n",
            "[ -n \"$report\" ] || exit 0\n",
            "printf '<testsuite><testcase classname=\"a\" name=\"ok\"/></testsuite>' > \"$report\"\n",
        ),
        flag = flag,
        refused = refused
    );
    std::fs::write(dir.join(name), script).expect("write runner");
    std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
}

/// `sooth run --no-history --color never <args>` in `dir`: (exit, stdout, stderr).
fn first_command(dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(dir)
        .args(["run", "--no-history", "--color", "never"])
        .args(args)
        .output()
        .expect("sooth should run");
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout should be UTF-8"),
        String::from_utf8(output.stderr).expect("stderr should be UTF-8"),
    )
}

#[test]
fn first_command_a_runner_that_names_itself_needs_no_preset() {
    let cwd = Scratch::new("first-inferred");
    write_named_runner(&cwd, "pytest", "--junit-xml", "--none");
    let (exit, stdout, stderr) = first_command(&cwd, &["--", "./pytest"]);

    assert_eq!(exit, Some(0), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("tests: 1 total"),
        "the report was read: {stdout:?}"
    );
    assert!(
        stderr.contains("`--preset pytest`") && stderr.contains("`./pytest`"),
        "the choice is said out loud: {stderr:?}"
    );

    // An explicit choice is nobody's inference.
    let (_, _, stderr) = first_command(&cwd, &["--preset", "pytest", "--", "./pytest"]);
    assert!(!stderr.contains("inferred"), "got: {stderr:?}");
}

#[test]
fn first_command_a_runner_php_runs_is_named_and_gets_its_flags_behind_the_script() {
    let cwd = Scratch::new("first-php-script");
    write_named_runner(&cwd, "phpunit", "--log-junit", "--none");
    // A PHP binary, as far as its command line goes: its own options, then
    // the script and what is the script's. An option it does not know
    // stops it, as `--log-junit` stops the real one.
    write_executable(
        &cwd,
        "php",
        concat!(
            "#!/bin/sh\n",
            "while [ $# -gt 0 ]; do\n",
            "  case \"$1\" in\n",
            "    -d) shift 2 ;;\n",
            "    -*) echo \"php: unknown option $1\" >&2; exit 1 ;;\n",
            "    *) break ;;\n",
            "  esac\n",
            "done\n",
            "exec \"$@\"\n",
        ),
    );
    let (exit, stdout, stderr) = first_command(
        &cwd,
        &[
            "--",
            "./php",
            "-d",
            "memory_limit=1G",
            "./phpunit",
            "--testdox",
        ],
    );

    assert_eq!(exit, Some(0), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("tests: 1 total"),
        "the report was read: {stdout:?}"
    );
    assert!(
        stderr.contains("`--preset phpunit`")
            && stderr.contains("`./php -d memory_limit=1G ./phpunit`"),
        "the choice is said out loud: {stderr:?}"
    );
    let ran_with = std::fs::read_to_string(cwd.join("ran-with")).expect("the runner ran");
    assert!(
        ran_with.starts_with("--log-junit\n") && ran_with.ends_with("--testdox\n"),
        "the runner got sooth's flags, then the user's: {ran_with:?}"
    );
}

#[test]
fn first_command_pest_is_a_name_for_the_phpunit_preset() {
    let cwd = Scratch::new("first-pest");
    write_named_runner(&cwd, "pest", "--log-junit", "--none");
    let (exit, stdout, stderr) = first_command(&cwd, &["--preset", "pest", "--", "./pest"]);

    assert_eq!(exit, Some(0), "got: {stdout:?} {stderr:?}");
    assert!(stdout.contains("tests: 1 total"), "got: {stdout:?}");
}

#[test]
fn first_command_without_a_report_source_names_the_flags_that_add_one() {
    let cwd = Scratch::new("first-plain");
    let (exit, stdout, stderr) = first_command(&cwd, &["--", "true"]);

    assert_eq!(exit, Some(0));
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("result: ✓ PASSED")),
        "stdout keeps its shape: {stdout:?}"
    );
    assert!(
        stderr.contains("--preset <RUNNER>") && stderr.contains("--junit <PATH>"),
        "got: {stderr:?}"
    );
}

#[test]
fn first_command_jest_is_named_but_never_assumed() {
    // The jest preset needs the jest-junit package: injecting its reporter
    // into a project without it would break a command that works.
    let cwd = Scratch::new("first-jest");
    write_named_runner(&cwd, "jest", "--never", "--reporters=*");
    let (exit, stdout, stderr) = first_command(&cwd, &["--", "./jest"]);

    assert_eq!(exit, Some(0), "got: {stdout:?} {stderr:?}");
    assert!(
        stderr.contains("`--preset jest`") && stderr.contains("jest-junit"),
        "got: {stderr:?}"
    );
}

#[test]
fn first_command_that_writes_its_own_report_is_left_as_typed() {
    // A second `--log-junit` would win over sooth's: the run would end
    // without the report sooth asked for.
    let cwd = Scratch::new("first-own-report");
    write_named_runner(&cwd, "phpunit", "--log-junit", "--none");
    let (exit, stdout, stderr) =
        first_command(&cwd, &["--", "./phpunit", "--log-junit", "build.xml"]);

    assert_eq!(exit, Some(0), "got: {stdout:?} {stderr:?}");
    let ran_with = std::fs::read_to_string(cwd.join("ran-with")).expect("the runner ran");
    assert_eq!(ran_with, "--log-junit\nbuild.xml\n", "nothing was injected");
    assert!(
        stderr.contains("--junit build.xml"),
        "the report it writes is one flag away: {stderr:?}"
    );
}

#[test]
fn first_command_with_a_report_asked_for_in_the_environment_is_left_as_typed() {
    // pytest reads more arguments from PYTEST_ADDOPTS: sooth's own flag
    // would override the report the user asked for there.
    let cwd = Scratch::new("first-env-report");
    write_named_runner(&cwd, "pytest", "--never", "--junit-xml=*");
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&cwd)
        .env("PYTEST_ADDOPTS", "-q --junitxml=ci.xml")
        .args(["run", "--no-history", "--color", "never", "--", "./pytest"])
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stderr:?}");
    assert!(!stderr.contains("inferred"), "got: {stderr:?}");
    assert!(stderr.contains("`--junit ci.xml`"), "got: {stderr:?}");
}

#[test]
fn first_command_behind_a_container_wrapper_is_left_as_typed() {
    // `sail artisan test` runs in a container: a report path on this
    // machine means nothing there, so nothing is inferred or injected.
    let cwd = Scratch::new("first-sail");
    write_named_runner(&cwd, "sail", "--never", "--log-junit");
    let (exit, stdout, stderr) = first_command(&cwd, &["--", "./sail", "artisan", "test"]);

    assert_eq!(exit, Some(0), "got: {stdout:?} {stderr:?}");
    assert!(!stderr.contains("inferred"), "got: {stderr:?}");
    let ran_with = std::fs::read_to_string(cwd.join("ran-with")).expect("the wrapper ran");
    assert_eq!(ran_with, "artisan\ntest\n", "nothing was injected");
}

#[test]
fn first_command_the_gate_and_json_work_on_an_inferred_preset() {
    let Some(dir) = scratch_repo("first-gate") else {
        return;
    };
    write_named_runner(&dir, "pytest", "--junit-xml", "--none");
    std::fs::write(dir.join("test_a.py"), "\n").expect("write");
    let (exit, stdout, stderr) = first_command(
        &dir,
        &["--changed=HEAD", "--runs", "2", "--json", "--", "./pytest"],
    );

    assert_eq!(exit, Some(0), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.lines().count() == 1 && stdout.starts_with('{'),
        "bare --json still owns stdout: {stdout:?}"
    );
    assert!(stdout.contains("\"gate\""), "got: {stdout:?}");
}

/// `sooth run --color never <args> -- sh -c <script>` in a scratch dir. The
/// script gets the number of its invocation as `$n` (1, 2, …).
fn repeat_script(tag: &str, args: &[&str], script: &str) -> std::process::Output {
    let (cwd, mut command) = sooth_in(tag);
    let counted = format!(
        "n=$(( $(cat '{c}' 2>/dev/null || echo 0) + 1 )); echo $n > '{c}'; {script}",
        c = cwd.join("invocations").display()
    );
    command
        .args(["run", "--color", "never"])
        .args(args)
        .args(["--", "sh", "-c", &counted])
        .output()
        .expect("sooth should run")
}

#[test]
fn repeated_runs_keep_the_runner_quiet_and_say_one_line_each() {
    let output = repeat_script(
        "quiet-green",
        &["--runs", "3"],
        "echo NOISE; echo ERRNOISE >&2",
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert!(
        !stdout.contains("NOISE") && !stderr.contains("NOISE"),
        "twenty copies of a green run say nothing: {stdout:?} {stderr:?}"
    );
    for run in 1..=3 {
        assert_eq!(
            stdout
                .matches(&format!("run {run}/3: runner exit=0"))
                .count(),
            1,
            "one line per run: {stdout:?}"
        );
    }
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("result: ✓ PASSED")),
        "got: {stdout:?}"
    );
}

#[test]
fn the_first_failing_run_shows_its_output_once() {
    let output = repeat_script(
        "quiet-red",
        &["--runs", "4"],
        "echo \"OUT $n\"; echo \"ERR $n\" >&2; [ $n -lt 2 ]",
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains("output of run 2/4, the first that failed:\nOUT 2\nERR 2\n"),
        "both streams of the failing run, in order: {stdout:?}"
    );
    for hidden in ["OUT 1", "OUT 3", "OUT 4", "ERR 3"] {
        assert!(
            !stdout.contains(hidden),
            "`{hidden}` must stay hidden: {stdout:?}"
        );
    }
    // The block sits between the run lines and the verdict.
    let block = stdout.find("output of run 2/4").expect("the block");
    assert!(
        stdout.find("run 4/4:").is_some_and(|line| line < block),
        "got: {stdout:?}"
    );
    assert!(
        stdout.find("result: ").is_some_and(|line| line > block),
        "got: {stdout:?}"
    );
}

#[test]
fn show_output_lets_every_run_speak_as_before() {
    let output = repeat_script("quiet-off", &["--runs", "3", "--show-output"], "echo NOISE");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(stdout.matches("NOISE").count(), 3, "got: {stdout:?}");
    assert!(!stdout.contains("output of run"), "got: {stdout:?}");
    assert!(stdout.contains("run 3/3: runner exit=0"), "got: {stdout:?}");
}

#[test]
fn a_single_run_shows_the_tests_as_if_you_ran_them() {
    let output = repeat_script("quiet-single", &[], "echo NOISE");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(stdout.matches("NOISE").count(), 1, "got: {stdout:?}");
}

#[test]
fn a_quiet_run_that_wrote_no_report_still_shows_why() {
    // "The runner's own output above likely explains…" has to stay true.
    let (cwd, mut command) = sooth_in("quiet-no-report");
    write_named_runner(&cwd, "pytest", "--never", "--none");
    std::fs::write(
        cwd.join("pytest"),
        "#!/bin/sh\necho 'usage: pytest: unrecognized arguments'\nexit 4\n",
    )
    .expect("write");
    let output = command
        .args([
            "run",
            "--no-history",
            "--color",
            "never",
            "--runs",
            "3",
            "--",
            "./pytest",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("unrecognized arguments"),
        "the hidden output is the explanation: {stdout:?}"
    );
    assert!(
        stderr.contains("wrote no JUnit-XML report"),
        "got: {stderr:?}"
    );
}

#[test]
fn quiet_repeats_under_bare_json_leave_stdout_to_the_document() {
    // The fixture report has a failing test, so the first run's output is
    // replayed — on stderr, because bare --json owns stdout.
    let (cwd, mut command) = sooth_in("quiet-json");
    let (report, write_report) = fresh_report(&cwd);
    let output = command
        .args([
            "run",
            "--no-history",
            "--color",
            "never",
            "--runs",
            "2",
            "--junit",
            &report.display().to_string(),
            "--json",
            "--",
            "sh",
            "-c",
            &format!("echo NOISE; {write_report}"),
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert!(
        stdout.lines().count() == 1 && stdout.starts_with("{\"schema_version\""),
        "one line, the document: {stdout:?}"
    );
    assert_eq!(stderr.matches("NOISE").count(), 1, "got: {stderr:?}");
}

#[test]
fn a_quiet_run_that_left_the_users_report_untouched_is_still_shown() {
    // Run 2 dies before it writes the report: sooth refuses the stale file,
    // and what run 2 printed is the only explanation there is.
    let (cwd, mut command) = sooth_in("quiet-stale");
    let (report, write_report) = fresh_report(&cwd);
    let counted = format!(
        "n=$(( $(cat '{c}' 2>/dev/null || echo 0) + 1 )); echo $n > '{c}'; \
         echo \"OUT $n\"; [ $n -eq 1 ] || {{ echo \"CRASH $n\" >&2; exit 3; }}; {write_report}",
        c = cwd.join("invocations").display()
    );
    let output = command
        .args([
            "run",
            "--no-history",
            "--color",
            "never",
            "--runs",
            "3",
            "--junit",
            &report.display().to_string(),
            "--",
            "sh",
            "-c",
            &counted,
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stderr.contains("was not touched by this run"),
        "got: {stderr:?}"
    );
    assert!(
        stdout.contains("run 2/3: runner exit=3") && stdout.contains("OUT 2\nCRASH 2\n"),
        "the run that broke, and what it said: {stdout:?}"
    );
}

#[test]
fn an_earlier_failing_run_is_still_shown_when_a_later_one_stops_sooth() {
    let (cwd, mut command) = sooth_in("quiet-abort-after-failure");
    // Run 1 fails and reports it; run 2 writes no report at all.
    std::fs::write(
        cwd.join("pytest"),
        concat!(
            "#!/bin/sh\n",
            "n=$(( $(cat invocations 2>/dev/null || echo 0) + 1 )); echo $n > invocations\n",
            "[ $n -eq 1 ] || { echo 'NOREPORT 2'; exit 4; }\n",
            "for a in \"$@\"; do case \"$a\" in --junit-xml=*) report=\"${a#--junit-xml=}\" ;; esac; done\n",
            "echo 'FAILED 1'\n",
            "printf '<testsuite><testcase classname=\"c\" name=\"t\"><failure/></testcase></testsuite>' > \"$report\"\n",
            "exit 1\n",
        ),
    )
    .expect("write");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(cwd.join("pytest"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    let output = command
        .args([
            "run",
            "--no-history",
            "--color",
            "never",
            "--runs",
            "3",
            "--",
            "./pytest",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("output of run 1/3, the first that failed:\nFAILED 1\n"),
        "run 1's failure is not lost with the abort: {stdout:?}"
    );
    assert!(stdout.contains("NOREPORT 2"), "got: {stdout:?}");
    assert!(
        stderr.contains("wrote no JUnit-XML report"),
        "got: {stderr:?}"
    );
}

#[test]
fn an_earlier_failing_run_is_still_shown_when_the_next_ones_output_has_nowhere_to_go() {
    use std::os::unix::fs::PermissionsExt;
    let (cwd, mut command) = sooth_in("quiet-abort-no-capture");
    let tmp = cwd.join("tmp");
    std::fs::create_dir_all(&tmp).expect("mkdir");
    // Run 1 fails. Run 2 takes away the place the runs' output is kept: run
    // 3 cannot start, and sooth stops there.
    let script = concat!(
        "n=$(( $(cat invocations 2>/dev/null || echo 0) + 1 )); echo $n > invocations; ",
        "[ $n -ne 1 ] || { echo 'FAILED 1'; exit 1; }; ",
        "rm -f \"$TMPDIR\"/sooth-*/run.log; chmod 500 \"$TMPDIR\"/sooth-*",
    );
    let output = command
        .env("TMPDIR", &tmp)
        .args(["run", "--color", "never", "--runs", "3", "--", "sh", "-c"])
        .arg(script)
        .output()
        .expect("sooth should run");
    // The directory sooth could not empty: writable again, so the scratch
    // directory can be removed.
    for entry in std::fs::read_dir(&tmp).expect("read tmp") {
        let _ = std::fs::set_permissions(
            entry.expect("entry").path(),
            std::fs::Permissions::from_mode(0o700),
        );
    }
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stderr.contains("failed to create a file for the run's output"),
        "got: {stderr:?}"
    );
    assert!(
        stdout.contains("output of run 1/3, the first that failed:\nFAILED 1\n"),
        "run 1's failure is not lost with the abort: {stdout:?}"
    );
}

/// `sooth run --color never --junit report.xml <args> -- cp src.xml
/// report.xml` in a scratch dir, with `xml` as the report: its stdout.
fn stdout_for_report(tag: &str, xml: &str, args: &[&str]) -> String {
    let (cwd, mut command) = sooth_in(tag);
    std::fs::write(cwd.join("src.xml"), xml).expect("write");
    let output = command
        .args(["run", "--color", "never", "--junit", "report.xml"])
        .args(args)
        .args(["--", "cp", "src.xml", "report.xml"])
        .output()
        .expect("sooth should run");
    String::from_utf8(output.stdout).expect("stdout should be UTF-8")
}

#[test]
fn an_unread_history_is_not_quoted() {
    // `--no-history` reads no history: "new (nothing in history)" would be
    // a claim about a file nobody opened.
    let stdout = stdout_for_report(
        "unread-history",
        r#"<testsuite><testcase classname="c" name="wob"><failure/></testcase></testsuite>"#,
        &["--no-history"],
    );
    assert!(
        stdout.contains("\n1 failure\n  ✗ c::wob\nnote: the run history was not consulted"),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("in history)"), "got: {stdout:?}");
    assert!(!stdout.contains("new"), "got: {stdout:?}");
}

#[test]
fn totals_of_several_runs_say_which_run_they_count() {
    let report = r#"<testsuite><testcase classname="c" name="ok"/></testsuite>"#;
    let repeated = stdout_for_report("totals-final", report, &["--no-history", "--runs", "2"]);
    assert!(
        repeated.contains("tests (final run): 1 total"),
        "got: {repeated:?}"
    );
    let single = stdout_for_report("totals-single", report, &["--no-history"]);
    assert!(single.contains("\ntests: 1 total"), "got: {single:?}");
}

#[test]
fn the_slowest_ranking_lists_measurements_and_only_when_it_ranks() {
    let cases = |timed: usize, untimed: usize| {
        let timed = (0..timed).map(|index| {
            format!(
                r#"<testcase classname="c" name="timed{index}" time="0.{:03}"/>"#,
                index + 1
            )
        });
        let untimed = (0..untimed)
            .map(|index| format!(r#"<testcase classname="c" name="untimed{index}" time="0"/>"#));
        let body: Vec<String> = timed.chain(untimed).collect();
        format!("<testsuite>{}</testsuite>", body.concat())
    };

    // Three tests have no "slowest ten".
    let small = stdout_for_report("slowest-small", &cases(3, 0), &["--no-history"]);
    assert!(!small.contains("slowest tests:"), "got: {small:?}");
    // Asked for, the ranking is given.
    let asked = stdout_for_report(
        "slowest-asked",
        &cases(3, 0),
        &["--no-history", "--slowest", "2"],
    );
    assert!(
        asked.contains("slowest tests:\n  1. c::timed2 (3.00ms)\n  2. c::timed1 (2.00ms)\n"),
        "got: {asked:?}"
    );
    // A suite larger than the cut-off gets it unasked.
    let large = stdout_for_report("slowest-large", &cases(12, 0), &["--no-history"]);
    assert!(
        large.contains("slowest tests:\n  1. c::timed11 (12.00ms)"),
        "got: {large:?}"
    );
    // `time="0"` is not a measurement: it is never ranked, and a suite with
    // nothing measured has no ranking at all.
    let mixed = stdout_for_report(
        "slowest-mixed",
        &cases(1, 3),
        &["--no-history", "--slowest", "3"],
    );
    assert!(
        mixed.contains("slowest tests:\n  1. c::timed0 (1.00ms)\n") && !mixed.contains("0.00ns"),
        "got: {mixed:?}"
    );
    let unmeasured = stdout_for_report(
        "slowest-unmeasured",
        &cases(0, 12),
        &["--no-history", "--slowest", "3"],
    );
    assert!(
        !unmeasured.contains("slowest tests:"),
        "got: {unmeasured:?}"
    );
}

#[test]
fn a_usage_error_says_what_was_meant() {
    for (args, tip) in [
        (
            &["run", "php", "artisan", "test"][..],
            "tip: the test command goes after `--`: `sooth run -- php artisan test`",
        ),
        (
            &["run", "--json", "out.json", "--", "true"][..],
            "tip: `--json` takes its value with an `=`: `--json=out.json`",
        ),
    ] {
        let (_cwd, mut command) = sooth_in("usage-tip");
        let output = command.args(args).output().expect("sooth should run");
        let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

        assert_eq!(
            output.status.code(),
            Some(2),
            "a usage error is sooth's: {stderr:?}"
        );
        assert!(stderr.contains(tip), "got: {stderr:?}");
        assert!(output.stdout.is_empty(), "nothing ran");
    }
}

#[test]
fn an_argument_that_is_not_utf8_is_a_usage_error_not_a_crash() {
    use std::os::unix::ffi::OsStrExt;
    let (_cwd, mut command) = sooth_in("usage-not-utf8");
    let output = command
        .args(["run", "--junit"])
        .arg(OsStr::from_bytes(b"\xff"))
        .arg("pytest")
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert!(
        stderr.contains("error: ") && !stderr.contains("internal error"),
        "clap's own error, whatever it says about the bytes: {stderr:?}"
    );
}

#[test]
fn sooth_leaves_nothing_behind_in_the_temp_dir_however_it_ends() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let cwd = Scratch::new("no-leftovers");
    let tmp = cwd.join("tmp");
    std::fs::create_dir_all(&tmp).expect("mkdir");
    write_named_runner(&cwd, "pytest", "--junit-xml", "--none");
    // A runner that fails and writes no report: sooth stops with exit 2.
    std::fs::write(cwd.join("phpunit"), "#!/bin/sh\necho no report\nexit 3\n").expect("write");
    std::fs::set_permissions(cwd.join("phpunit"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    write_verify_runner(&cwd);
    let sooth = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sooth"));
        command
            .current_dir(&cwd)
            .env("TMPDIR", &tmp)
            .env(
                "SOOTH_TEST_VERIFY_CASE",
                r#"<testcase classname="c" name="wob"><failure/></testcase>"#,
            )
            .args(["run", "--no-history", "--color", "never"])
            .args(args);
        command
    };
    let leftovers = || -> Vec<String> {
        std::fs::read_dir(&tmp)
            .expect("read tmp")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };

    for (what, args, exit) in [
        ("a green preset run", &["--", "./pytest"][..], 0),
        ("quiet repeats", &["--runs", "3", "--", "./pytest"][..], 0),
        ("a run that wrote no report", &["--", "./phpunit"][..], 2),
        (
            "quiet repeats that stop at a run",
            &["--runs", "3", "--", "./phpunit"][..],
            2,
        ),
        (
            "a verified failure",
            &["--verify", "--preset", "phpunit", "--", "./runner.sh"][..],
            1,
        ),
    ] {
        let output = sooth(args).output().expect("sooth should run");
        assert_eq!(output.status.code(), Some(exit), "{what}");
        assert_eq!(leftovers(), Vec::<String>::new(), "{what}");
    }

    // A reader that goes away: sooth dies on its next line, and the panic
    // still unwinds through the guards.
    let mut child = sooth(&["--runs", "3", "--", "./pytest"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("sooth should start");
    drop(child.stdout.take());
    let status = child.wait().expect("wait");
    assert_eq!(status.code(), Some(2), "a closed pipe is sooth's failure");
    assert_eq!(leftovers(), Vec::<String>::new(), "a closed pipe");
}

#[test]
fn the_history_records_how_long_each_test_took() {
    // What a later slow-test pass reads: "slower since X" needs a before.
    let (cwd, mut command) = sooth_in("history-durations");
    std::fs::write(
        cwd.join("src.xml"),
        concat!(
            r#"<testsuite>"#,
            r#"<testcase classname="c" name="slow" time="1.840"/>"#,
            r#"<testcase classname="c" name="quick" time="0.000"/>"#,
            r#"<testcase classname="c" name="untimed"/>"#,
            r#"</testsuite>"#
        ),
    )
    .expect("write");
    let output = command
        .args(["run", "--color", "never", "--junit", "report.xml"])
        .args(["--", "cp", "src.xml", "report.xml"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(0));

    let history = std::fs::read_to_string(cwd.join(".sooth/history.jsonl")).expect("history");
    let line_of = |id: &str| {
        history
            .lines()
            .find(|line| line.ends_with(&format!(r#""id":"{id}"}}"#)))
            .unwrap_or_else(|| panic!("no line for {id}: {history}"))
    };
    assert!(line_of("c::slow").contains(r#""ms":1840,"#), "{history}");
    assert!(
        line_of("c::quick").contains(r#""ms":0,"#),
        "timed at nothing is still timed: {history}"
    );
    assert!(
        !line_of("c::untimed").contains(r#""ms":"#),
        "a time nobody measured is not recorded: {history}"
    );

    // An imported report brings its durations along.
    let import = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&cwd)
        .args(["import", "--color", "never", "--env", "ci", "src.xml"])
        .output()
        .expect("sooth should run");
    assert_eq!(import.status.code(), Some(0));
    let history = std::fs::read_to_string(cwd.join(".sooth/history.jsonl")).expect("history");
    assert!(
        history
            .lines()
            .any(|line| line.contains(r#""env":"ci""#) && line.contains(r#""ms":1840,"#)),
        "{history}"
    );
}

/// A sooth run of five in `cwd` whose third run hangs: runs 1 and 2 exit 0
/// at once, run 3 becomes `sleep 30`. Returns the child once run 3 has
/// started, with its stdout and stderr piped. `alone` puts sooth in a
/// process group of its own, so a signal can be sent to the whole group
/// the way a terminal's Ctrl-C is.
fn sooth_hanging_in_its_third_run(cwd: &Path, extra: &[&str]) -> std::process::Child {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    std::fs::write(
        cwd.join("runner"),
        concat!(
            "#!/bin/sh\n",
            "n=$(( $(cat invocations 2>/dev/null || echo 0) + 1 )); echo $n > invocations\n",
            "echo $$ > \"pid-$n\"\n",
            "echo \"speaking in run $n\"\n",
            "[ $n -lt 3 ] || exec sleep 30\n",
            "exit 0\n",
        ),
    )
    .expect("write runner");
    std::fs::set_permissions(cwd.join("runner"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    std::fs::create_dir_all(cwd.join("tmp")).expect("mkdir");
    let child = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(cwd)
        .env("TMPDIR", cwd.join("tmp"))
        .args(["run", "--color", "never", "--runs", "5"])
        .args(extra)
        .args(["--", "./runner"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("sooth should start");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !cwd.join("pid-3").exists() {
        assert!(std::time::Instant::now() < deadline, "run 3 never started");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child
}

/// `kill <signal> <target>`, asserting it was delivered.
fn send_signal(signal: &str, target: &str) {
    let sent = Command::new("kill")
        .args([signal, "--", target])
        .status()
        .expect("kill should run");
    assert!(sent.success(), "kill {signal} {target}");
}

/// Whether the process the runner's third invocation wrote down is alive.
fn third_run_is_alive(cwd: &Path) -> bool {
    let pid = std::fs::read_to_string(cwd.join("pid-3")).expect("pid-3");
    Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn asked_to_stop_alone_sooth_reports_what_finished_and_takes_the_runner_down() {
    // CI cancelling a job, a timeout wrapper, `kill`: only sooth gets the
    // signal. The runner knows nothing and would run on as an orphan.
    let cwd = Scratch::new("interrupt-alone");
    let child = sooth_hanging_in_its_third_run(&cwd, &[]);
    let asked = std::time::Instant::now();
    send_signal("-TERM", &child.id().to_string());
    let output = child.wait_with_output().expect("sooth should exit");
    let waited = asked.elapsed();
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("run 1/5: runner exit=0") && stdout.contains("run 2/5: runner exit=0"),
        "the runs that finished are reported: {stdout:?}"
    );
    assert!(
        !stdout.contains("run 3/5:"),
        "a run cut short is not a run: {stdout:?}"
    );
    assert!(
        stdout.contains("result: interrupted after 2 of 5 runs — none of them failed"),
        "no verdict, what happened: {stdout:?}"
    );
    assert!(
        stderr.contains("interrupted after 2 of 5 runs — the report covers the runs that finished"),
        "got: {stderr:?}"
    );
    // Quiet runs hide their output; for a run that hung, what it had
    // written is the only trace of where.
    assert!(
        stdout.contains("output of run 3/5, the one that was cut short:\nspeaking in run 3\n"),
        "got: {stdout:?}"
    );
    assert!(
        !stdout.contains("speaking in run 1"),
        "a run that finished and passed stays quiet: {stdout:?}"
    );
    assert!(
        waited >= std::time::Duration::from_secs(2) && waited < std::time::Duration::from_secs(8),
        "the runner got its moment, and no more: {waited:?}"
    );
    assert!(
        !third_run_is_alive(&cwd),
        "the runner must not outlive sooth"
    );
    let leftovers: Vec<_> = std::fs::read_dir(cwd.join("tmp")).expect("tmp").collect();
    assert!(
        leftovers.is_empty(),
        "nothing left in the temp dir: {leftovers:?}"
    );
}

#[test]
fn a_ctrl_c_ends_sooth_at_once_with_the_runs_that_finished() {
    // A terminal sends the signal to the whole foreground group: the
    // runner dies of it by itself, and sooth has nothing to wait for.
    let cwd = Scratch::new("interrupt-group");
    let child = sooth_hanging_in_its_third_run(&cwd, &[]);
    let asked = std::time::Instant::now();
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let waited = asked.elapsed();
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?}");
    assert!(
        stdout.contains("result: interrupted after 2 of 5 runs — none of them failed"),
        "got: {stdout:?}"
    );
    assert!(
        waited < std::time::Duration::from_millis(1500),
        "no grace to sit out when the runner is already gone: {waited:?}"
    );
    assert!(!third_run_is_alive(&cwd));
}

#[test]
fn an_interrupted_run_under_json_says_so_in_the_document() {
    let cwd = Scratch::new("interrupt-json");
    // A report source, so --json is allowed: runs 1 and 2 leave a report.
    let (report, write_report) = fresh_report(&cwd);
    let child = {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let script = format!(
            "n=$(( $(cat invocations 2>/dev/null || echo 0) + 1 )); echo $n > invocations; \
             echo $$ > pid-$n; {write_report}; [ $n -lt 3 ] || exec sleep 30"
        );
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&cwd)
            .args([
                "run",
                "--no-history",
                "--color",
                "never",
                "--runs",
                "5",
                "--json",
            ])
            .args(["--junit", &report.display().to_string()])
            .args(["--", "sh", "-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("sooth should start")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !cwd.join("pid-3").exists() {
        assert!(std::time::Instant::now() < deadline, "run 3 never started");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let document = stdout.lines().last().unwrap_or_default();

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?}");
    assert!(
        document
            .ends_with(r#""interrupted":{"during":"runs","finished_runs":2,"requested_runs":5}}"#),
        "got: {document:?}"
    );
    assert_eq!(
        document.matches(r#""exit_code""#).count(),
        2,
        "the document's runs are the two that finished: {document:?}"
    );
}

/// An executable script `name` in `dir`.
fn write_executable(dir: &Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(dir.join(name), script).expect("write script");
    std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
}

/// `sooth run --no-history --color never <args>` in `cwd`, in a process
/// group of its own, so a signal to the group reaches sooth and its runner
/// and not the test.
fn sooth_in_its_own_group(cwd: &Path, args: &[&str]) -> std::process::Child {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(cwd)
        .args(["run", "--no-history", "--color", "never"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("sooth should start")
}

/// Wait until the runner has written `file`: it is then inside that run.
fn wait_for(file: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !file.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            file.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The start of a fake pytest: counts its invocations in `$n` and finds
/// the report path sooth handed it in `$report`.
const FAKE_PYTEST: &str = concat!(
    "#!/bin/sh\n",
    "n=$(( $(cat invocations 2>/dev/null || echo 0) + 1 )); echo $n > invocations\n",
    "for a in \"$@\"; do case \"$a\" in --junit-xml=*) report=\"${a#--junit-xml=}\" ;; esac; done\n",
);

/// A report that sooth can only read after it was asked to stop: the
/// runner leaves a pipe where the report goes and is gone at once; a
/// process it left behind signals sooth — the runner's parent — and only
/// then writes `xml` into the pipe.
fn report_that_arrives_after_a_stop(xml: &str) -> String {
    format!(
        "mkfifo \"$report\"\n\
         (sleep 0.3; kill -INT $PPID; sleep 0.3; printf '%s' '{xml}' > \"$report\") \
         > /dev/null 2>&1 &\n"
    )
}

const ONE_FAILING_TEST: &str =
    r#"<testsuite><testcase classname="c" name="t"><failure/></testcase></testsuite>"#;

#[test]
fn an_interrupted_run_does_not_call_a_run_passed_that_its_report_calls_red() {
    let cwd = Scratch::new("interrupt-red-report");
    // Every run exits 0 over a report with a failure; the second hangs.
    write_executable(
        &cwd,
        "pytest",
        &format!(
            "{FAKE_PYTEST}printf '%s' '{ONE_FAILING_TEST}' > \"$report\"\n\
             echo $$ > \"pid-$n\"\n[ $n -lt 2 ] || exec sleep 30\nexit 0\n"
        ),
    );
    let child = sooth_in_its_own_group(&cwd, &["--runs", "3", "--", "./pytest"]);
    wait_for(&cwd.join("pid-2"));
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("result: interrupted after 1 of 3 runs — it failed"),
        "the run that finished was red by its report: {stdout:?}"
    );
    assert!(
        !stderr.contains("exiting 1"),
        "the exit is 2, and nothing may say otherwise: {stderr:?}"
    );
}

#[test]
fn a_request_to_stop_that_comes_after_a_run_ended_still_counts_that_run() {
    let cwd = Scratch::new("interrupt-after-run");
    write_executable(
        &cwd,
        "pytest",
        &format!(
            "{FAKE_PYTEST}{}exit 0\n",
            report_that_arrives_after_a_stop(
                r#"<testsuite><testcase classname="c" name="t"/></testsuite>"#
            )
        ),
    );
    let child = sooth_in_its_own_group(&cwd, &["--runs", "2", "--", "./pytest"]);
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    // The runner had exited when sooth was asked to stop: that run counts.
    assert!(
        stdout.contains("run 1/2: runner exit=0"),
        "got: {stdout:?} {stderr:?}"
    );
    assert!(
        stdout.contains("result: interrupted after 1 of 2 runs — it passed"),
        "got: {stdout:?} {stderr:?}"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("invocations"))
            .expect("invocations")
            .trim(),
        "1",
        "no run is started after the request"
    );
}

#[test]
fn asked_to_stop_before_verifying_sooth_starts_no_re_run_and_exits_two() {
    let cwd = Scratch::new("interrupt-before-verify");
    write_executable(
        &cwd,
        "pytest",
        &format!(
            "{FAKE_PYTEST}{}exit 1\n",
            report_that_arrives_after_a_stop(ONE_FAILING_TEST)
        ),
    );
    let child = sooth_in_its_own_group(&cwd, &["--verify", "--", "./pytest"]);
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert_eq!(
        std::fs::read_to_string(cwd.join("invocations"))
            .expect("invocations")
            .trim(),
        "1",
        "the suite ran; no re-run was started"
    );
    // The suite ran to its end: its verdict stands, its failures unverified.
    assert!(stdout.contains("result: ✗ FAILED"), "got: {stdout:?}");
    assert!(
        stderr.contains("interrupted before the failures were verified"),
        "got: {stderr:?}"
    );
    // And the report says so itself, for a reader of stdout alone.
    assert!(
        stdout.contains("note: interrupted before the failures were verified"),
        "got: {stdout:?}"
    );
}

#[test]
fn a_verification_cut_short_exits_two_with_the_suites_own_report() {
    let cwd = Scratch::new("interrupt-during-verify");
    // The suite fails and reports it; the first re-run hangs.
    write_executable(
        &cwd,
        "pytest",
        &format!(
            "{FAKE_PYTEST}echo $$ > \"pid-$n\"\n[ $n -lt 2 ] || exec sleep 30\n\
             printf '%s' '{ONE_FAILING_TEST}' > \"$report\"\nexit 1\n"
        ),
    );
    let child = sooth_in_its_own_group(&cwd, &["--verify", "--", "./pytest"]);
    wait_for(&cwd.join("pid-2"));
    let asked = std::time::Instant::now();
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let waited = asked.elapsed();
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(stdout.contains("result: ✗ FAILED"), "got: {stdout:?}");
    assert!(
        stderr.contains("interrupted before the failures were verified"),
        "got: {stderr:?}"
    );
    assert!(
        waited < std::time::Duration::from_millis(1500),
        "the re-run died of the same Ctrl-C: {waited:?}"
    );
}

#[test]
fn a_pardon_under_a_cut_short_verification_does_not_stand_alone_on_stdout() {
    let cwd = Scratch::new("interrupt-verify-pardon");
    // The suite's one failure is on the list, so the run is pardoned; the
    // first re-run hangs.
    write_executable(
        &cwd,
        "pytest",
        &format!(
            "{FAKE_PYTEST}echo $$ > \"pid-$n\"\n[ $n -lt 2 ] || exec sleep 30\n\
             printf '%s' '{ONE_FAILING_TEST}' > \"$report\"\nexit 1\n"
        ),
    );
    std::fs::write(cwd.join(".sooth-quarantine"), "c::t\n").expect("write quarantine");
    let child = sooth_in_its_own_group(&cwd, &["--verify", "--fail-on-flaky", "--", "./pytest"]);
    wait_for(&cwd.join("pid-2"));
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?}");
    assert!(stdout.contains("PASSED"), "got: {stdout:?}");
    // Exit 2 beside a bare PASSED would be a riddle in a log of stdout.
    let note = stdout
        .find("note: interrupted before the failures were verified")
        .expect("the report says the verification was cut short");
    assert!(
        note < stdout.find("result:").expect("a verdict"),
        "the verdict stays the last line: {stdout:?}"
    );
}

#[test]
fn the_verdict_counts_a_run_that_only_its_report_calls_red() {
    let cwd = Scratch::new("verdict-red-by-report");
    // Run 1 exits 0 over a failing report, run 2 fails outright, run 3 is
    // green: two runs failed, whatever the first one's exit code says.
    write_executable(
        &cwd,
        "pytest",
        &format!(
            "{FAKE_PYTEST}case $n in\n\
             1) printf '%s' '{ONE_FAILING_TEST}' > \"$report\"; exit 0 ;;\n\
             2) printf '%s' '{ONE_FAILING_TEST}' > \"$report\"; exit 1 ;;\n\
             *) printf '%s' '<testsuite><testcase classname=\"c\" name=\"t\"/></testsuite>' > \"$report\"; exit 0 ;;\n\
             esac\n"
        ),
    );
    let output = sooth_in_its_own_group(&cwd, &["--runs", "3", "--", "./pytest"])
        .wait_with_output()
        .expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains("c::t — flaky (2 of 3 runs now)"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("result: ✗ FAILED — 2 of 3 runs failed"),
        "the verdict agrees with the line above it: {stdout:?}"
    );
}

#[test]
fn interrupted_before_any_run_finished_there_is_nothing_to_report() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let cwd = Scratch::new("interrupt-first");
    let child = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&cwd)
        .args([
            "run",
            "--color",
            "never",
            "--",
            "sh",
            "-c",
            "echo $$ > pid-1; exec sleep 30",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("sooth should start");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !cwd.join("pid-1").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the run never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stderr.contains("interrupted before the run finished — there is nothing to report"),
        "got: {stderr:?}"
    );
    assert!(
        !stdout.contains("result:"),
        "no verdict on nothing: {stdout:?}"
    );
}

#[test]
fn pruning_the_history_changes_its_size_and_not_what_it_says() {
    let Some(dir) = scratch_repo("history-prune") else {
        return;
    };
    // `c::wob`: 120 invocations on one clean commit, failing every third —
    // a proven flake. `c::ok`: green in the same runs. `c::once`: a flake
    // proven in the first ten, and skipped ever since.
    let lines: String = (1..=120)
        .flat_map(|at| {
            let wob = if at % 3 == 0 { "failed" } else { "passed" };
            let once = match at {
                11.. => "skipped",
                _ if at % 2 == 0 => "passed",
                _ => "failed",
            };
            [("c::wob", wob), ("c::ok", "passed"), ("c::once", once)].map(|(id, status)| {
                format!(
                    r#"{{"at":{at},"commit":"aaa","dirty":false,"env":"local","run":"run-{at}-1","status":"{status}","id":"{id}"}}"#
                ) + "\n"
            })
        })
        .collect();
    std::fs::create_dir_all(dir.join(".sooth")).expect("mkdir");
    std::fs::write(dir.join(".sooth/history.jsonl"), &lines).expect("write history");
    let history = |extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args(["history", "--color", "never"])
            .args(extra)
            .output()
            .expect("sooth should run");
        assert_eq!(output.status.code(), Some(0));
        String::from_utf8(output.stdout).expect("stdout should be UTF-8")
    };
    // The analysis, without the line that states the size.
    let findings = |stdout: &str| -> Vec<String> {
        stdout
            .lines()
            .filter(|line| !line.starts_with("history now holds") && !line.starts_with("pruned:"))
            .map(str::to_owned)
            .collect()
    };

    let before = history(&[]);
    assert!(
        before.contains("c::wob") && before.contains("c::once"),
        "both flakes are found: {before:?}"
    );
    assert!(
        before.contains("history now holds 360 observations"),
        "got: {before:?}"
    );

    // Each test keeps its last 50 invocations; `c::once` also the ten that
    // said something, which the skips after them do not push out.
    let pruned = history(&["--prune"]);
    assert!(
        pruned.contains(
            "pruned: removed 200 observations older than each test's last 50 invocations, kept 160"
        ),
        "got: {pruned:?}"
    );
    assert!(
        pruned.contains("history now holds 160 observations"),
        "got: {pruned:?}"
    );
    assert_eq!(
        findings(&pruned),
        findings(&before),
        "pruning changed a finding"
    );

    let again = history(&["--prune"]);
    assert!(
        again.contains("pruned: nothing to remove"),
        "got: {again:?}"
    );
    assert_eq!(findings(&again), findings(&before));
}

/// A fake `phpunit` whose tests depend on the order they run in. `needs`
/// (and `needs1`, `needs2`, …) passes only behind `setup`; `victim` fails
/// behind `leak`. Asked for a
/// random order (`--random-order-seed=N`) it runs them back to front, the
/// same way for every seed; asked for one test (`--filter`) it runs that
/// test alone. `odd` errors with a `TypeError` in the shuffled order and
/// fails plainly alone. Knobs, through the environment: `TESTS` (the tests
/// in their own order), `WOB_FAILS_IN` and `WOB_SKIPS_IN` (the invocations
/// in which the test `wob` fails, or is skipped), `HANG_IN` (the invocation
/// in which the test `hangs` never comes back), `HANG_ALONE` (a test run
/// alone never comes back), `ALONE_ALSO` (tests the runner runs in front of
/// the one it was asked for, as a selection by name can), `KEEP_ORDER`
/// (ignore the shuffle). It writes down the seed of every
/// suite run in `seeds`, an empty line for none, and the test of every run
/// alone in `alone`.
fn write_order_runner(dir: &Path) {
    write_executable(
        dir,
        "phpunit",
        concat!(
            "#!/bin/sh\n",
            "n=$(( $(cat invocations 2>/dev/null || echo 0) + 1 )); echo $n > invocations\n",
            "seed=''; report=''; filter=''; prev=''\n",
            "for a in \"$@\"; do\n",
            "  case \"$a\" in --random-order-seed=*) seed=\"${a#--random-order-seed=}\" ;; esac\n",
            "  [ \"$prev\" = '--log-junit' ] && report=\"$a\"\n",
            "  [ \"$prev\" = '--filter' ] && filter=\"$a\"\n",
            "  prev=\"$a\"\n",
            "done\n",
            "order=${TESTS:-setup needs victim leak steady}\n",
            "if [ -n \"$filter\" ]; then\n",
            "  order=$(printf '%s' \"$filter\" | sed -E 's/.*\\(([^()]*)\\)\\$\\/i$/\\1/')\n",
            "  echo \"$order\" >> alone\n",
            "  printf '%s\\n' \"$filter\" >> filters\n",
            "  order=\"${ALONE_ALSO:+$ALONE_ALSO }$order\"\n",
            "  [ -z \"${HANG_ALONE:-}\" ] || { echo $$ > hanging; exec sleep 30; }\n",
            "else\n",
            "  echo \"$seed\" >> seeds\n",
            "fi\n",
            "if [ -n \"$seed\" ] && [ -z \"${KEEP_ORDER:-}\" ]; then\n",
            "  back=''; for t in $order; do back=\"$t $back\"; done; order=$back\n",
            "fi\n",
            "cases=''; ran=' '; failed=''\n",
            "for t in $order; do\n",
            "  fail=''; how='<failure/>'\n",
            "  case \"$t\" in\n",
            "    odd) if [ -n \"$filter\" ]; then fail=1; elif [ -n \"$seed\" ]; then fail=1; how='<error type=\\\"TypeError\\\"/>'; fi ;;\n",
            "    needs*) case \"$ran\" in *' setup '*) ;; *) fail=1 ;; esac ;;\n",
            "    victim) case \"$ran\" in *' leak '*) fail=1 ;; esac ;;\n",
            "    wob) case \" ${WOB_FAILS_IN:-} \" in *\" $n \"*) fail=1 ;; esac\n",
            "         case \" ${WOB_SKIPS_IN:-} \" in *\" $n \"*) fail=1; how='<skipped/>' ;; esac ;;\n",
            "    hangs) [ \"${HANG_IN:-}\" != \"$n\" ] || { echo $$ > hanging; exec sleep 30; } ;;\n",
            "  esac\n",
            "  if [ -n \"$fail\" ]; then\n",
            "    cases=\"$cases<testcase classname=\\\"c\\\" name=\\\"$t\\\">$how</testcase>\"\n",
            "    [ \"$how\" = '<skipped/>' ] || failed=1\n",
            "  else\n",
            "    cases=\"$cases<testcase classname=\\\"c\\\" name=\\\"$t\\\"/>\"\n",
            "  fi\n",
            "  ran=\"$ran$t \"\n",
            "done\n",
            "printf '<testsuite>%s</testsuite>' \"$cases\" > \"$report\"\n",
            "echo \"ran: $order\"\n",
            "[ -z \"$failed\" ]\n",
        ),
    );
}

/// `sooth order --color never <args> -- ./phpunit` in `cwd`, with `envs`
/// for the fake runner.
fn order_pass(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(cwd)
        .envs(envs.iter().copied())
        .args(["order", "--color", "never"])
        .args(args)
        .args(["--", "./phpunit"])
        .output()
        .expect("sooth should run")
}

fn lines_of(cwd: &Path, file: &str) -> Vec<String> {
    std::fs::read_to_string(cwd.join(file))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_order_pass_finds_the_tests_whose_result_follows_the_order() {
    let cwd = Scratch::new("order-finds");
    write_order_runner(&cwd);
    let output = order_pass(&cwd, &[], &[]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    // A test that fails in another order fails a run: exit 1.
    assert_eq!(output.status.code(), Some(1), "got: {stdout:?} {stderr:?}");
    // Your order, shuffled — and, because tests differed, both once more.
    let seeds = lines_of(&cwd, "seeds");
    assert_eq!(seeds.len(), 4, "got: {seeds:?}");
    assert!(seeds[0].is_empty() && seeds[3].is_empty(), "got: {seeds:?}");
    assert!(
        !seeds[1].is_empty() && seeds[1] == seeds[2],
        "one seed, replayed: {seeds:?}"
    );
    let seed = &seeds[1];
    for line in [
        "your order: runner exit=0".to_owned(),
        format!("shuffled, seed {seed}: runner exit=1"),
        format!("shuffled again, seed {seed}: runner exit=1"),
        "your order again: runner exit=0".to_owned(),
        "alone: 2 tests, twice each".to_owned(),
        "tests: 5 tests compared in both orders".to_owned(),
        "2 tests depend on the order".to_owned(),
        // Which way each dependency runs is what the test says alone.
        concat!(
            "  ✗ c::needs — passed in your order, failed shuffled, twice each\n",
            "      alone it fails the same way: it needs something a test before it leaves behind\n",
        )
        .to_owned(),
        concat!(
            "  ✗ c::victim — passed in your order, failed shuffled, twice each\n",
            "      alone it passes: something a test before it leaves behind breaks it\n",
        )
        .to_owned(),
        format!("replay this order: sooth order --seed {seed} -- ./phpunit"),
        "result: ✗ ORDER-DEPENDENT — 2 of 5 tests".to_owned(),
    ] {
        assert!(stdout.contains(&line), "missing {line:?} in {stdout:?}");
    }
    // Each of the two, by itself, twice — and nothing else. Asked for by
    // class and by its whole name: `needs` is not `needs_more`.
    assert_eq!(
        lines_of(&cwd, "alone"),
        ["needs", "needs", "victim", "victim"]
    );
    assert_eq!(
        lines_of(&cwd, "filters")[0],
        r"/^(?:P\\)?c::(?:test_?)?(needs)$/i",
        "the selection names the class and ends where the name ends"
    );
    // Quiet runs: the runner speaks once, for the first run that failed.
    assert!(
        stdout.contains(
            "output of the shuffled run, the first that failed:\nran: steady leak victim needs setup \n"
        ),
        "got: {stdout:?}"
    );
    assert_eq!(stdout.matches("ran: ").count(), 1, "got: {stdout:?}");
    assert!(
        stderr.contains("using `--preset phpunit`, inferred from `./phpunit`"),
        "got: {stderr:?}"
    );
    // The run in the suite's own order is an ordinary run and is recorded;
    // the shuffled ones would read as flakes to the history and are not.
    assert_eq!(lines_of(&cwd, ".sooth/history.jsonl").len(), 5);
}

#[test]
fn the_order_pass_replays_the_order_of_a_seed_it_is_given() {
    let cwd = Scratch::new("order-seed");
    write_order_runner(&cwd);
    let output = order_pass(&cwd, &["--no-history", "--seed", "7"], &[]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert_eq!(lines_of(&cwd, "seeds"), ["", "7", "7", ""]);
    assert!(
        stdout.contains("replay this order: sooth order --seed 7 --no-history -- ./phpunit"),
        "got: {stdout:?}"
    );
    assert!(!cwd.join(".sooth").exists(), "--no-history records nothing");
}

#[test]
fn two_orders_that_agree_are_two_runs_and_a_sample_not_a_proof() {
    let cwd = Scratch::new("order-agrees");
    write_order_runner(&cwd);
    let output = order_pass(&cwd, &["--no-history"], &[("TESTS", "a b c")]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert_eq!(lines_of(&cwd, "seeds").len(), 2, "nothing to run again");
    assert!(
        stdout.contains(
            "no test behaved differently in the shuffled order — two orders are a sample, \
             not a proof"
        ),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("result: ✓ NO DIFFERENCE — 3 tests behaved the same in both orders, seed "),
        "got: {stdout:?}"
    );
    assert!(
        !stdout.contains("ran: "),
        "a green pass stays quiet: {stdout:?}"
    );
}

#[test]
fn a_difference_that_does_not_hold_is_not_called_an_order_dependency() {
    let cwd = Scratch::new("order-flake");
    write_order_runner(&cwd);
    // `wob` fails in invocation 2, the first shuffled run, and never again.
    let output = order_pass(
        &cwd,
        &["--no-history"],
        &[("TESTS", "a wob b"), ("WOB_FAILS_IN", "2")],
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "a run was red: {stdout:?}");
    assert_eq!(lines_of(&cwd, "seeds").len(), 4);
    assert!(
        stdout.contains(
            "1 test differed between the orders, and not the same way when both ran again"
        ),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("  ~ c::wob — passed in your order, failed shuffled, the first time\n"),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("ORDER-DEPENDENT"), "got: {stdout:?}");
    assert!(
        stdout
            .contains("result: ✗ FAILED — a run was red; no test followed the order in these runs"),
        "got: {stdout:?}"
    );
}

#[test]
fn a_flake_in_the_first_run_is_not_a_test_that_fails_in_your_order() {
    let cwd = Scratch::new("order-flake-first");
    write_order_runner(&cwd);
    // `wob` fails in invocation 1 only: red in your order once, green in
    // the shuffled order twice. Only your order, run again, tells.
    let output = order_pass(
        &cwd,
        &["--no-history"],
        &[("TESTS", "a wob b"), ("WOB_FAILS_IN", "1")],
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains("  ~ c::wob — failed in your order, passed shuffled, the first time\n"),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("ORDER-DEPENDENT"), "got: {stdout:?}");
}

#[test]
fn the_order_pass_refuses_what_it_cannot_do_before_it_runs_anything() {
    let cwd = Scratch::new("order-refuses");
    write_order_runner(&cwd);
    let refused = |args: &[&str], command: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&cwd)
            .args(["order", "--color", "never"])
            .args(args)
            .arg("--")
            .args(command)
            .output()
            .expect("sooth should run");
        assert_eq!(output.status.code(), Some(2), "{args:?} {command:?}");
        String::from_utf8(output.stderr).expect("stderr should be UTF-8")
    };
    assert!(refused(&["--preset", "pytest"], &["./phpunit"]).contains("cannot shuffle pytest yet"));
    assert!(refused(&[], &["./phpunit", "--order-by=defects"])
        .contains("the command sets an order of its own (`--order-by=defects`)"));
    assert!(refused(&[], &["./phpunit", "--parallel"]).contains("spreads the tests over workers"));
    // A command that names no runner sooth knows.
    assert!(refused(&[], &["sh", "-c", "true"]).contains("it has to know the runner"));
    assert!(
        !cwd.join("invocations").exists(),
        "nothing ran for a pass that was refused"
    );
}

#[test]
fn a_shuffle_with_nothing_to_show_is_refused_for_that_and_not_tried_again() {
    let cwd = Scratch::new("order-nothing-to-show");
    // A Jest project: one file is the rows of a `test.each` under one
    // title, the other has one test. Jest shuffles inside a file, and no
    // report shows rows that share a name changing places.
    write_executable(
        &cwd,
        "jest",
        concat!(
            "#!/bin/sh\n",
            "echo run >> runs\n",
            "row='<testcase classname=\"each.test.js rows\" name=\"row\" file=\"each.test.js\"/>'\n",
            "one='<testcase classname=\"other.test.js\" name=\"x\" file=\"other.test.js\"/>'\n",
            "printf '<testsuite>%s%s%s</testsuite>' \"$row\" \"$row\" \"$one\" > \"$JEST_JUNIT_OUTPUT_FILE\"\n",
        ),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&cwd)
        .args([
            "order",
            "--color",
            "never",
            "--no-history",
            "--preset",
            "jest",
        ])
        .args(["--", "./jest"])
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert!(
        stderr.contains(
            "the order pass has no order to compare: this runner shuffles the tests inside \
             each file, and no file has two tests with names of their own that ran in both \
             runs. 1 test stands for several cases under one name"
        ),
        "got: {stderr:?}"
    );
    // Nothing is blamed on a configuration, and no other seed is tried:
    // none could show more.
    assert!(!stderr.contains("is one set there?"), "got: {stderr:?}");
    assert_eq!(lines_of(&cwd, "runs").len(), 2, "got: {stderr:?}");
}

#[test]
fn files_that_finished_in_another_order_are_not_said_to_have_stayed_in_place() {
    let cwd = Scratch::new("order-files-swapped");
    // Jest ignores the shuffle here, as under a pinned order, and its two
    // files finish the other way round the second time: every test stands
    // somewhere else in the report, and inside each file nothing moved.
    write_executable(
        &cwd,
        "jest",
        concat!(
            "#!/bin/sh\n",
            "n=$(( $(cat invocations 2>/dev/null || echo 0) + 1 )); echo $n > invocations\n",
            "file() { printf '<testcase classname=\"%s.test.js\" name=\"one\" file=\"%s.test.js\"/>' $1 $1\n",
            "         printf '<testcase classname=\"%s.test.js\" name=\"two\" file=\"%s.test.js\"/>' $1 $1; }\n",
            "if [ $n = 1 ]; then cases=\"$(file b)$(file a)\"; else cases=\"$(file a)$(file b)\"; fi\n",
            "printf '<testsuite>%s</testsuite>' \"$cases\" > \"$JEST_JUNIT_OUTPUT_FILE\"\n",
        ),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&cwd)
        .args([
            "order",
            "--color",
            "never",
            "--no-history",
            "--preset",
            "jest",
        ])
        .args(["--seed", "3", "--", "./jest"])
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert!(
        stderr.contains(
            "the shuffled run (seed 3) lists the tests that ran in both runs in the order \
             the first run had them inside every file, so the order pass compared nothing."
        ),
        "got: {stderr:?}"
    );
}

#[test]
fn a_shuffle_that_changed_nothing_is_a_pass_that_was_not_made() {
    let cwd = Scratch::new("order-unchanged");
    write_order_runner(&cwd);
    // The runner ignores the shuffle, as it does when its own configuration
    // sets an order. A seed that was asked for is not second-guessed.
    let output = order_pass(
        &cwd,
        &["--no-history", "--seed", "3"],
        &[("KEEP_ORDER", "1")],
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert!(
        stderr.contains(
            "the shuffled run (seed 3) lists the tests that ran in both runs in the order \
             the first run had them, so the order pass compared nothing. An order set in \
             the runner's configuration or inside a script outranks the flag sooth adds: is \
             one set there?"
        ),
        "got: {stderr:?}"
    );
    assert_eq!(lines_of(&cwd, "seeds").len(), 2);

    // Without one, a few more seeds are tried before giving up.
    std::fs::remove_file(cwd.join("seeds")).expect("remove");
    let output = order_pass(&cwd, &["--no-history"], &[("KEEP_ORDER", "1")]);
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert_eq!(
        lines_of(&cwd, "seeds").len(),
        6,
        "one given run, five seeds"
    );
    assert!(stderr.contains("trying another"), "got: {stderr:?}");

    // One test has no order to depend on.
    let output = order_pass(&cwd, &["--no-history"], &[("TESTS", "only")]);
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert!(
        stderr.contains("needs at least two tests that ran in both runs, and found 1"),
        "got: {stderr:?}"
    );
}

#[test]
fn a_test_the_history_proved_flaky_is_not_called_order_dependent() {
    let cwd = Scratch::new("order-known-flake");
    write_order_runner(&cwd);
    // The history has seen `c::wob` pass and fail on one clean commit.
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    let seen = |at: u32, status: &str| {
        format!(
            r#"{{"at":{at},"commit":"aaa","dirty":false,"env":"local","run":"run-{at}-1","status":"{status}","id":"c::wob"}}"#
        ) + "\n"
    };
    std::fs::write(
        cwd.join(".sooth/history.jsonl"),
        seen(1, "passed") + &seen(2, "failed") + &seen(3, "passed"),
    )
    .expect("write history");
    // Here it fails in both shuffled runs and in neither of the others:
    // what an order-dependent test does, and what a flake can do too.
    let output = order_pass(&cwd, &[], &[("TESTS", "a wob b"), ("WOB_FAILS_IN", "2 3")]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains(
            "1 test differed that the history already proved flaky — no word on order from it"
        ),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("  ~ c::wob — passed in your order, failed shuffled"),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("ORDER-DEPENDENT"), "got: {stdout:?}");

    // Without the history there is nothing to hold against the four runs.
    std::fs::remove_file(cwd.join("invocations")).expect("remove");
    let output = order_pass(
        &cwd,
        &["--no-history"],
        &[("TESTS", "a wob b"), ("WOB_FAILS_IN", "2 3")],
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(
        stdout.contains("1 test depends on the order"),
        "got: {stdout:?}"
    );
}

#[test]
fn a_suite_where_everything_depends_on_one_test_is_not_started_a_hundred_times() {
    let cwd = Scratch::new("order-alone-cap");
    write_order_runner(&cwd);
    // Twenty-two tests that all need `setup` before them.
    let needy: Vec<String> = (1..=22).map(|n| format!("needs{n}")).collect();
    let tests = format!("setup {}", needy.join(" "));
    let output = order_pass(&cwd, &["--no-history"], &[("TESTS", &tests)]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains("22 tests depend on the order"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("alone: 20 tests, twice each"),
        "got: {stdout:?}"
    );
    assert_eq!(lines_of(&cwd, "alone").len(), 40);
    assert_eq!(
        stdout.matches("alone it fails the same way").count(),
        20,
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("note: 20 tests were run alone; the other 2 were not — sooth stops there"),
        "got: {stdout:?}"
    );
}

#[test]
fn a_run_alone_that_was_not_alone_gives_no_direction() {
    let cwd = Scratch::new("order-in-company");
    write_order_runner(&cwd);
    // Asked for `needs`, this runner also runs `setup` in front of it, as a
    // selection by name does when two tests answer to it. In that company
    // the test passes — and "alone it passes" would send its reader to a
    // leak that does not exist: the test needs `setup`.
    let output = order_pass(
        &cwd,
        &["--no-history"],
        &[("TESTS", "setup needs steady"), ("ALONE_ALSO", "setup")],
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains(concat!(
            "  ✗ c::needs — passed in your order, failed shuffled, twice each\n",
            "      not seen alone: asked for this test, the runner ran others with it, so \
             which way the dependency runs is not known\n",
        )),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("alone it passes"), "got: {stdout:?}");
    assert!(
        stdout.contains("result: ✗ ORDER-DEPENDENT — 1 of 3 tests"),
        "it still depends on the order: {stdout:?}"
    );
}

#[test]
fn a_difference_no_second_run_spoke_to_is_said_to_be_unconfirmed() {
    let cwd = Scratch::new("order-unconfirmed");
    write_order_runner(&cwd);
    // `wob` fails in the first shuffled run (2) and is skipped in the
    // second (3): nobody saw the difference hold, and nobody saw it not.
    let output = order_pass(
        &cwd,
        &["--no-history"],
        &[
            ("TESTS", "a wob b"),
            ("WOB_FAILS_IN", "2"),
            ("WOB_SKIPS_IN", "3"),
        ],
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains(concat!(
            "1 test differed between the orders, and a second run had no result for it\n",
            "  ? c::wob — passed in your order, failed shuffled, once\n",
        )),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("depends on the order"), "got: {stdout:?}");
    assert!(!stdout.contains("not the same way"), "got: {stdout:?}");
}

#[test]
fn a_test_that_does_not_hold_alone_is_taken_back_out() {
    let cwd = Scratch::new("order-mixed-alone");
    write_order_runner(&cwd);
    // Four suite runs that look like a dependency — green in your order
    // (1, 4), red shuffled (2, 3) — and then, alone, a failure (5) and a
    // pass (6). Nothing ran before it either time.
    let output = order_pass(
        &cwd,
        &["--no-history"],
        &[("TESTS", "a wob b"), ("WOB_FAILS_IN", "2 3 5")],
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert_eq!(lines_of(&cwd, "alone"), ["wob", "wob"]);
    assert!(
        stdout.contains(concat!(
            "1 test followed the order in the suite and did not do the same thing alone \
             twice — sooth does not call it order-dependent\n",
            "  ~ c::wob — passed in your order, failed shuffled, twice each; alone it passed \
             once and failed once\n",
        )),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("ORDER-DEPENDENT"), "got: {stdout:?}");
    assert!(!stdout.contains("depends on the order"), "got: {stdout:?}");
}

#[test]
fn a_failure_alone_that_is_another_failure_gives_no_direction() {
    let cwd = Scratch::new("order-failed-differently");
    write_order_runner(&cwd);
    // `odd` errors with a TypeError when shuffled; alone it fails an
    // assertion. That is not the suite's failure reproduced.
    let output = order_pass(&cwd, &["--no-history"], &[("TESTS", "a odd b")]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert!(
        stdout.contains(concat!(
            "  ✗ c::odd — passed in your order, failed shuffled, twice each\n",
            "      alone it fails in another way than in the suite, which says nothing about \
             this failure\n",
        )),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("it needs something"), "got: {stdout:?}");
}

#[test]
fn asked_to_stop_while_the_tests_run_alone_the_pass_is_still_reported() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let cwd = Scratch::new("order-interrupted-alone");
    write_order_runner(&cwd);
    let child = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&cwd)
        .env("HANG_ALONE", "1")
        .args([
            "order",
            "--no-history",
            "--color",
            "never",
            "--",
            "./phpunit",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("sooth should start");
    wait_for(&cwd.join("hanging"));
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    // The four runs were made: what they found is said, without a direction.
    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("2 tests depend on the order"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("result: ✗ ORDER-DEPENDENT — 2 of 5 tests"),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains("alone it"), "got: {stdout:?}");
    assert!(
        stderr.contains("interrupted while the tests were run alone"),
        "got: {stderr:?}"
    );
}

#[test]
fn an_order_pass_that_is_cut_short_reports_nothing_and_exits_two() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let cwd = Scratch::new("order-interrupted");
    write_order_runner(&cwd);
    std::fs::create_dir_all(cwd.join("tmp")).expect("mkdir");
    // The shuffled run, the second invocation, never comes back.
    let child = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&cwd)
        .env("TMPDIR", cwd.join("tmp"))
        .env("TESTS", "a hangs b")
        .env("HANG_IN", "2")
        .args([
            "order",
            "--no-history",
            "--color",
            "never",
            "--",
            "./phpunit",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("sooth should start");
    wait_for(&cwd.join("hanging"));
    send_signal("-INT", &format!("-{}", child.id()));
    let output = child.wait_with_output().expect("sooth should exit");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(
        stderr.contains(
            "interrupted during the shuffled run — the order pass compares whole runs, so \
             there is nothing to report"
        ),
        "got: {stderr:?}"
    );
    assert!(
        !stdout.contains("result:"),
        "no verdict on half a pass: {stdout:?}"
    );
    let leftovers: Vec<_> = std::fs::read_dir(cwd.join("tmp")).expect("tmp").collect();
    assert!(
        leftovers.is_empty(),
        "nothing left in the temp dir: {leftovers:?}"
    );
}

#[test]
fn the_order_pass_under_bare_json_prints_the_document_and_nothing_else() {
    let cwd = Scratch::new("order-json");
    write_order_runner(&cwd);
    let output = order_pass(&cwd, &["--no-history", "--seed", "7", "--json"], &[]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "got: {stdout:?}");
    assert_eq!(stdout.lines().count(), 1, "one line of JSON: {stdout:?}");
    assert!(
        stdout.starts_with(r#"{"schema_version":1,"sooth_version":""#),
        "got: {stdout:?}"
    );
    for part in [
        r#""order":{"seed":7,"compared":5,"runs":[{"order":"given","exit_code":0,"success":true,"#,
        r#"{"order":"shuffled","seed":7,"exit_code":1,"success":false,"#,
        r#""order_dependent":[{"name":"c::needs","passed_in":"given","alone":"needs_predecessor"},{"name":"c::victim","passed_in":"given","alone":"broken_by_predecessor"}],"unstable":[],"unconfirmed":[],"known_flakes":[],"alone":{"tests":2,"not_run":0}}}"#,
    ] {
        assert!(stdout.contains(part), "missing {part} in {stdout:?}");
    }
    assert_eq!(stdout.matches(r#""order":"shuffled""#).count(), 2);
    // What the quiet runs wrote goes to stderr: stdout is the document's.
    assert!(
        stderr.contains("output of the shuffled run, the first that failed:"),
        "got: {stderr:?}"
    );
}

#[test]
fn json_to_a_file_keeps_the_human_report_on_stdout() {
    let (cwd, mut command) = sooth_in("json-file");
    let (report, write_report) = fresh_report(&cwd);
    let json_path = cwd.join("report.json");
    let output = command
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
}

#[test]
fn a_junit_report_that_predates_the_run_is_rejected_as_stale() {
    let (cwd, mut command) = sooth_in("stale");
    let report = cwd.join("report.xml");
    // Written BEFORE the run; the wrapped command touches nothing.
    std::fs::copy(fixture(), &report).expect("fixture should copy");

    let output = command
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
    let (_cwd, mut command) = sooth_in("failing-command");
    let output = command
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
    let (_cwd, mut command) = sooth_in("unspawnable");
    let output = command
        .args(["run", "--", "sooth-no-such-binary-xyzzy"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("failed to run"), "got: {stderr:?}");
}

#[test]
fn reportless_json_is_rejected_with_exit_two() {
    let (_cwd, mut command) = sooth_in("reportless-json");
    let output = command
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
fn a_failure_without_a_name_is_never_verified_against_the_whole_suite() {
    use std::os::unix::fs::PermissionsExt;
    let (cwd, mut command) = sooth_in("verify-nameless");
    // The suite's one failure has no `name`. A re-run would have to select
    // "every test"; the runner records whether it was asked to.
    std::fs::write(
        cwd.join("runner.sh"),
        concat!(
            "#!/bin/sh\n",
            "report=\"\"; prev=\"\"\n",
            "for a in \"$@\"; do\n",
            "  if [ \"$prev\" = \"--log-junit\" ]; then report=\"$a\"; fi\n",
            "  case \"$a\" in --filter) touch re-ran;; esac\n",
            "  prev=\"$a\"\n",
            "done\n",
            "printf '<testsuite><testcase classname=\"c\"><failure/></testcase></testsuite>' > \"$report\"\n",
            "exit 1\n",
        ),
    )
    .expect("write runner");
    std::fs::set_permissions(
        cwd.join("runner.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod");
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
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        !cwd.join("re-ran").exists(),
        "the re-run selected every test for a failure it could not name"
    );
    assert!(
        stderr.contains("verification skipped"),
        "skipping must be said: {stderr:?}"
    );
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
}

#[test]
fn verify_selects_a_test_by_the_name_its_report_gave_it() {
    use std::os::unix::fs::PermissionsExt;
    // The fixture applies `--filter` with perl, as PHPUnit does with PCRE.
    if Command::new("perl").arg("-v").output().is_err() {
        return;
    }
    // (the method PHPUnit filters on, the name the report carries)
    let cases = [
        // Plain PHPUnit reports the method name itself.
        ("test_ships", "test_ships"),
        (
            "test_numbers with data set #0",
            "test_numbers with data set #0",
        ),
        // A Pest-style test is filtered on its description.
        ("ships on time", "ships on time"),
        // Pest prettifies a class-based method: the prefix goes, underscores
        // and camelCase humps become spaces, the first letter is raised.
        ("test_that_true_is_true", "That true is true"),
        ("testCamelCaseName", "Camel case name"),
        ("testHttp2Works", "Http 2 works"),
        ("it_is_attribute_based", "It is attribute based"),
        ("test_UPPER_and_lower", "UPPER and lower"),
        ("test_numbers with data set #1", "Numbers with data set #1"),
        (
            "testNamedSets with data set \"big.one\"",
            "Named sets with data set &quot;big.one&quot;",
        ),
    ];
    for (method, reported) in cases {
        let (cwd, mut command) = sooth_in("verify-spelling");
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/filter_runner.sh"
        );
        let runner = cwd.join("pest");
        std::fs::copy(fixture, &runner).expect("copy runner");
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let output = command
            .env("SOOTH_TEST_METHOD", method)
            .env("SOOTH_TEST_NAME", reported)
            .args([
                "run",
                "--verify",
                "--no-history",
                "--preset",
                "phpunit",
                "--color",
                "never",
                "--",
                "./pest",
            ])
            .output()
            .expect("sooth should run");
        let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

        assert!(
            stdout.contains("real (reproduced on re-run)"),
            "`{method}`, reported as `{reported}`, must be found again: {stdout:?}"
        );
    }
}

#[test]
fn verify_with_an_unselectable_preset_is_rejected_up_front_with_exit_two() {
    let (_cwd, mut command) = sooth_in("verify-unselectable");
    let output = command
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

#[test]
fn an_unreadable_quarantine_warns_and_pardons_nothing() {
    let (cwd, mut command) = sooth_in("quarantine-unreadable");
    // A directory where the list should be: present, but not readable as a file.
    std::fs::create_dir(cwd.join(".sooth-quarantine")).expect("quarantine dir should create");
    let (report, write_report) = fresh_report(&cwd);
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

    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(
        output.status.code(),
        Some(1),
        "an unreadable list pardons nothing, got: {stderr:?}"
    );
    assert!(
        stderr.contains("sooth: could not read `.sooth-quarantine`")
            && stderr.contains("no failure will be recognized as a known flake"),
        "got: {stderr:?}"
    );
}

#[test]
fn a_quarantined_failure_is_pardoned_with_fail_on_flaky() {
    let (cwd, mut command) = sooth_in("quarantine-hit");
    std::fs::write(
        cwd.join(".sooth-quarantine"),
        "# known flakes\ntests.test_math::test_subtraction\n",
    )
    .expect("quarantine file should write");
    let (report, write_report) = fresh_report(&cwd);
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
fn a_pardoned_regression_is_not_called_a_flake() {
    let (cwd, mut command) = sooth_in("quarantine-regression");
    // Green on `aaa`, red on `bbb` in two invocations: a regression by the
    // history, and listed in the quarantine all the same.
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    let line = |at: u32, commit: &str, status: &str| {
        format!(
            r#"{{"at":{at},"commit":"{commit}","dirty":false,"env":"local","run":"r{at}","status":"{status}","id":"c::t"}}"#
        ) + "\n"
    };
    std::fs::write(
        cwd.join(".sooth/history.jsonl"),
        line(1, "aaa", "passed") + &line(2, "bbb", "failed") + &line(3, "bbb", "failed"),
    )
    .expect("history");
    std::fs::write(cwd.join(".sooth-quarantine"), "c::t\n").expect("quarantine");
    let report = cwd.join("report.xml");
    let script = format!(
        "printf '<testsuite><testcase classname=\"c\" name=\"t\"><failure/></testcase></testsuite>' > '{}'; exit 1",
        report.display()
    );
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
            &script,
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    // A human listed the id, so the pardon stands — loudly.
    assert_eq!(output.status.code(), Some(0), "got: {stdout:?}");
    assert!(
        stdout.contains("▼ c::t — failing since bbb"),
        "a regression keeps its mark when it is pardoned: {stdout:?}"
    );
    assert!(
        !stdout.contains("only quarantined flakes failed"),
        "the closing line called a regression a flake: {stdout:?}"
    );
    assert!(
        stdout.contains("only quarantined tests failed (1 test pardoned, 1 of them not a flake"),
        "got: {stdout:?}"
    );
}

#[test]
fn an_unlisted_failure_still_fails_with_fail_on_flaky() {
    let (cwd, mut command) = sooth_in("quarantine-miss");
    std::fs::write(cwd.join(".sooth-quarantine"), "some.other::test_flake\n")
        .expect("quarantine file should write");
    let (report, write_report) = fresh_report(&cwd);
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

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(stdout.contains("result: ✗ FAILED"), "got: {stdout:?}");
}

#[test]
fn reportless_fail_on_flaky_is_rejected_with_exit_two() {
    let (_cwd, mut command) = sooth_in("reportless-fail-on-flaky");
    let output = command
        .args(["run", "--fail-on-flaky", "--", "true"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("requires a report"), "got: {stderr:?}");
}

#[test]
fn a_signal_killed_run_reports_the_signal_and_exits_one() {
    let (_cwd, mut command) = sooth_in("signal");
    let output = command
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
    let (cwd, mut command) = sooth_in("mismatch");
    let (report, write_report) = fresh_report(&cwd);
    let output = command
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
    let (cwd, mut command) = sooth_in("green-mismatch");
    let report = cwd.join("report.xml");
    let write_green_then_fail = format!(
        "printf '<testsuite><testcase classname=\"c\" name=\"ok\"/></testsuite>' > '{}'; exit 3",
        report.display()
    );
    let output = command
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
    let (cwd, mut command) = sooth_in("crash");
    let report = cwd.join("report.xml");
    let write_garbage = format!("echo 'PHP Fatal error' > '{}'; exit 255", report.display());
    let output = command
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

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("failed to parse"), "got: {stderr:?}");
    assert!(stderr.contains("runner exit=255"), "got: {stderr:?}");
    assert!(stderr.contains("output above"), "got: {stderr:?}");
}

#[test]
fn repeated_runs_report_mixed_outcomes_as_flaky() {
    let (cwd, mut command) = sooth_in("flaky");
    let report = cwd.join("report.xml");
    let marker = cwd.join("marker");
    // Run 1: the test fails (runner exits 1). Run 2: it passes. Mixed = flaky.
    let script = format!(
        "if [ -f '{marker}' ]; then printf '<testsuite><testcase classname=\"c\" name=\"wobbly\"/></testsuite>' > '{report}'; else printf '<testsuite><testcase classname=\"c\" name=\"wobbly\"><failure/></testcase></testsuite>' > '{report}'; touch '{marker}'; exit 1; fi",
        marker = marker.display(),
        report = report.display()
    );
    let output = command
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
}

#[test]
fn a_preset_runner_that_stops_writing_reports_fails_loudly() {
    use std::os::unix::fs::PermissionsExt;
    // Run 1 writes a report; run 2 writes nothing. Because sooth deletes the
    // preset report before every run, run 2 must fail loudly instead of
    // silently re-serving run 1's truth.
    let (cwd, mut command) = sooth_in("fakebin");
    let dir = cwd.join("bin");
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
    let output = command
        .env("PATH", path_env)
        .args([
            "run", "--runs", "2", "--preset", "pytest", "--color", "never", "--", "pytest",
        ])
        .output()
        .expect("sooth should run");

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
    let (cwd, mut command) = sooth_in("reorder");
    let report = cwd.join("report.xml");
    let marker = cwd.join("marker");
    let script = format!(
        "if [ -f '{m}' ]; then rm '{m}'; \
         printf '<testsuite><testcase classname=\"c\" name=\"steady\"/><testcase classname=\"c\" name=\"wobbly\"/></testsuite>' > '{r}'; \
         else touch '{m}'; \
         printf '<testsuite><testcase classname=\"c\" name=\"wobbly\"><failure/></testcase><testcase classname=\"c\" name=\"steady\"/></testsuite>' > '{r}'; fi",
        m = marker.display(),
        r = report.display()
    );
    let output = command
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
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(1), "run 1 failed: {stdout:?}");
    assert!(
        stdout.contains(
            "flaky or order-dependent (1 of 2 runs now; run 2 did not share run 1's order)"
        ),
        "a reordered repeat must not claim plain flakiness: {stdout:?}"
    );
}

/// A scratch git repo and the directory holding it. Anything a history test
/// writes next to the repo — a report above all — must land outside the
/// working tree, because an untracked file makes every run dirty and a dirty
/// run is never evidence.
struct Repo {
    scratch: Scratch,
    dir: PathBuf,
}

impl Repo {
    /// A path beside the repo, inside the same scratch directory.
    fn outside(&self, name: &str) -> PathBuf {
        self.scratch.join(name)
    }
}

impl Deref for Repo {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.dir
    }
}

impl AsRef<Path> for Repo {
    fn as_ref(&self) -> &Path {
        &self.dir
    }
}

impl AsRef<OsStr> for Repo {
    fn as_ref(&self) -> &OsStr {
        self.dir.as_os_str()
    }
}

/// A scratch git repo (one commit, `.sooth/` ignored) for history tests;
/// returns `None` when git is unavailable.
fn scratch_repo(tag: &str) -> Option<Repo> {
    Command::new("git").arg("--version").output().ok()?;
    let scratch = Scratch::new(tag);
    let dir = scratch.join("repo");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(dir.join(".gitignore"), ".sooth/\n").ok()?;
    for args in [
        &["init", "-q"][..],
        &["add", "."][..],
        &["commit", "-q", "-m", "init"][..],
    ] {
        git_in(&dir, args);
    }
    Some(Repo { scratch, dir })
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
fn what_sooth_and_the_run_write_does_not_make_a_clean_tree_dirty() {
    let Some(dir) = scratch_repo("own-files") else {
        return;
    };
    // The harshest case: `.sooth/` is not ignored, and the report the run
    // writes sits inside the repository, as in the README's own example.
    std::fs::write(dir.join(".gitignore"), "").expect("write");
    git_in(&dir, &["commit", "-q", "-am", "ignore nothing"]);
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args([
                "run",
                "--junit",
                "report.xml",
                "--color",
                "never",
                "--",
                "sh",
                "-c",
                "printf '<testsuite><testcase classname=\"c\" name=\"t\"/></testsuite>' > report.xml",
            ])
            .output()
            .expect("sooth should run")
    };
    assert_eq!(run().status.code(), Some(0));
    assert_eq!(run().status.code(), Some(0));

    let history = std::fs::read_to_string(dir.join(".sooth/history.jsonl")).expect("history");
    assert_eq!(history.lines().count(), 2, "got: {history}");
    assert!(
        !history.contains(r#""dirty":true"#),
        "the run's own report or sooth's own history marked a clean tree dirty: {history}"
    );
}

/// One green run in `dir`, with its report beside the repository.
fn green_run_in(dir: &Repo) -> std::process::Output {
    let report = dir.outside("report.xml");
    let script = format!(
        "printf '<testsuite><testcase classname=\"c\" name=\"t\"/></testsuite>' > '{}'",
        report.display()
    );
    Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(dir)
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
}

fn git_status_of(dir: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain"])
        .output()
        .expect("git should run");
    String::from_utf8(output.stdout).expect("git output should be UTF-8")
}

#[test]
fn creating_the_history_keeps_it_out_of_git_status_without_touching_the_project() {
    let Some(dir) = scratch_repo("exclude") else {
        return;
    };
    // The project ignores nothing; sooth may not ask it to.
    std::fs::write(dir.join(".gitignore"), "").expect("write");
    git_in(&dir, &["commit", "-q", "-am", "ignore nothing"]);

    let first = green_run_in(&dir);
    let stderr = String::from_utf8(first.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("added `.sooth/` to .git/info/exclude"),
        "the one change sooth makes must be said: {stderr:?}"
    );
    let exclude = std::fs::read_to_string(dir.join(".git/info/exclude")).expect("exclude file");
    assert!(
        exclude.lines().any(|line| line == ".sooth/"),
        "got: {exclude:?}"
    );
    assert_eq!(
        git_status_of(&dir),
        "",
        "the project's tree must stay clean"
    );

    // Said once: the second run finds it ignored and stays quiet.
    let second = green_run_in(&dir);
    let stderr = String::from_utf8(second.stderr).expect("stderr should be UTF-8");
    assert!(!stderr.contains("info/exclude"), "got: {stderr:?}");
    let exclude = std::fs::read_to_string(dir.join(".git/info/exclude")).expect("exclude file");
    assert_eq!(exclude.matches(".sooth/").count(), 1, "got: {exclude:?}");
}

#[test]
fn a_project_that_already_ignores_the_history_is_left_alone() {
    // `scratch_repo` commits a `.gitignore` with `.sooth/`.
    let Some(dir) = scratch_repo("exclude-ignored") else {
        return;
    };
    let before = std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap_or_default();
    let output = green_run_in(&dir);
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert!(!stderr.contains("info/exclude"), "got: {stderr:?}");
    let after = std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap_or_default();
    assert_eq!(before, after);
}

#[test]
fn history_accumulates_across_invocations_and_reports_proven_flakes() {
    let Some(dir) = scratch_repo("history") else {
        return; // no git: identity degrades to unknown, covered by unit tests
    };
    let report = dir.outside("report.xml");
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
}

#[test]
fn a_red_run_labels_its_failures_against_the_accumulated_history() {
    let Some(dir) = scratch_repo("explain-run") else {
        return; // no git: identity degrades to unknown, covered by unit tests
    };
    let report = dir.outside("report.xml");
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
}

#[test]
fn a_first_run_never_cites_its_own_runs_as_known() {
    let Some(dir) = scratch_repo("own-evidence") else {
        return;
    };
    let report = dir.outside("report.xml");
    let count = dir.outside("count");
    // Three runs in one invocation on one clean commit: fail, pass, fail.
    let script = format!(
        "n=$(cat '{count}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{count}'; \
         if [ $n -eq 2 ]; then printf '<testsuite><testcase classname=\"c\" name=\"wob\"/></testsuite>' > '{report}'; \
         else printf '<testsuite><testcase classname=\"c\" name=\"wob\"><failure/></testcase></testsuite>' > '{report}'; exit 1; fi",
        count = count.display(),
        report = report.display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--runs",
            "3",
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

    assert!(
        !stdout.contains("known flake"),
        "a first run cited its own observations as earlier evidence: {stdout:?}"
    );
    assert!(
        stdout.contains("c::wob") && stdout.contains("new (nothing in history)"),
        "the failure must read as new before any earlier run: {stdout:?}"
    );
}

#[test]
fn the_very_first_run_says_its_failures_read_as_new_for_lack_of_history() {
    let Some(dir) = scratch_repo("explain-first") else {
        return;
    };
    let report = dir.outside("report.xml");
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
}

#[test]
fn explain_classifies_a_report_without_running_or_recording_anything() {
    let Some(dir) = scratch_repo("explain-cmd") else {
        return;
    };
    let report = dir.outside("report.xml");
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
}

/// A scratch dir holding a hand-written history for `c::t` — `(at, commit,
/// run, status)` per line — and a report in which `c::t` fails.
fn explain_against_history(tag: &str, lines: &[(u32, &str, &str, &str)]) -> String {
    let (cwd, mut command) = sooth_in(tag);
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    let history: String = lines
        .iter()
        .map(|(at, commit, run, status)| {
            format!(
                r#"{{"at":{at},"commit":"{commit}","dirty":false,"env":"local","run":"{run}","status":"{status}","id":"c::t"}}"#
            ) + "\n"
        })
        .collect();
    std::fs::write(cwd.join(".sooth/history.jsonl"), history).expect("history");
    std::fs::write(
        cwd.join("red.xml"),
        r#"<testsuite><testcase classname="c" name="t"><failure/></testcase></testsuite>"#,
    )
    .expect("report");
    let output = command
        .args(["explain", "--junit", "red.xml", "--color", "never"])
        .output()
        .expect("sooth should run");
    String::from_utf8(output.stdout).expect("stdout should be UTF-8")
}

#[test]
fn a_known_flake_that_went_red_at_a_commit_is_never_nothing_new() {
    // Flaky on `aaa`, green on `bbb`, then red in three invocations on `ccc`.
    let stdout = explain_against_history(
        "red-flake",
        &[
            (1, "aaa", "r1", "passed"),
            (2, "aaa", "r2", "failed"),
            (3, "bbb", "r3", "passed"),
            (4, "bbb", "r4", "passed"),
            (5, "ccc", "r5", "failed"),
            (6, "ccc", "r6", "failed"),
            (7, "ccc", "r7", "failed"),
        ],
    );
    assert!(
        stdout.contains("known flake (4 of 7 in history, 57%), and every run since ccc (3 runs)"),
        "got: {stdout:?}"
    );
    assert!(
        !stdout.contains("nothing new"),
        "a flake that stopped passing was waved through: {stdout:?}"
    );
}

#[test]
fn a_test_never_seen_passing_is_not_called_a_regression() {
    let stdout = explain_against_history(
        "never-passed",
        &[(1, "aaa", "r1", "failed"), (2, "aaa", "r2", "failed")],
    );
    assert!(
        stdout.contains("1 failure — 1 never seen passing"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("never seen passing (failed all 2 observed runs, first on aaa)"),
        "got: {stdout:?}"
    );
    assert!(
        !stdout.contains("regression") && !stdout.contains("failing since"),
        "no commit broke a test that never passed: {stdout:?}"
    );
}

/// Any panic will do, and no input is known to cause one any more (the
/// last was a `time` no `Duration` holds, #158): a debug build of sooth
/// panics when asked to. A release build has no such switch.
#[cfg(debug_assertions)]
#[test]
fn a_panic_is_sooth_failing_exit_two_never_101() {
    let (_cwd, mut command) = sooth_in("panic-exit");
    let output = command
        .env("SOOTH_TEST_PANIC", "1")
        .arg("history")
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(
        output.status.code(),
        Some(2),
        "a panic is sooth's own failure, inside the 0/1/2 contract: {stderr:?}"
    );
    assert!(stderr.contains("sooth: internal error"), "got: {stderr:?}");
    assert!(
        !stderr.contains("RUST_BACKTRACE"),
        "the raw panic message reached the user: {stderr:?}"
    );
}

#[test]
fn a_reader_that_closes_the_pipe_is_a_quiet_exit_two() {
    let (cwd, _) = sooth_in("closed-pipe");
    // `true` is gone long before sooth writes its first line, so the write
    // meets a closed pipe: `sooth history | head` in miniature.
    let script = format!(
        "(sleep 0.3; '{}' history 2> err.txt; echo $? > code.txt) | true",
        env!("CARGO_BIN_EXE_sooth")
    );
    let ran = Command::new("sh")
        .current_dir(&cwd)
        .args(["-c", &script])
        .status()
        .expect("sh should run");
    assert!(ran.success());
    // The subshell may still be writing its two files when `true` returns.
    let code = (0..50)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::read_to_string(cwd.join("code.txt"))
                .ok()
                .filter(|code| !code.trim().is_empty())
        })
        .expect("sooth should have exited");
    let stderr = std::fs::read_to_string(cwd.join("err.txt")).expect("stderr file");

    assert_eq!(code.trim(), "2", "stderr: {stderr:?}");
    assert!(
        stderr.is_empty(),
        "a reader walking away is not worth a word, let alone a panic: {stderr:?}"
    );
}

#[test]
fn a_test_name_cannot_repaint_or_forge_sooths_own_lines() {
    let (cwd, mut command) = sooth_in("forged-verdict");
    let report = cwd.join("forged.xml");
    // ESC[2K + CR erase the line, a fake verdict follows, ESC[8m hides the
    // rest; the `type` and a newline try the same through other fields.
    std::fs::write(
        &report,
        concat!(
            r#"<testsuite><testcase classname="c" "#,
            r#"name="x&#27;[2K&#13;result: ✓ PASSED&#27;[8m&#10;second line">"#,
            r#"<failure type="E&#27;[31mvil"/></testcase></testsuite>"#,
        ),
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

    assert!(
        !stdout.contains('\u{1b}') && !stdout.contains('\r'),
        "a control character from a test name reached the terminal: {stdout:?}"
    );
    assert_eq!(
        stdout.lines().count(),
        3, // headline, the one failure, the empty-history note
        "a newline in a test name must not make a line of its own: {stdout:?}"
    );
    assert!(
        stdout.contains("c::x\u{fffd}[2K\u{fffd}result: ✓ PASSED\u{fffd}[8m\u{fffd}second line"),
        "the name must stay readable, with the control characters replaced: {stdout:?}"
    );
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
}

#[test]
fn explain_on_a_red_report_without_history_says_the_evidence_is_empty() {
    let (cwd, mut command) = sooth_in("explain-empty-history");
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
}

#[test]
fn the_quarantine_labels_a_failure_without_the_flag_but_never_steers_the_exit() {
    let (cwd, mut command) = sooth_in("quarantine-label");
    std::fs::write(
        cwd.join(".sooth-quarantine"),
        "tests.test_math::test_subtraction\n",
    )
    .expect("quarantine file should write");
    let (report, write_report) = fresh_report(&cwd);
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
    let report = dir.outside("report.xml");
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
}

#[test]
fn a_history_made_entirely_on_a_dirty_tree_says_why_it_proves_nothing() {
    let Some(dir) = scratch_repo("dirty-history") else {
        return;
    };
    // An untracked file makes every run dirty — the ordinary state of a
    // machine someone is working on.
    std::fs::write(dir.join("scratch.txt"), "work in progress").expect("write");
    let report = dir.outside("report.xml");
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
    let report = dir.outside("report.xml");

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
}

#[test]
fn an_abbreviated_commit_is_stored_as_the_full_sha_and_an_unknown_one_is_refused() {
    let Some(dir) = scratch_repo("import-short-sha") else {
        return;
    };
    let full = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git");
    let full = String::from_utf8(full.stdout)
        .expect("utf8")
        .trim()
        .to_owned();
    let report = dir.outside("ci.xml");
    std::fs::write(
        &report,
        r#"<testsuite><testcase classname="c" name="t"><failure/></testcase></testsuite>"#,
    )
    .expect("write report");
    let import = |commit: &str| {
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args([
                "import", "--env", "ci", "--commit", commit, "--color", "never",
            ])
            .arg(&report)
            .output()
            .expect("sooth should run")
    };

    // What every CI screen shows — must combine with local observations.
    let output = import(full.get(..7).expect("a full sha is 40 hex characters"));
    assert_eq!(output.status.code(), Some(0));
    let history = std::fs::read_to_string(dir.join(".sooth/history.jsonl")).expect("history");
    assert!(
        history.contains(&format!(r#""commit":"{full}""#)),
        "the observation must carry the full sha, got: {history:?}"
    );

    let output = import("deadbee");
    assert_eq!(
        output.status.code(),
        Some(2),
        "a sha that resolves to nothing must be refused, not stored"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("`--commit deadbee` names no commit"),
        "got: {stderr:?}"
    );

    std::fs::remove_dir_all(dir.join(".sooth")).expect("reset history");
    let output = import(&full.to_ascii_uppercase());
    assert_eq!(output.status.code(), Some(0));
    let history = std::fs::read_to_string(dir.join(".sooth/history.jsonl")).expect("history");
    assert!(
        history.contains(&format!(r#""commit":"{full}""#)),
        "a full sha in capitals must be stored as local runs record it, got: {history:?}"
    );

    // A branch resolves to where it points now, not to the commit CI ran on.
    let output = import("HEAD");
    assert_eq!(
        output.status.code(),
        Some(2),
        "a revision name must be refused"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("`--commit HEAD` is not a sha"),
        "got: {stderr:?}"
    );
}

#[test]
fn an_import_made_with_the_wrong_label_can_be_replaced() {
    let Some(dir) = scratch_repo("import-replace") else {
        return;
    };
    std::fs::write(
        dir.join("ci-report.xml"),
        r#"<testsuite><testcase classname="c" name="wob"><failure/></testcase></testsuite>"#,
    )
    .expect("write artifact");
    let import = |extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args(["import", "--color", "never"])
            .args(extra)
            .arg("ci-report.xml")
            .output()
            .expect("sooth should run");
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("stdout should be UTF-8"),
        )
    };
    let history = || std::fs::read_to_string(dir.join(".sooth/history.jsonl")).expect("history");

    let (code, stdout) = import(&["--env", "staging"]);
    assert_eq!(code, Some(0), "got: {stdout:?}");
    assert!(history().contains(r#""env":"staging""#));

    // The same file under the right label: skipped, and told how.
    let (_, stdout) = import(&["--env", "ci"]);
    assert!(
        stdout.contains("skipped (already imported)") && stdout.contains("--replace"),
        "got: {stdout:?}"
    );
    assert!(!history().contains(r#""env":"ci""#), "nothing changed yet");

    let (code, stdout) = import(&["--env", "ci", "--replace"]);
    assert_eq!(code, Some(0), "got: {stdout:?}");
    assert!(
        stdout.contains("ci-report.xml: 1 observation, replacing its earlier import"),
        "got: {stdout:?}"
    );
    assert!(
        stdout.contains("history now holds 1 observation"),
        "the wrong observation is gone, not joined: {stdout:?}"
    );
    let recorded = history();
    assert!(
        recorded.contains(r#""env":"ci""#) && !recorded.contains(r#""env":"staging""#),
        "got: {recorded:?}"
    );
    let ledger = std::fs::read_to_string(dir.join(".sooth/imported")).expect("ledger");
    assert_eq!(
        ledger.lines().count(),
        1,
        "one file, one ledger line: {ledger:?}"
    );
}

#[test]
fn a_file_named_twice_in_one_import_is_one_file() {
    let Some(dir) = scratch_repo("import-twice") else {
        return;
    };
    std::fs::write(
        dir.join("a.xml"),
        r#"<testsuite><testcase classname="c" name="one"/><testcase classname="c" name="two"/></testsuite>"#,
    )
    .expect("write");
    let import = |extra: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args(["import", "--color", "never", "--env", "ci"])
            .args(extra)
            .args(["a.xml", "a.xml"])
            .output()
            .expect("sooth should run");
        (
            String::from_utf8(output.stdout).expect("stdout should be UTF-8"),
            String::from_utf8(output.stderr).expect("stderr should be UTF-8"),
        )
    };

    // Under --replace the second copy used to be taken for a re-import.
    for extra in [&["--replace"][..], &["--replace"][..]] {
        let (stdout, stderr) = import(extra);
        assert!(
            stdout.contains("a.xml: skipped (same content as an earlier file of this import)"),
            "got: {stdout:?}"
        );
        assert!(
            stdout.contains("history now holds 2 observations"),
            "two tests, imported once: {stdout:?}"
        );
        assert!(stderr.is_empty(), "nothing to explain: {stderr:?}");
    }
}

#[test]
fn replacing_an_import_whose_history_is_gone_is_a_plain_import() {
    let Some(dir) = scratch_repo("import-replace-no-history") else {
        return;
    };
    std::fs::write(
        dir.join("a.xml"),
        r#"<testsuite><testcase classname="c" name="one"/></testsuite>"#,
    )
    .expect("write");
    let import = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
            .args(["import", "--color", "never", "--env", "ci"])
            .args(extra)
            .arg("a.xml")
            .output()
            .expect("sooth should run")
    };
    assert_eq!(import(&[]).status.code(), Some(0));
    std::fs::remove_file(dir.join(".sooth/history.jsonl")).expect("remove history");

    let output = import(&["--replace"]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(
        stdout.contains("history now holds 1 observation"),
        "got: {stdout:?}"
    );
    assert!(
        stderr.is_empty(),
        "a missing history is not an old sooth's doing: {stderr:?}"
    );
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
    let report = dir.outside("report.xml");
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

    // An import is all or nothing, and so is what it says: a green log
    // that is followed by a refused one was not imported either.
    let (cwd_both, mut command) = sooth_in("log-green-then-foreign");
    std::fs::write(
        cwd_both.join("green.log"),
        "....\nOK (4 tests, 8 assertions)\n",
    )
    .expect("write");
    std::fs::write(cwd_both.join("pytest.log"), "collected 3 items\n3 passed\n").expect("write");
    let output = command
        .args([
            "import",
            "--log",
            "phpunit",
            "--env",
            "ci",
            "green.log",
            "pytest.log",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stdout, "",
        "nothing is said of an import that did not happen"
    );
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
}

/// History lines for `id`: one timed pass per invocation, `runs` of them on
/// `commit` at `ms` each, local and clean. `first` numbers the invocations.
fn timed_lines(id: &str, commit: &str, ms: u64, first: u64, runs: u64) -> String {
    (first..first + runs)
        .map(|run| {
            format!(
                r#"{{"at":{run},"commit":"{commit}","dirty":false,"env":"local","run":"r{run}","ms":{ms},"status":"passed","id":"{id}"}}"#
            ) + "\n"
        })
        .collect()
}

#[test]
fn a_test_that_takes_longer_since_a_commit_is_named_with_the_commit() {
    let (cwd, mut command) = sooth_in("history-slower-since");
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    // Three runs at 15 ms, then five at 284 ms from commit bbb on; a second
    // test that got slower by too little to say, and one that did not move.
    let history = [
        timed_lines("c::export", "aaa", 15, 1, 3),
        timed_lines("c::export", "bbb1234def", 284, 4, 5),
        timed_lines("c::invoice", "aaa", 2, 1, 3),
        timed_lines("c::invoice", "bbb1234def", 15, 4, 5),
        timed_lines("c::steady", "aaa", 1200, 1, 3),
        timed_lines("c::steady", "bbb1234def", 1250, 4, 5),
    ]
    .concat();
    std::fs::write(cwd.join(".sooth/history.jsonl"), history).expect("write");

    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "slower is no failure");
    assert!(
        stdout.contains(
            "slower since bbb1234 (local) — 1 test of the 2 that take 20ms or more:\n  \
             ▲ c::export 15ms → 284ms (3 runs before, 5 since)\n"
        ),
        "got: {stdout:?}"
    );
    assert!(
        !stdout.contains("c::invoice"),
        "13 ms is not said: {stdout:?}"
    );
    assert!(!stdout.contains("c::steady"), "got: {stdout:?}");
    assert!(!stdout.contains("nothing stands out"), "got: {stdout:?}");
}

#[test]
fn many_tests_slower_at_one_commit_are_said_to_be_many_and_shown_in_part() {
    let (cwd, mut command) = sooth_in("history-slower-many");
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    // Twelve tests a hundred times as slow from one commit on, each by a
    // little more than the one before it, among twenty that did not move:
    // the typical test is as fast as it was, and twelve of thirty-two is a
    // lot of tests to change at once. A debugger that traces PHP and
    // leaves a call into C alone looks like this.
    let slowed = (1..=12_u64).map(|test| {
        let id = format!("c::t{test:02}");
        timed_lines(&id, "aaa", 10, 1, 3) + &timed_lines(&id, "bbb1234def", 1000 + test, 4, 3)
    });
    let steady = (1..=20_u64).map(|test| {
        let id = format!("c::s{test:02}");
        timed_lines(&id, "aaa", 50, 1, 3) + &timed_lines(&id, "bbb1234def", 50, 4, 3)
    });
    let history: String = slowed.chain(steady).collect();
    std::fs::write(cwd.join(".sooth/history.jsonl"), history).expect("write");

    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert!(
        stdout.contains(
            "slower since bbb1234 (local) — 12 tests of the 32 that take 20ms or more — that \
             many at once can as well be the machine, a setting or something the tests \
             share: sooth reads the clock, not the cause. The 3 that gained most:\n"
        ),
        "got: {stdout:?}"
    );
    // Three of them stand for the rest, the one that gained most first.
    let shown: Vec<&str> = stdout.lines().filter(|line| line.contains("▲ ")).collect();
    assert_eq!(shown.len(), 3, "got: {stdout:?}");
    assert!(
        shown[0].contains("c::t12 10ms → 1.0s (3 runs before, 3 since)")
            && shown[2].contains("c::t10 "),
        "got: {stdout:?}"
    );
    assert!(stdout.contains("  … and 9 more\n"), "got: {stdout:?}");
}

#[test]
fn tests_that_got_slower_with_the_rest_of_the_suite_are_one_line_and_no_name() {
    let (cwd, mut command) = sooth_in("history-slower-withheld");
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    // Six tests, each 2.6 times as long from one commit on: what a laptop
    // on battery does to a suite, and what a slower fixture they all use
    // does. Not six findings.
    let history: String = (1..=6_u64)
        .map(|test| {
            let id = format!("c::t{test}");
            timed_lines(&id, "aaa", 100 * test, 1, 3)
                + &timed_lines(&id, "bbb1234def", 260 * test, 4, 3)
        })
        .collect();
    std::fs::write(cwd.join(".sooth/history.jsonl"), history).expect("write");

    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout.contains(
            "slower since bbb1234 (local) — 6 tests by their own clock, not named: over the \
             same runs, a quarter of the other tests that took 20ms or more before took 2.6 \
             times as long or more, read from at least 5 of them. The machine, a setting or \
             something the tests share.\n"
        ),
        "got: {stdout:?}"
    );
    assert!(!stdout.contains('▲'), "got: {stdout:?}");
    assert!(!stdout.contains("nothing stands out"), "got: {stdout:?}");
}

#[test]
fn a_history_in_which_nothing_got_slower_says_so_among_the_rest() {
    let (cwd, mut command) = sooth_in("history-nothing-slower");
    std::fs::create_dir_all(cwd.join(".sooth")).expect("mkdir");
    std::fs::write(
        cwd.join(".sooth/history.jsonl"),
        timed_lines("c::steady", "aaa", 40, 1, 3) + &timed_lines("c::steady", "bbb", 44, 4, 3),
    )
    .expect("write");

    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert!(
        stdout.contains(
            "nothing stands out: no proven flakes, no failing-since pointers, no \
             slower-since pointers"
        ),
        "got: {stdout:?}"
    );
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
}

#[test]
fn an_unlisted_known_flake_under_fail_on_flaky_explains_the_gap() {
    let Some(dir) = scratch_repo("pardon-gap") else {
        return;
    };
    let report = dir.outside("report.xml");
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
}

#[test]
fn an_empty_history_says_so_instead_of_printing_nothing() {
    let (_cwd, mut command) = sooth_in("history-empty");
    let output = command
        .args(["history", "--color", "never"])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("the history is empty"), "got: {stdout:?}");
}

#[test]
fn the_gate_needs_a_preset_and_a_real_runs_count() {
    let (_cwd, mut command) = sooth_in("gate-reject");
    let output = command
        .args(["run", "--changed", "--runs", "5", "--", "true"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("needs `--preset"), "got: {stderr:?}");

    let (_cwd_single, mut command) = sooth_in("gate-one-run");
    let output = command
        .args(["run", "--changed", "--preset", "phpunit", "--", "true"])
        .output()
        .expect("sooth should run");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("20 is a good start"), "got: {stderr:?}");
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
}

#[test]
fn the_gate_refuses_a_command_that_brings_its_own_selection() {
    // `./runner.sh tests/` plus the gate's files is the union: all of
    // `tests/`, `--runs` times, under a line that names one changed file.
    let Some(dir) = scratch_repo("gate-own-selection") else {
        return;
    };
    write_gate_runner(&dir);
    std::fs::create_dir_all(dir.join("tests")).expect("mkdir");
    std::fs::write(dir.join("tests/NewTest.php"), "<?php\n").expect("write");
    let gate = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(&dir)
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
            .args(extra)
            .output()
            .expect("sooth should run")
    };

    let output = gate(&["tests/", "--stop-on-failure"]);
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert!(
        stderr.contains("already selects `tests/`"),
        "the refusal names what the command selects: {stderr:?}"
    );
    assert!(!dir.join("ran-tests").exists(), "nothing may have run");

    // An option's value is not a selection, even when it is a path.
    let output = gate(&["--bootstrap", "tests/NewTest.php"]);
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert_eq!(output.status.code(), Some(0), "got: {stderr:?}");
    assert!(dir.join("ran-tests").exists(), "the gate must have run");
}

/// A one-file gate of the phpunit preset over a file called `name`, with a
/// runner that notes its arguments in `ran-with`: (stdout, the arguments).
fn gate_one_file_named(tag: &str, name: &str) -> Option<(String, String)> {
    let dir = scratch_repo(tag)?;
    write_named_runner(&dir, "phpunit", "--log-junit", "--bootstrap=*");
    std::fs::write(dir.join(name), "<?php\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
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
            "./phpunit",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let ran_with = std::fs::read_to_string(dir.join("ran-with")).unwrap_or_default();
    Some((stdout, ran_with))
}

#[test]
fn the_gate_selects_a_test_file_whose_name_git_would_quote() {
    // Without -z, git C-quotes a path holding a quote or a backslash; the
    // quoted form is no test file, so it silently left the selection.
    for name in ["Wei\"rdTest.php", "Back\\slashTest.php"] {
        let Some((stdout, ran_with)) = gate_one_file_named("gate-quoted", name) else {
            return;
        };
        assert!(
            stdout.contains("gate: 1 changed test file against HEAD"),
            "`{name}` must be gated: {stdout:?}"
        );
        assert!(
            ran_with.ends_with(&format!("./{name}\n")),
            "the runner gets the path as it is on disk: {ran_with:?}"
        );
    }
}

#[test]
fn the_gate_hands_the_runner_paths_never_options() {
    // A test file may be called anything. Unanchored, this one reaches the
    // runner as an option: `--bootstrap=…` runs code before any test.
    let Some((stdout, ran_with)) = gate_one_file_named("gate-dash", "--bootstrap=payloadTest.php")
    else {
        return;
    };
    assert!(
        stdout.contains("✓ PASSED"),
        "the runner must have taken it as a path: {stdout:?}"
    );
    assert!(
        ran_with.ends_with("./--bootstrap=payloadTest.php\n"),
        "got: {ran_with:?}"
    );
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
}

#[test]
fn a_pest_gate_reads_the_phpunit_version_pest_runs_on() {
    use std::os::unix::fs::PermissionsExt;
    let Some(dir) = scratch_repo("gate-pest") else {
        return;
    };
    // Pest prints its own banner; the PHPUnit it runs on is its sibling.
    let bin = dir.join("vendor/bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gate_runner.sh");
    std::fs::copy(fixture, bin.join("pest")).expect("copy runner");
    std::fs::write(
        bin.join("phpunit"),
        "#!/bin/sh\necho 'PHPUnit 12.5.33 by Sebastian Bergmann and contributors.'\n",
    )
    .expect("write phpunit");
    for name in ["pest", "phpunit"] {
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    std::fs::write(dir.join("ATest.php"), "<?php\n").expect("write");
    std::fs::write(dir.join("BTest.php"), "<?php\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .env(
            "SOOTH_TEST_PHPUNIT_VERSION",
            "\n  Pest Testing Framework 4.7.8.\n",
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
            "vendor/bin/pest",
        ])
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stderr:?}");
    assert!(
        !stderr.contains("could not read a PHPUnit version"),
        "Pest's PHPUnit is next to it, so there was nothing to warn about: {stderr:?}"
    );
    assert!(dir.join("ran-tests").exists(), "the gate must have run");
}

#[test]
fn php_artisan_test_gets_the_reporter_flags_after_its_subcommand() {
    use std::os::unix::fs::PermissionsExt;
    let Some(dir) = scratch_repo("artisan") else {
        return;
    };
    // A fake `artisan` that, like Laravel, only forwards options that come
    // after `test`: anything in front of the subcommand is a usage error.
    std::fs::write(
        dir.join("artisan"),
        concat!(
            "#!/bin/sh\n",
            "[ \"$1\" = test ] || { echo \"artisan: unknown command $1\" >&2; exit 64; }\n",
            "shift; report=\"\"; prev=\"\"\n",
            "for a in \"$@\"; do\n",
            "  if [ \"$prev\" = \"--log-junit\" ]; then report=\"$a\"; fi\n",
            "  prev=\"$a\"\n",
            "done\n",
            "touch ran-tests\n",
            "printf '<testsuite><testcase classname=\"a\" name=\"ok\"/></testsuite>' > \"$report\"\n",
        ),
    )
    .expect("write artisan");
    let bin = dir.join("vendor/bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    std::fs::write(
        bin.join("phpunit"),
        "#!/bin/sh\necho 'PHPUnit 12.5.33 by Sebastian Bergmann and contributors.'\n",
    )
    .expect("write phpunit");
    for path in [dir.join("artisan"), bin.join("phpunit")] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    std::fs::write(dir.join("ATest.php"), "<?php\n").expect("write");
    std::fs::write(dir.join("BTest.php"), "<?php\n").expect("write");

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
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
            "./artisan",
            "test",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stdout:?} {stderr:?}");
    assert!(stdout.contains("2 of 2 runs"), "got: {stdout:?}");
    assert!(
        !stderr.contains("could not read a PHPUnit version"),
        "the project's vendor/bin/phpunit names the version: {stderr:?}"
    );
}

/// A fake `composer` in `dir` that, like Composer, reads every option in
/// front of `--` as its own and hands what follows `--` to the script — here
/// `first_command`, then a runner that notes its arguments in `runner-args`.
fn write_fake_composer(dir: &std::path::Path, first_command: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(
        dir.join("composer"),
        format!(
            concat!(
                "#!/bin/sh\n",
                "script=\"\"\n",
                "while [ $# -gt 0 ]; do\n",
                "  case \"$1\" in\n",
                "    --) shift; break ;;\n",
                "    -*) echo \"The \\\"$1\\\" option does not exist.\" >&2; exit 1 ;;\n",
                "    *) [ -z \"$script\" ] || {{ echo \"composer: stray argument $1\" >&2; exit 1; }}\n",
                "       script=\"$1\"; shift ;;\n",
                "  esac\n",
                "done\n",
                "[ \"$script\" = test ] || {{ echo \"Command \\\"$script\\\" is not defined.\" >&2; exit 1; }}\n",
                "{first_command} \"$@\" || exit 1\n",
                "exec ./runner \"$@\"\n",
            ),
            first_command = first_command
        ),
    )
    .expect("write composer");
    std::fs::write(
        dir.join("runner"),
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' \"$@\" > runner-args\n",
            "report=\"\"; prev=\"\"\n",
            "for a in \"$@\"; do\n",
            "  if [ \"$prev\" = \"--log-junit\" ]; then report=\"$a\"; fi\n",
            "  prev=\"$a\"\n",
            "done\n",
            "printf '<testsuite><testcase classname=\"a\" name=\"ok\"/></testsuite>' > \"$report\"\n",
        ),
    )
    .expect("write runner");
    let bin = dir.join("vendor/bin");
    std::fs::create_dir_all(&bin).expect("mkdir");
    std::fs::write(
        bin.join("phpunit"),
        "#!/bin/sh\necho 'PHPUnit 12.5.33 by Sebastian Bergmann and contributors.'\n",
    )
    .expect("write phpunit");
    for path in [
        dir.join("composer"),
        dir.join("runner"),
        bin.join("phpunit"),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

#[test]
fn a_composer_script_gets_the_reporter_flags_after_a_double_dash() {
    let Some(dir) = scratch_repo("composer") else {
        return;
    };
    // `true` stands for a first command marked `@no_additional_args`.
    write_fake_composer(&dir, "true ||");
    std::fs::write(dir.join("ATest.php"), "<?php\n").expect("write");
    std::fs::write(dir.join("BTest.php"), "<?php\n").expect("write");

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
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
            "./composer",
            "test",
        ])
        .output()
        .expect("sooth should run");
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stdout:?} {stderr:?}");
    assert!(stdout.contains("2 of 2 runs"), "got: {stdout:?}");
    let runner_args = std::fs::read_to_string(dir.join("runner-args")).expect("the runner ran");
    assert!(
        runner_args.starts_with("--log-junit\n")
            && runner_args.ends_with("./ATest.php\n./BTest.php\n"),
        "the script gets the flags, then the gate's files: {runner_args:?}"
    );
    assert!(
        !stderr.contains("could not read a PHPUnit version"),
        "the project's vendor/bin/phpunit names the version: {stderr:?}"
    );
}

#[test]
fn a_composer_script_that_cannot_take_the_flags_is_told_what_composer_does() {
    let Some(dir) = scratch_repo("composer-every-command") else {
        return;
    };
    // The first command of the script gets the arguments too, and refuses
    // them: Composer appends them to every command of a script.
    write_fake_composer(&dir, "false");

    let output = Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(&dir)
        .args([
            "run",
            "--no-history",
            "--preset",
            "phpunit",
            "--color",
            "never",
            "--",
            "./composer",
            "test",
        ])
        .output()
        .expect("sooth should run");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
    assert!(
        stderr.contains("wrote no JUnit-XML report") && stderr.contains("@no_additional_args"),
        "got: {stderr:?}"
    );
    assert!(
        !stderr.contains("the test runner itself"),
        "sooth knows `composer` is not the runner: {stderr:?}"
    );
}

#[test]
fn a_gate_of_several_files_that_got_no_report_is_pointed_at_paratest() {
    // A `--parallel` inside a script cannot be seen up front; when such a
    // gate comes back empty-handed, sooth names the one thing it knows.
    let Some(dir) = scratch_repo("gate-hidden-parallel") else {
        return;
    };
    write_fake_composer(&dir, "false");
    std::fs::write(dir.join("ATest.php"), "<?php\n").expect("write");
    std::fs::write(dir.join("BTest.php"), "<?php\n").expect("write");
    let gate = |dir: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_sooth"))
            .current_dir(dir)
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
                "./composer",
                "test",
            ])
            .output()
            .expect("sooth should run")
    };
    let stderr = String::from_utf8(gate(&dir).stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("the gate passed 2 test files") && stderr.contains("paratest"),
        "got: {stderr:?}"
    );

    // One file is a path paratest takes: nothing to point at.
    std::fs::remove_file(dir.join("BTest.php")).expect("remove");
    let stderr = String::from_utf8(gate(&dir).stderr).expect("stderr should be UTF-8");
    assert!(
        stderr.contains("wrote no JUnit-XML report") && !stderr.contains("paratest"),
        "got: {stderr:?}"
    );
}

/// A gate invocation in `dir`, where the fixture runner stands in for
/// `./artisan`, with `extra` after `./artisan test`.
fn gate_under_artisan(dir: &std::path::Path, extra: &[&str]) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    // Laravel's `artisan test`: the subcommand, then what it forwards.
    std::fs::write(
        dir.join("artisan"),
        "#!/bin/sh\nshift\nexec ./runner.sh \"$@\"\n",
    )
    .expect("write artisan");
    std::fs::set_permissions(dir.join("artisan"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    write_gate_runner(dir);
    Command::new(env!("CARGO_BIN_EXE_sooth"))
        .current_dir(dir)
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
            "./artisan",
            "test",
        ])
        .args(extra)
        .output()
        .expect("sooth should run")
}

#[test]
fn the_gate_refuses_a_parallel_command_before_anything_runs() {
    // paratest takes a single path: two changed files would end in its
    // "Too many arguments" and a gate that blocks the push without a reason.
    for flag in ["--parallel", "-p"] {
        let Some(dir) = scratch_repo("gate-parallel") else {
            return;
        };
        std::fs::write(dir.join("ATest.php"), "<?php\n").expect("write");
        std::fs::write(dir.join("BTest.php"), "<?php\n").expect("write");
        let output = gate_under_artisan(&dir, &[flag]);
        let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

        assert_eq!(output.status.code(), Some(2), "got: {stderr:?}");
        assert!(
            stderr.contains(&format!("Drop `{flag}`")) && stderr.contains("single path"),
            "the refusal names the flag and the reason: {stderr:?}"
        );
        assert!(!dir.join("ran-tests").exists(), "nothing may have run");
    }
}

#[test]
fn the_gate_refuses_a_parallel_command_even_when_nothing_changed() {
    // A hook that works until the day two test files change fails at the
    // worst moment; one that is refused on its first push gets fixed once.
    let Some(dir) = scratch_repo("gate-parallel-empty") else {
        return;
    };
    let output = gate_under_artisan(&dir, &["--parallel"]);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(2), "got: {stdout:?} {stderr:?}");
    assert!(stderr.contains("Drop `--parallel`"), "got: {stderr:?}");
    assert!(!stdout.contains("nothing to prove"), "got: {stdout:?}");
}

#[test]
fn the_same_gate_without_parallel_runs() {
    let Some(dir) = scratch_repo("gate-serial") else {
        return;
    };
    std::fs::write(dir.join("ATest.php"), "<?php\n").expect("write");
    std::fs::write(dir.join("BTest.php"), "<?php\n").expect("write");
    let output = gate_under_artisan(&dir, &["--stop-on-failure"]);
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");

    assert_eq!(output.status.code(), Some(0), "got: {stderr:?}");
    assert!(dir.join("ran-tests").exists(), "the gate must have run");
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
}

#[test]
fn no_history_neither_writes_nor_reports() {
    let Some(dir) = scratch_repo("nohistory") else {
        return;
    };
    let report = dir.outside("report.xml");
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
}
