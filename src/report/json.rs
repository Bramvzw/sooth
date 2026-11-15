//! The machine report: one JSON document per command, hand-rolled (see
//! `DECISIONS.md`). Nothing here prints; a document is a `String`.

use super::{Analyses, Interrupted, JunitSummary};
use crate::analyzers::{explain, flaky, history};
use crate::json::escape;
use crate::runner::RunOutcome;
use crate::verify;

/// Version of the `--json` shape. Fields are only added within a version;
/// this number is bumped when the shape changes incompatibly.
pub const JSON_SCHEMA_VERSION: u32 = 1;

/// Hand-rolled JSON: the run outcomes plus the junit summary, versioned via
/// `schema_version`. Revisited when this story landed and deliberately kept
/// hand-rolled: the shape is still small and fixed, so `serde_json` is still
/// not worth a second dependency — see `DECISIONS.md`.
pub fn to_json(outcomes: &[RunOutcome], summary: &JunitSummary, analyses: &Analyses<'_>) -> String {
    let Analyses {
        flaky,
        history,
        verify,
        pardoned,
        explanation,
        gate,
        interrupted,
        // Context for the human note only; the JSON carries the counts.
        history_observations: _,
    } = *analyses;
    let runs: Vec<String> = outcomes
        .iter()
        .map(|outcome| {
            let exit_code = outcome
                .exit_code
                .map_or_else(|| "null".to_owned(), |code| code.to_string());
            format!(
                r#"{{"exit_code":{exit_code},"success":{},"duration_seconds":{}}}"#,
                outcome.success,
                outcome.duration.as_secs_f64()
            )
        })
        .collect();

    let slowest: Vec<String> = summary
        .slowest
        .iter()
        .map(|(name, duration)| {
            let name = escape(name);
            format!(
                r#"{{"name":"{name}","duration_seconds":{}}}"#,
                duration.as_secs_f64()
            )
        })
        .collect();

    // Additive within schema_version 1: the analysis fields appear only
    // when a multi-run analysis ran.
    let active = flaky.map_or(String::new(), |pass| {
        let reordered: Vec<String> = pass.reordered_runs.iter().map(usize::to_string).collect();
        let monotone: Vec<String> = pass
            .monotone
            .iter()
            .map(|flip| {
                format!(
                    r#"{{"name":"{}","flipped_after_run":{},"started_green":{}}}"#,
                    escape(&flip.outcomes.id),
                    flip.flipped_after_run,
                    flip.started_green
                )
            })
            .collect();
        let lone: Vec<String> = pass
            .lone_failures
            .iter()
            .map(|test| {
                format!(
                    r#"{{"name":"{}","absent_runs":{}}}"#,
                    escape(&test.id),
                    test.absent_runs
                )
            })
            .collect();
        format!(
            r#","flaky":[{}],"broken":[{}],"monotone":[{}],"lone_failures":[{}],"reordered_runs":[{}]"#,
            outcome_entries(&pass.flaky),
            outcome_entries(&pass.broken),
            monotone.join(","),
            lone.join(","),
            reordered.join(",")
        )
    });

    // Additive within schema_version 1: present whenever the history pass ran.
    let history = history.map_or(String::new(), history_object);

    let verification = verify.map_or(String::new(), verification_object);

    let quarantine = pardoned.map_or(String::new(), |ids| {
        format!(r#","quarantine":{{"pardoned":[{}]}}"#, json_ids(ids))
    });

    let explanation = explanation.map_or(String::new(), |explanations| {
        format!(r#","explanation":{}"#, explanation_object(explanations))
    });

    // Additive within schema_version 1: present when `--changed` gated the run.
    let gate = gate.map_or(String::new(), |selection| {
        format!(
            r#","gate":{{"base":"{}","files":[{}]}}"#,
            escape(&selection.base),
            json_ids(&selection.files)
        )
    });

    let interrupted = interrupted.map_or(String::new(), |stop| {
        interrupted_member(stop, outcomes.len())
    });

    format!(
        r#"{{"schema_version":{JSON_SCHEMA_VERSION},"sooth_version":"{}","runs":[{}],"junit":{{"total":{},"passed":{},"failed":{},"errors":{},"skipped":{},"slowest":[{}]}}{active}{history}{verification}{quarantine}{explanation}{gate}{interrupted}}}"#,
        env!("CARGO_PKG_VERSION"),
        runs.join(","),
        summary.total,
        summary.passed,
        summary.failed,
        summary.error,
        summary.skipped,
        slowest.join(","),
    )
}

/// The `interrupted` member, with its leading comma. Additive within
/// `schema_version` 1: present when sooth was asked to stop before it was
/// done. The document's `runs` are the `finished_runs`.
fn interrupted_member(stop: Interrupted, finished_runs: usize) -> String {
    let (during, requested) = match stop {
        Interrupted::Runs { requested, .. } => ("runs", requested),
        // Every run that was asked for had finished by then.
        Interrupted::Verification => ("verification", finished_runs),
    };
    format!(
        r#","interrupted":{{"during":"{during}","finished_runs":{finished_runs},"requested_runs":{requested}}}"#
    )
}

/// The whole machine output of `sooth explain`: no runs and no totals, since
/// explain observes no run.
pub fn explanation_json(
    report_path: &std::path::Path,
    explanations: &[explain::Explanation],
) -> String {
    format!(
        r#"{{"schema_version":{JSON_SCHEMA_VERSION},"sooth_version":"{}","report":"{}","explanation":{}}}"#,
        env!("CARGO_PKG_VERSION"),
        escape(&report_path.display().to_string()),
        explanation_object(explanations)
    )
}

/// A category under the name the document gives it.
fn category_name(category: explain::Category) -> &'static str {
    match category {
        explain::Category::KnownFlake => "known_flake",
        explain::Category::FailingSince => "failing_since",
        explain::Category::NeverPassed => "never_passed",
        explain::Category::Quarantined => "quarantined",
        explain::Category::New => "new",
    }
}

/// The explanation object both output shapes share. `verdict` names the
/// category the failure was counted in, `quarantined` the label beside it.
fn explanation_object(explanations: &[explain::Explanation]) -> String {
    let entries: Vec<String> = explanations
        .iter()
        .map(|explanation| {
            let name = escape(&explanation.id);
            let quarantined = explanation.quarantined;
            // Asked, not derived again: the counts below rest on the same rule.
            let verdict = category_name(explanation.category());
            match &explanation.verdict {
                explain::Verdict::KnownFlake {
                    failed_runs,
                    observed_runs,
                    // The environment belongs to the human line; the JSON
                    // carries it under `history`, where the pass owns it.
                    failures_confined_to: _,
                    red_since,
                } => format!(
                    r#"{{"name":"{name}","verdict":"{verdict}","quarantined":{quarantined},"failed_runs":{failed_runs},"observed_runs":{observed_runs},"red_since":{}}}"#,
                    red_streak_json(red_since.as_ref())
                ),
                explain::Verdict::FailingSince {
                    commit,
                    failed_runs,
                } => format!(
                    r#"{{"name":"{name}","verdict":"{verdict}","quarantined":{quarantined},"commit":"{}","failed_runs":{failed_runs}}}"#,
                    escape(commit)
                ),
                explain::Verdict::NeverPassed {
                    first_commit,
                    failed_runs,
                } => format!(
                    r#"{{"name":"{name}","verdict":"{verdict}","quarantined":{quarantined},"first_commit":"{}","failed_runs":{failed_runs}}}"#,
                    escape(first_commit)
                ),
                explain::Verdict::Unknown => format!(
                    r#"{{"name":"{name}","verdict":"{verdict}","quarantined":{quarantined}}}"#
                ),
            }
        })
        .collect();
    let counts = explain::Counts::of(explanations);
    format!(
        r#"{{"failures":[{}],"counts":{{"known_flakes":{},"failing_since":{},"never_passed":{},"quarantined":{},"new":{}}},"only_known_flakes":{}}}"#,
        entries.join(","),
        counts.known_flakes,
        counts.failing_since,
        counts.never_passed,
        counts.quarantined,
        counts.new,
        counts.only_known_flakes()
    )
}

/// The verification pass as JSON: the three id lists plus, per failure that
/// failed differently, both signatures.
fn verification_object(verdict: &verify::Verdict) -> String {
    let different: Vec<String> = verdict
        .failed_differently
        .iter()
        .map(|failure| {
            format!(
                r#"{{"name":"{}","suite":"{}","isolation":"{}"}}"#,
                escape(&failure.id),
                escape(&failure.suite),
                escape(&failure.isolation)
            )
        })
        .collect();
    format!(
        r#","verification":{{"real":[{}],"flaky_or_order_dependent":[{}],"failed_differently":[{}],"unverified":[{}]}}"#,
        json_ids(&verdict.real),
        json_ids(&verdict.flaky_or_order),
        different.join(","),
        json_ids(&verdict.unverified)
    )
}

/// The history pass as JSON. `failures_confined_to` is the environment
/// half: `null` unless every failure came from one environment and another
/// environment observed the test too.
fn history_object(pass: &history::Analysis) -> String {
    let flaky_entries: Vec<String> = pass
        .flaky
        .iter()
        .map(|test| {
            let confined = test.failures_confined_to.as_deref().map_or_else(
                || "null".to_owned(),
                |environment| format!(r#""{}""#, escape(environment)),
            );
            format!(
                r#"{{"name":"{}","failed_runs":{},"observed_runs":{},"failures_confined_to":{confined},"red_since":{}}}"#,
                escape(&test.outcomes.id),
                test.outcomes.failed,
                test.outcomes.observed(),
                red_streak_json(test.red_since.as_ref())
            )
        })
        .collect();
    let never_entries: Vec<String> = pass
        .never_passed
        .iter()
        .map(|test| {
            format!(
                r#"{{"name":"{}","first_commit":"{}","failed_runs":{}}}"#,
                escape(&test.id),
                escape(&test.first_commit),
                test.failed_runs
            )
        })
        .collect();
    let since_entries: Vec<String> = pass
        .failing_since
        .iter()
        .map(|test| {
            format!(
                r#"{{"name":"{}","commit":"{}","failed_runs":{}}}"#,
                escape(&test.id),
                escape(&test.commit),
                test.failed_runs
            )
        })
        .collect();
    format!(
        r#","history":{{"flaky":[{}],"failing_since":[{}],"never_passed":[{}]}}"#,
        flaky_entries.join(","),
        since_entries.join(","),
        never_entries.join(",")
    )
}

/// `null`, or the streak a proven flake has been red for.
fn red_streak_json(streak: Option<&history::RedStreak>) -> String {
    streak.map_or_else(
        || "null".to_owned(),
        |streak| {
            format!(
                r#"{{"commit":"{}","failed_runs":{}}}"#,
                escape(&streak.commit),
                streak.failed_runs
            )
        },
    )
}

/// A comma-joined JSON array body of per-test outcome counts: the shape both
/// flaky rankings serialize to.
fn outcome_entries(tests: &[flaky::TestOutcomes]) -> String {
    tests
        .iter()
        .map(|test| {
            format!(
                r#"{{"name":"{}","failed_runs":{},"observed_runs":{}}}"#,
                escape(&test.id),
                test.failed,
                test.observed()
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A comma-joined JSON array body of escaped id strings.
fn json_ids(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!(r#""{}""#, escape(id)))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{outcome, test_case};
    use super::to_json;
    use crate::analyzers::{explain, flaky};
    use crate::junit::{JunitReport, TestStatus};
    use crate::report::{Analyses, JunitSummary};

    #[test]
    fn the_json_says_what_sooth_was_doing_when_it_was_asked_to_stop() {
        use super::Interrupted;
        let json_for = |interrupted| {
            to_json(
                &[outcome(false)],
                &JunitSummary::empty(),
                &Analyses {
                    interrupted,
                    ..Analyses::default()
                },
            )
        };
        assert!(!json_for(None).contains("interrupted"));
        assert!(json_for(Some(Interrupted::Runs {
            requested: 4,
            failed: 1
        }))
        .ends_with(r#","interrupted":{"during":"runs","finished_runs":1,"requested_runs":4}}"#));
        // The runs were all made; it is the re-runs that were not.
        assert!(json_for(Some(Interrupted::Verification)).ends_with(
            r#","interrupted":{"during":"verification","finished_runs":1,"requested_runs":1}}"#
        ));
    }

    #[test]
    fn json_output_is_versioned_and_carries_runs_and_summary() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Passed, 0.25)],
            },
            10,
        );
        let json = to_json(&[outcome(true)], &summary, &Analyses::default());

        assert!(json.starts_with(r#"{"schema_version":1,"#));
        assert!(json.contains(&format!(
            r#""sooth_version":"{}""#,
            env!("CARGO_PKG_VERSION")
        )));
        assert!(json.contains(r#""success":true"#));
        assert!(json.contains(r#""total":1"#));
        // Renamed from "error" before v0.1.0 froze the schema: every other
        // count is plural and the human output says "errors".
        assert!(json.contains(r#""errors":0"#));
        assert!(json.contains(r#""name":"a""#));
    }

    #[test]
    fn the_explanation_json_carries_the_verdict_the_counts_and_the_report() {
        let explanations = [
            explain::Explanation {
                observed: None,
                id: "c::wobbly".to_owned(),
                verdict: explain::Verdict::KnownFlake {
                    failed_runs: 4,
                    observed_runs: 50,
                    failures_confined_to: None,
                    red_since: None,
                },
                quarantined: true,
            },
            explain::Explanation {
                observed: None,
                id: "c::fresh".to_owned(),
                verdict: explain::Verdict::Unknown,
                quarantined: false,
            },
        ];
        let json = super::explanation_json(std::path::Path::new("out/report.xml"), &explanations);

        assert!(json.starts_with(r#"{"schema_version":1,"#));
        assert!(json.contains(r#""report":"out/report.xml""#));
        assert!(json.contains(
            r#"{"name":"c::wobbly","verdict":"known_flake","quarantined":true,"failed_runs":4,"observed_runs":50,"red_since":null}"#
        ));
        assert!(json.contains(r#"{"name":"c::fresh","verdict":"new","quarantined":false}"#));
        assert!(json
            .contains(r#""counts":{"known_flakes":1,"failing_since":0,"never_passed":0,"quarantined":0,"new":1}"#));
        assert!(json.contains(r#""only_known_flakes":false"#));
    }

    #[test]
    fn the_json_carries_which_runs_were_reordered() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = flaky::Analysis {
            reordered_runs: vec![2, 3],
            ..flaky::Analysis::default()
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&pass),
                ..Analyses::default()
            },
        );
        // A consumer must be able to see that the ranking rests on runs that
        // did not share one order.
        assert!(json.contains(r#""reordered_runs":[2,3]"#), "got: {json}");

        let stable = flaky::Analysis::default();
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&stable),
                ..Analyses::default()
            },
        );
        assert!(json.contains(r#""reordered_runs":[]"#), "got: {json}");
    }

    #[test]
    fn the_json_carries_the_flaky_and_broken_entries_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = flaky::Analysis {
            flaky: vec![flaky::TestOutcomes {
                id: "c::wob".to_owned(),
                passed: 1,
                failed: 2,
            }],
            broken: vec![flaky::TestOutcomes {
                id: "c::dead".to_owned(),
                passed: 0,
                failed: 3,
            }],
            ..flaky::Analysis::default()
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&pass),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(r#""flaky":[{"name":"c::wob","failed_runs":2,"observed_runs":3}]"#),
            "got: {json}"
        );
        assert!(
            json.contains(r#""broken":[{"name":"c::dead","failed_runs":3,"observed_runs":3}]"#),
            "got: {json}"
        );
    }

    #[test]
    fn the_json_carries_the_monotone_and_lone_entries_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = flaky::Analysis {
            monotone: vec![flaky::MonotoneFlip {
                outcomes: flaky::TestOutcomes {
                    id: "c::polluted".to_owned(),
                    passed: 1,
                    failed: 2,
                },
                flipped_after_run: 1,
                started_green: true,
            }],
            lone_failures: vec![flaky::LoneFailure {
                id: "c::drift".to_owned(),
                absent_runs: 2,
            }],
            ..flaky::Analysis::default()
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                flaky: Some(&pass),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(
                r#""monotone":[{"name":"c::polluted","flipped_after_run":1,"started_green":true}]"#
            ),
            "got: {json}"
        );
        assert!(
            json.contains(r#""lone_failures":[{"name":"c::drift","absent_runs":2}]"#),
            "got: {json}"
        );
    }

    #[test]
    fn the_json_carries_the_history_objects_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let pass = crate::analyzers::history::Analysis {
            flaky: vec![crate::analyzers::history::HistoricFlake {
                outcomes: flaky::TestOutcomes {
                    id: "c::wob".to_owned(),
                    passed: 3,
                    failed: 1,
                },
                failures_confined_to: Some("ci".to_owned()),
                red_since: None,
            }],
            failing_since: vec![crate::analyzers::history::FailingSince {
                id: "c::reg".to_owned(),
                commit: "abc1234def".to_owned(),
                failed_runs: 3,
            }],
            never_passed: Vec::new(),
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                history: Some(&pass),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(
                r#""history":{"flaky":[{"name":"c::wob","failed_runs":1,"observed_runs":4,"failures_confined_to":"ci","red_since":null}],"failing_since":[{"name":"c::reg","commit":"abc1234def","failed_runs":3}],"never_passed":[]}"#
            ),
            "got: {json}"
        );
    }

    #[test]
    fn the_json_carries_the_verification_buckets_with_their_fields() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.1)],
            },
            0,
        );
        let verdict = crate::verify::Verdict {
            real: vec!["c::dead".to_owned()],
            flaky_or_order: vec!["c::wob".to_owned()],
            failed_differently: vec![crate::verify::DifferentFailure {
                id: "c::boot".to_owned(),
                suite: "AssertionError".to_owned(),
                isolation: "RuntimeException".to_owned(),
            }],
            unverified: vec!["c::missed".to_owned()],
        };
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                verify: Some(&verdict),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(
                r#""verification":{"real":["c::dead"],"flaky_or_order_dependent":["c::wob"],"failed_differently":[{"name":"c::boot","suite":"AssertionError","isolation":"RuntimeException"}],"unverified":["c::missed"]}"#
            ),
            "got: {json}"
        );
    }

    #[test]
    fn the_run_json_gains_the_explanation_only_when_the_pass_ran() {
        let summary = JunitSummary::from_report(
            &JunitReport {
                test_cases: vec![test_case("a", TestStatus::Failed, 0.25)],
            },
            0,
        );
        assert!(!to_json(&[outcome(false)], &summary, &Analyses::default())
            .contains(r#""explanation""#));

        let explanations = [explain::Explanation {
            observed: None,
            id: "a".to_owned(),
            verdict: explain::Verdict::FailingSince {
                commit: "abc1234".to_owned(),
                failed_runs: 3,
            },
            quarantined: false,
        }];
        let json = to_json(
            &[outcome(false)],
            &summary,
            &Analyses {
                explanation: Some(&explanations),
                ..Analyses::default()
            },
        );
        assert!(json.contains(
            r#""explanation":{"failures":[{"name":"a","verdict":"failing_since","quarantined":false,"commit":"abc1234","failed_runs":3}]"#
        ));
    }

    #[test]
    fn the_json_carries_the_gate_selection_when_the_gate_ran() {
        let selection = crate::gate::Selection {
            base: "origin/main".to_owned(),
            files: vec!["a/BTest.php".to_owned()],
        };
        let json = to_json(
            &[],
            &JunitSummary::empty(),
            &Analyses {
                gate: Some(&selection),
                ..Analyses::default()
            },
        );
        assert!(
            json.contains(r#""gate":{"base":"origin/main","files":["a/BTest.php"]}"#),
            "got: {json}"
        );
        // The empty gate's document: zero runs is a result, not an omission.
        assert!(json.contains(r#""runs":[]"#), "got: {json}");
    }
}
