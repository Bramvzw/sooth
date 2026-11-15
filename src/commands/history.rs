//! `sooth history`: the evidence sooth has, and — on request — less of it.

use std::process::ExitCode;

use super::{load_history, Outcome, SoothError};
use crate::report::Analyses;
use crate::{analyzers, cli, history, interrupt, report};

/// `sooth history` looks at the evidence without adding to it: nothing runs,
/// so the observer principle leaves nothing to record.
pub fn run(args: &cli::HistoryArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let path = std::path::Path::new(history::HISTORY_PATH);
    if args.prune {
        // Seconds, for a large history: asked to stop, a prune leaves the
        // history as it was and nothing beside it. Once it is done there
        // is nothing left to stop, and the report below follows as usual.
        let interrupt = interrupt::Interrupt::listen();
        let pruned = history::prune(path, &interrupt)
            .map_err(|err| SoothError::new(format!("could not prune {}: {err}", path.display())))?;
        // No file: the empty-history line below says all there is to say.
        if let Some(pruned) = pruned {
            report::print_pruned(pruned);
        }
    }
    let loaded = load_history(path);
    if loaded.observations.is_empty() {
        report::print_empty_history();
        return Ok(ExitCode::SUCCESS);
    }
    let analysis = analyzers::history::analyze(&loaded.observations);
    if analysis.is_empty() {
        report::print_nothing_stands_out();
    }
    report::print_history(
        &Analyses {
            history: Some(&analysis),
            ..Analyses::default()
        },
        style,
    );
    report::print_history_gap(
        Some(analyzers::history::PriorEvidence::of(&loaded.observations)),
        style,
    );
    if args.prune && loaded.truncated {
        report::print_history_beyond_one_load(loaded.observations.len());
    } else {
        report::print_history_size(loaded.observations.len(), loaded.truncated);
    }
    Ok(ExitCode::SUCCESS)
}
