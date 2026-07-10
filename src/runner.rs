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

/// Run `command` `runs` times in a fixed order, inheriting stdio so the test
/// output stays visible, and record each run's exit status and wall-time.
/// `envs` is added to the child's environment (presets use it to point
/// reporters at the report file).
///
/// Runs happen in a fixed order on purpose: flaky detection (v0.2) repeats in a
/// fixed order, while order-dependence detection shuffles — never both at once
/// (see `DECISIONS.md`).
///
/// # Errors
///
/// Returns the underlying I/O error if the command cannot be spawned (for
/// example when the program is not found on `PATH`).
pub fn run(
    command: &[String],
    runs: u32,
    envs: &[(String, String)],
) -> std::io::Result<Vec<RunOutcome>> {
    let (program, rest) = command
        .split_first()
        .expect("clap guarantees the command has at least one element");

    let mut outcomes = Vec::with_capacity(runs as usize);
    for _ in 0..runs {
        let start = Instant::now();
        let status = Command::new(program)
            .args(rest)
            .envs(envs.iter().map(|(key, value)| (key, value)))
            .status()?;
        outcomes.push(RunOutcome {
            exit_code: status.code(),
            signal: signal_of(status),
            success: status.success(),
            duration: start.elapsed(),
        });
    }
    Ok(outcomes)
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
    use super::run;

    // `true`, `false`, `sh` and signals only exist on Unix; the runner itself
    // is portable. Revisit when the CI matrix grows beyond Linux (ROADMAP.md).
    #[cfg(unix)]
    #[test]
    fn records_success_for_a_zero_exit_command() {
        let outcomes = run(&["true".to_owned()], 2, &[]).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|outcome| outcome.success));
        assert_eq!(outcomes[0].exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn records_failure_for_a_nonzero_exit_command() {
        let outcomes = run(&["false".to_owned()], 1, &[]).unwrap();
        assert!(!outcomes[0].success);
        assert_eq!(outcomes[0].exit_code, Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn records_the_signal_when_the_process_is_killed() {
        let command = ["sh", "-c", "kill -TERM $$"].map(String::from);
        let outcomes = run(&command, 1, &[]).unwrap();
        assert!(!outcomes[0].success);
        assert_eq!(outcomes[0].exit_code, None);
        assert_eq!(outcomes[0].signal, Some(15)); // SIGTERM
    }

    #[cfg(unix)]
    #[test]
    fn passes_environment_variables_to_the_child() {
        let command = ["sh", "-c", r#"test "$SOOTH_TEST_ENV" = value"#].map(String::from);
        let envs = [("SOOTH_TEST_ENV".to_owned(), "value".to_owned())];
        let outcomes = run(&command, 1, &envs).unwrap();
        assert!(outcomes[0].success);
    }

    #[test]
    fn errors_when_the_program_cannot_be_spawned() {
        let result = run(&["sooth-no-such-binary-xyzzy".to_owned()], 1, &[]);
        assert!(result.is_err());
    }
}
