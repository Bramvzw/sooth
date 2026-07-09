//! Runs the test command as a subprocess and records how each run went.

use std::process::Command;
use std::time::{Duration, Instant};

/// The outcome of a single execution of the test command.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Process exit code, or `None` if the process was terminated by a signal.
    pub exit_code: Option<i32>,
    /// Whether the process exited successfully (exit code `0`).
    pub success: bool,
    /// Wall-clock time the run took.
    pub duration: Duration,
}

/// Run `command` `runs` times in a fixed order, inheriting stdio so the test
/// output stays visible, and record each run's exit status and wall-time.
///
/// Runs happen in a fixed order on purpose: flaky detection (v0.2) repeats in a
/// fixed order, while order-dependence detection shuffles — never both at once
/// (see `DECISIONS.md`).
///
/// # Errors
///
/// Returns the underlying I/O error if the command cannot be spawned (for
/// example when the program is not found on `PATH`).
pub fn run(command: &[String], runs: u32) -> std::io::Result<Vec<RunOutcome>> {
    let (program, rest) = command
        .split_first()
        .expect("clap guarantees the command has at least one element");

    let mut outcomes = Vec::with_capacity(runs as usize);
    for _ in 0..runs {
        let start = Instant::now();
        let status = Command::new(program).args(rest).status()?;
        outcomes.push(RunOutcome {
            exit_code: status.code(),
            success: status.success(),
            duration: start.elapsed(),
        });
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn records_success_for_a_zero_exit_command() {
        let outcomes = run(&["true".to_owned()], 2).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|outcome| outcome.success));
        assert_eq!(outcomes[0].exit_code, Some(0));
    }

    #[test]
    fn records_failure_for_a_nonzero_exit_command() {
        let outcomes = run(&["false".to_owned()], 1).unwrap();
        assert!(!outcomes[0].success);
        assert_eq!(outcomes[0].exit_code, Some(1));
    }

    #[test]
    fn errors_when_the_program_cannot_be_spawned() {
        let result = run(&["sooth-no-such-binary-xyzzy".to_owned()], 1);
        assert!(result.is_err());
    }
}
