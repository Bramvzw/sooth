//! `sooth explain`: diagnosis only. It reads a report someone else's run
//! left and says what the evidence knows about its failures.

use std::process::ExitCode;

use super::{load_history, load_quarantine, Outcome, SoothError};
use crate::report::Analyses;
use crate::{analyzers, cli, history, junit, report, verify};

/// Handle `sooth explain`: label every failure in a JUnit-XML report against
/// the accumulated evidence. Runs nothing, records nothing (see
/// `DECISIONS.md`); exits 0 when the report could be read, 2 when it could not.
pub fn run(args: &cli::ExplainArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let report = load_report(&args.junit).map_err(SoothError::new)?;
    let loaded = load_history(std::path::Path::new(history::HISTORY_PATH));
    let analysis = analyzers::history::analyze(&loaded.observations);
    let quarantine = load_quarantine();
    // Explain runs nothing, so only the history has anything to contribute.
    let passes = analyzers::explain::Passes {
        history: Some(&analysis),
        ..analyzers::explain::Passes::default()
    };
    let explanations =
        analyzers::explain::explain(&verify::failed_ids(&report), passes, &quarantine);

    if args.json {
        report::print_json(&report::explanation_json(&args.junit, &explanations));
        return Ok(ExitCode::SUCCESS);
    }
    if explanations.is_empty() {
        report::print_nothing_to_explain(&args.junit);
        return Ok(ExitCode::SUCCESS);
    }
    report::print_explanation(
        &Analyses {
            explanation: Some(&explanations),
            history_observations: Some(analyzers::history::PriorEvidence::of(&loaded.observations)),
            ..Analyses::default()
        },
        style,
    );
    Ok(ExitCode::SUCCESS)
}

/// Load the JUnit-XML report at `path`, or say why not.
fn load_report(path: &std::path::Path) -> Result<junit::JunitReport, String> {
    junit::parse_file(path).map_err(|err| {
        format!(
            "failed to parse JUnit-XML report `{}`: {err}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_user_junit_file_reports_the_path() {
        let missing = std::env::temp_dir().join("sooth-test-no-such-report.xml");
        let message =
            super::load_report(&missing).expect_err("a missing --junit file should error");
        assert!(message.contains("failed to parse"));
        assert!(message.contains("sooth-test-no-such-report.xml"));
    }
}
