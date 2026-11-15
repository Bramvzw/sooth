//! Runs the test command as a subprocess and records how each run went.

use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use crate::interrupt::Interrupt;

/// How often a running command is looked at: often enough that the measured
/// wall time is the command's, seldom enough to cost nothing.
const POLL: Duration = Duration::from_millis(1);

/// How long a command gets to stop by itself once sooth was asked to stop,
/// before sooth kills it. At a terminal the command got the Ctrl-C too and
/// is already winding down — `PHPUnit` prints what it has, pytest its
/// summary — so this passes unused. When only sooth was signalled (CI, a
/// timeout wrapper), the command knows nothing and this is when it ends.
const GRACE: Duration = Duration::from_secs(2);

/// The outcome of a single execution of the test command.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Process exit code, or `None` if the process was terminated by a signal.
    pub exit_code: Option<i32>,
    /// Signal that terminated the process, if any (always `None` off Unix).
    pub signal: Option<i32>,
    /// Whether the process exited successfully (exit code `0`).
    pub success: bool,
    /// Wall-clock time the run took.
    pub duration: Duration,
}

impl RunOutcome {
    /// The runner's own status, always labeled `runner …`: a bare `exit=2`
    /// reads as sooth's exit-code contract (where 2 means "sooth itself
    /// failed") — two vocabularies that must stay distinct. Both the per-run
    /// lines and the crash context print through this one definition.
    pub fn status_label(&self) -> String {
        match (self.exit_code, self.signal) {
            (Some(code), _) => format!("runner exit={code}"),
            (None, Some(signal)) => format!("runner signal {signal}"),
            (None, None) => "runner killed by signal".to_owned(),
        }
    }
}

/// How one execution of the command ended.
#[derive(Debug, Clone)]
pub enum Execution {
    /// The command ran to its own end; this is how.
    Finished(RunOutcome),
    /// Sooth was asked to stop before it saw the command end. However the
    /// command exited then — by itself, of the same signal, or killed
    /// after the grace — its tests did not decide that.
    CutShort,
}

/// Run `command` exactly once, inheriting stdio so the test output stays
/// visible, and record the exit status and wall-time. `envs` is added to the
/// child's environment (presets use it to point reporters at the report
/// file).
///
/// Exactly once on purpose: spawning and measuring is this module's whole
/// job. *Repetition* is an orchestration strategy that lives with the caller
/// — flaky detection (v0.2) repeats in a fixed order and parses the report
/// per run, order-dependence detection (v0.4) runs different shuffled
/// invocations (see `DECISIONS.md`).
///
/// # Errors
///
/// Returns the underlying I/O error if the command cannot be spawned (for
/// example when the program is not found on `PATH`).
pub fn run_once(
    command: &[String],
    envs: &[(String, String)],
    interrupt: &Interrupt,
) -> std::io::Result<Execution> {
    spawn(command, envs, None, interrupt)
}

/// [`run_once`], with the child's stdout and stderr written to `capture`
/// instead of this terminal — one file for both, so the two streams stay in
/// the order the child wrote them.
///
/// # Errors
///
/// As [`run_once`], and when the file's handle cannot be shared between
/// the two streams.
pub fn run_captured(
    command: &[String],
    envs: &[(String, String)],
    capture: std::fs::File,
    interrupt: &Interrupt,
) -> std::io::Result<Execution> {
    spawn(command, envs, Some(capture), interrupt)
}

#[expect(
    clippy::disallowed_methods,
    reason = "Spawning the wrapped test command is what this module is for."
)]
fn spawn(
    command: &[String],
    envs: &[(String, String)],
    capture: Option<std::fs::File>,
    interrupt: &Interrupt,
) -> std::io::Result<Execution> {
    let (program, rest) = command
        .split_first()
        .expect("clap guarantees the command has at least one element");
    let mut child = Command::new(program);
    child
        .args(rest)
        .envs(envs.iter().map(|(key, value)| (key, value)));
    if let Some(file) = capture {
        child.stdout(file.try_clone()?).stderr(file);
    }

    let start = Instant::now();
    let mut child = child.spawn()?;
    // Looked at rather than waited for, so a request to stop is noticed
    // while the command runs. Only the command itself is killed: reaching
    // what it started in turn takes a signal to its process group, which
    // the standard library cannot send.
    let mut asked_at: Option<Instant> = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if interrupt.requested() && asked_at.get_or_insert_with(Instant::now).elapsed() >= GRACE {
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(POLL);
    };
    // Asked the moment the command is seen to have ended, and not later: a
    // request that comes while the run's report is being read finds a run
    // that finished, and that run counts.
    if interrupt.requested() {
        return Ok(Execution::CutShort);
    }
    Ok(Execution::Finished(RunOutcome {
        exit_code: status.code(),
        signal: signal_of(status),
        success: status.success(),
        duration: start.elapsed(),
    }))
}

/// The signal that terminated the process, if any.
fn signal_of(status: ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{run_once, Execution, RunOutcome};
    use crate::interrupt::Interrupt;

    /// The outcome of a command nobody asked to stop.
    fn finished(command: &[String], envs: &[(String, String)]) -> RunOutcome {
        match run_once(command, envs, &Interrupt::never()).unwrap() {
            Execution::Finished(outcome) => outcome,
            Execution::CutShort => panic!("nobody asked this run to stop"),
        }
    }

    // `true`, `false`, `sh` and signals only exist on Unix; the runner itself
    // is portable. Revisit when the CI matrix grows beyond Linux (ROADMAP.md).
    #[cfg(unix)]
    #[test]
    fn records_success_for_a_zero_exit_command() {
        let outcome = finished(&["true".to_owned()], &[]);
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn records_failure_for_a_nonzero_exit_command() {
        let outcome = finished(&["false".to_owned()], &[]);
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn records_the_signal_when_the_process_is_killed() {
        let command = ["sh", "-c", "kill -TERM $$"].map(String::from);
        let outcome = finished(&command, &[]);
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.signal, Some(15)); // SIGTERM
    }

    #[cfg(unix)]
    #[test]
    fn passes_environment_variables_to_the_child() {
        let command = ["sh", "-c", r#"test "$SOOTH_TEST_ENV" = value"#].map(String::from);
        let envs = [("SOOTH_TEST_ENV".to_owned(), "value".to_owned())];
        let outcome = finished(&command, &envs);
        assert!(outcome.success);
    }

    #[test]
    fn status_labels_keep_the_runner_prefix() {
        let base = super::RunOutcome {
            exit_code: Some(2),
            signal: None,
            success: false,
            duration: std::time::Duration::ZERO,
        };
        assert_eq!(base.status_label(), "runner exit=2");
        let signaled = super::RunOutcome {
            exit_code: None,
            signal: Some(15),
            ..base.clone()
        };
        assert_eq!(signaled.status_label(), "runner signal 15");
        let unknown = super::RunOutcome {
            exit_code: None,
            signal: None,
            ..base
        };
        assert_eq!(unknown.status_label(), "runner killed by signal");
    }

    #[test]
    fn errors_when_the_program_cannot_be_spawned() {
        let result = run_once(
            &["sooth-no-such-binary-xyzzy".to_owned()],
            &[],
            &Interrupt::never(),
        );
        assert!(result.is_err());
    }
    #[cfg(unix)]
    #[test]
    fn a_command_that_ignores_the_request_to_stop_is_killed_after_the_grace() {
        // The command was not signalled itself — only sooth was — so it
        // would sleep its thirty seconds out.
        let started = std::time::Instant::now();
        let command = ["sleep".to_owned(), "30".to_owned()];
        let execution = run_once(&command, &[], &Interrupt::already()).unwrap();
        // Back long before the thirty seconds: it was killed, and waited for.
        let waited = started.elapsed();
        assert!(
            waited >= super::GRACE && waited < super::GRACE * 3,
            "after the grace, not before and not much later: {waited:?}"
        );
        assert!(matches!(execution, Execution::CutShort));
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_ends_by_itself_is_not_waited_for_and_does_not_count() {
        // At a terminal the command got the Ctrl-C too and winds down in
        // its own time, well inside the grace. Its exit code then is its
        // answer to the signal, not to its tests.
        let started = std::time::Instant::now();
        let command = ["sh".to_owned(), "-c".to_owned(), "exit 7".to_owned()];
        let execution = run_once(&command, &[], &Interrupt::already()).unwrap();
        assert!(started.elapsed() < super::GRACE, "nothing to sit out");
        assert!(matches!(execution, Execution::CutShort));
    }
}
