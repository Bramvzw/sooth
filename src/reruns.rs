//! What a CI job that failed and passed on its rerun proves (see
//! `DECISIONS.md`): every guard between "the job went red, then green" and
//! "these tests failed and then passed on the same code", checked on the
//! two attempts' logs.

use std::collections::{BTreeMap, BTreeSet};

use crate::ci_log::{self, Cause, Reader, Step, Summary};
use crate::junit::TestStatus;
use crate::{collision_log, phpunit_log};

/// What a red attempt and its green rerun come to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judged {
    /// The tests that failed in the red attempt, by name; each passed in the
    /// green one.
    Named(BTreeMap<String, TestStatus>),
    /// A rerun pair, but a failure the red log does not name: what it does
    /// say about each, or why it could not be read.
    Unnamed { causes: Vec<Cause>, why: String },
    /// Not a rerun pair sooth can vouch for, and why.
    NotAPair(String),
}

/// Judge the red attempt's log against the green one's, for a run of
/// `head_sha`. `known` is what the history knows by name, which a Collision
/// log needs to name its failures.
pub fn judge(head_sha: &str, red: &str, green: &str, known: &BTreeSet<String>) -> Judged {
    let not = |why: &str| Judged::NotAPair(why.to_owned());
    for log in [red, green] {
        let shas = ci_log::checkout_shas(log);
        if shas.len() != 1 {
            return not(if shas.is_empty() {
                "no checkout of the commit in the log"
            } else {
                "more than one checkout in the job"
            });
        }
        if !shas[0].eq_ignore_ascii_case(head_sha) {
            return not("the job checked out another commit than its run's");
        }
    }
    let (Some((red_step, red_summary)), Some((green_step, green_summary))) =
        (tested(red), tested(green))
    else {
        return not("not exactly one test step with one summary in each attempt");
    };
    if red_step.command != green_step.command {
        return not("the attempts ran different test commands");
    }
    if red_summary.green() {
        return not("the red attempt's tests passed: the job failed elsewhere");
    }
    if !green_summary.green() {
        return not("the rerun's tests failed too: the job passed despite them");
    }
    if !red_summary.same_selection(&green_summary) {
        return not("the attempts counted different tests");
    }
    match red_summary.reader {
        Reader::Phpunit => match phpunit_log::parse_str(&red_step.text) {
            Ok(parsed) if !parsed.failures.is_empty() => Judged::Named(parsed.failures),
            Ok(_) => Judged::Unnamed {
                causes: Vec::new(),
                why: "the log names no failing test".to_owned(),
            },
            Err(why) => Judged::Unnamed {
                causes: Vec::new(),
                why,
            },
        },
        Reader::Collision => {
            let named = collision_log::parse_str(&red_step.text)
                .and_then(|parsed| collision_log::name(&parsed.failures, known));
            match named {
                Ok(failures) if !failures.is_empty() => Judged::Named(failures),
                Ok(_) => Judged::Unnamed {
                    causes: ci_log::collision_causes(&red_step.text),
                    why: "the log names no failing test".to_owned(),
                },
                Err(why) => Judged::Unnamed {
                    causes: ci_log::collision_causes(&red_step.text),
                    why,
                },
            }
        }
    }
}

/// The one step of `log` that printed a test summary, and that summary —
/// `None` when no step did, or more than one, or one printed two.
fn tested(log: &str) -> Option<(Step, Summary)> {
    let mut found = ci_log::steps(log).into_iter().filter_map(|step| {
        let summaries = ci_log::summaries(&step.text);
        (!summaries.is_empty()).then_some((step, summaries))
    });
    let (step, summaries) = found.next()?;
    if found.next().is_some() || summaries.len() != 1 {
        return None;
    }
    Some((step, summaries[0]))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{judge, Judged};
    use crate::junit::TestStatus;

    const SHA: &str = "2f040dcffca625c6a1dab909a6a7a14c63ae3b70";

    /// A job log: a checkout of `sha`, then a test step running `command`
    /// whose output is `output`, every line behind a timestamp.
    fn job(sha: &str, command: &str, output: &str) -> String {
        let mut log = String::from("\u{feff}");
        let lines = [
            "##[group]Run actions/checkout@v4".to_owned(),
            "[command]/usr/bin/git log -1 --format=%H".to_owned(),
            sha.to_owned(),
            "##[endgroup]".to_owned(),
            format!("##[group]Run {command}"),
            "##[endgroup]".to_owned(),
        ];
        for line in lines.iter().map(String::as_str).chain(output.lines()) {
            log.push_str("2026-11-20T16:05:02.4586855Z ");
            log.push_str(line);
            log.push('\n');
        }
        log
    }

    const RED: &str = "There was 1 failure:\n\n1) Tests\\Feature\\ShopTest::test_x\nFailed.\n\n\
                       FAILURES!\nTests: 12, Assertions: 30, Failures: 1, Skipped: 1.\n";
    const GREEN: &str = "OK, but there were issues!\nTests: 12, Assertions: 31, Skipped: 1.\n";
    const RUN: &str = "vendor/bin/phpunit";

    fn judged(red: &str, green: &str) -> Judged {
        judge(SHA, red, green, &BTreeSet::new())
    }

    fn reason(found: Judged) -> String {
        match found {
            Judged::NotAPair(why) => why,
            other => panic!("expected no pair, got {other:?}"),
        }
    }

    #[test]
    fn a_red_attempt_and_its_green_rerun_name_the_failures() {
        let found = judged(&job(SHA, RUN, RED), &job(SHA, RUN, GREEN));
        assert_eq!(
            found,
            Judged::Named(
                [(
                    "Tests.Feature.ShopTest::test_x".to_owned(),
                    TestStatus::Failed
                )]
                .into()
            )
        );
    }

    #[test]
    fn the_code_must_be_the_runs_own_commit_in_both_attempts() {
        let other = "1111111111111111111111111111111111111111";
        assert!(
            reason(judged(&job(other, RUN, RED), &job(SHA, RUN, GREEN))).contains("another commit")
        );
        assert!(
            reason(judged(&job(SHA, RUN, RED), &job(other, RUN, GREEN))).contains("another commit")
        );
        assert!(reason(judged(RED, &job(SHA, RUN, GREEN))).contains("no checkout"));
    }

    #[test]
    fn the_tests_decide_not_the_jobs_conclusion() {
        // A green job whose test step failed (continue-on-error, `| tee`).
        let masked = reason(judged(&job(SHA, RUN, RED), &job(SHA, RUN, RED)));
        assert!(masked.contains("failed too"), "{masked}");
        // A red job whose tests passed: it failed at another step.
        let elsewhere = reason(judged(&job(SHA, RUN, GREEN), &job(SHA, RUN, GREEN)));
        assert!(elsewhere.contains("failed elsewhere"), "{elsewhere}");
    }

    #[test]
    fn the_same_command_must_count_the_same_tests() {
        let other_command = reason(judged(
            &job(SHA, RUN, RED),
            &job(SHA, "phpunit tests", GREEN),
        ));
        assert!(other_command.contains("different test commands"));
        let fewer = "OK (11 tests, 30 assertions)\n";
        let counted = reason(judged(&job(SHA, RUN, RED), &job(SHA, RUN, fewer)));
        assert!(counted.contains("different tests"), "{counted}");
        // Two summaries in the step: a retry wrapper, two suites — no pair.
        let twice = format!("{RED}{GREEN}");
        let doubled = reason(judged(&job(SHA, RUN, &twice), &job(SHA, RUN, GREEN)));
        assert!(doubled.contains("exactly one test step"), "{doubled}");
        // A risky test is one that ran whatever its outcome.
        let risky = "FAILURES!\nTests: 12, Assertions: 30, Failures: 1, Skipped: 1, Risky: 1.\n";
        let unrisky = reason(judged(&job(SHA, RUN, risky), &job(SHA, RUN, GREEN)));
        assert!(unrisky.contains("different tests"), "{unrisky}");
    }

    #[test]
    fn a_cut_collision_failure_is_a_pair_without_a_name() {
        let red = "  FAILED  Modules\\Shop\\Test…  QueryException  \n\
                   SQLSTATE[42000]: Incorrect database name\n\
                   at vendor/laravel/framework/src/Illuminate/Database/Connection.php:864\n\
                   30  database/factories/TenantFactory.php:103\n\n\
                   Tests:    1 failed, 9 passed (20 assertions)\n";
        let green = "Tests:    10 passed (21 assertions)\n";
        let command = "php artisan test --compact";
        match judged(&job(SHA, command, red), &job(SHA, command, green)) {
            Judged::Unnamed { causes, why } => {
                assert!(why.contains("cut"), "{why}");
                assert_eq!(causes.len(), 1);
                assert_eq!(causes[0].exception.as_deref(), Some("QueryException"));
                assert_eq!(
                    causes[0].file.as_deref(),
                    Some("database/factories/TenantFactory.php")
                );
            }
            other => panic!("expected an unnamed pair, got {other:?}"),
        }
    }

    #[test]
    fn a_whole_collision_failure_is_named_against_the_history() {
        let red = "  FAILED  Tests\\Feature\\ShopTest > it adds a line  \n\
                   Tests:    1 failed, 9 passed (20 assertions)\n";
        let green = "Tests:    10 passed (21 assertions)\n";
        let command = "php artisan test";
        let known: BTreeSet<String> =
            ["Tests.Feature.ShopTest::test_it_adds_a_line".to_owned()].into();
        let found = judge(
            SHA,
            &job(SHA, command, red),
            &job(SHA, command, green),
            &known,
        );
        assert_eq!(
            found,
            Judged::Named(
                [(
                    "Tests.Feature.ShopTest::test_it_adds_a_line".to_owned(),
                    TestStatus::Failed
                )]
                .into()
            )
        );
        // A history that does not know the test yet cannot name it.
        assert!(matches!(
            judged(&job(SHA, command, red), &job(SHA, command, green)),
            Judged::Unnamed { .. }
        ));
    }
}
