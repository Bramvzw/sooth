//! Runs the test command as a subprocess and records how each run went.

use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

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

/// Run `command` exactly once, inheriting stdio so the test output stays
/// visible, and record the exit status and wall-time. `envs` is added to the
/// child's environment (presets use it to point reporters at the report
/// file).
///
/// Exactly once on purpose: spawning and measuring is this module's whole
/// job. *Repetition* is an orchestration strategy that lives with the caller
/// — flaky detection (v0.2) repeats in a fixed order and parses the report
/// per run, order-dependence detection (v0.3) runs different shuffled
/// invocations (see `DECISIONS.md`).
///
/// # Errors
///
/// Returns the underlying I/O error if the command cannot be spawned (for
/// example when the program is not found on `PATH`).
pub fn run_once(command: &[String], envs: &[(String, String)]) -> std::io::Result<RunOutcome> {
    let (program, rest) = command
        .split_first()
        .expect("clap guarantees the command has at least one element");

    let start = Instant::now();
    let status = Command::new(program)
        .args(rest)
        .envs(envs.iter().map(|(key, value)| (key, value)))
        .status()?;
    Ok(RunOutcome {
        exit_code: status.code(),
        signal: signal_of(status),
        success: status.success(),
        duration: start.elapsed(),
    })
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
    use super::run_once;

    // `true`, `false`, `sh` and signals only exist on Unix; the runner itself
    // is portable. Revisit when the CI matrix grows beyond Linux (ROADMAP.md).
    #[cfg(unix)]
    #[test]
    fn records_success_for_a_zero_exit_command() {
        let outcome = run_once(&["true".to_owned()], &[]).unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn records_failure_for_a_nonzero_exit_command() {
        let outcome = run_once(&["false".to_owned()], &[]).unwrap();
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn records_the_signal_when_the_process_is_killed() {
        let command = ["sh", "-c", "kill -TERM $$"].map(String::from);
        let outcome = run_once(&command, &[]).unwrap();
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.signal, Some(15)); // SIGTERM
    }

    #[cfg(unix)]
    #[test]
    fn passes_environment_variables_to_the_child() {
        let command = ["sh", "-c", r#"test "$SOOTH_TEST_ENV" = value"#].map(String::from);
        let envs = [("SOOTH_TEST_ENV".to_owned(), "value".to_owned())];
        let outcome = run_once(&command, &envs).unwrap();
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
        let result = run_once(&["sooth-no-such-binary-xyzzy".to_owned()], &[]);
        assert!(result.is_err());
    }
}
