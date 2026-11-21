//! `sooth import`: evidence from runs sooth did not make itself — CI's
//! reports and logs — brought into the local history.

use std::process::ExitCode;

use super::{append_history, load_history, Outcome, SoothError};
use crate::report::{Analyses, ImportFate, ImportedFile};
use crate::reported::file_state;
use crate::{analyzers, cli, history, interrupt, junit, phpunit_log, report};

/// One report file accepted for import, waiting for the all-or-nothing write.
struct Incoming {
    at_epoch_secs: u64,
    observations: Vec<history::Observation>,
    hash: u64,
    name: String,
    /// Imported before: `--replace` puts this in the earlier import's place.
    replaces: bool,
}

/// One import file's yield: its per-test outcomes plus the timestamp the
/// content itself carries. `None` is a valid file with nothing to record.
type FileYield = Option<(
    std::collections::BTreeMap<String, (junit::TestStatus, Option<u64>)>,
    Option<String>,
)>;

/// The outcomes one import file yields — `Ok(None)` is a green console log,
/// which names no tests. A file that does not parse is sooth's error.
fn file_outcomes(
    content: &str,
    log: Option<cli::LogFormat>,
    path: &std::path::Path,
) -> Result<FileYield, SoothError> {
    match log {
        Some(cli::LogFormat::Phpunit) => match phpunit_log::parse_str(content) {
            Ok(parsed) if parsed.failures.is_empty() => Ok(None),
            // A console log names its failures and no durations.
            Ok(parsed) => Ok(Some((
                parsed
                    .failures
                    .into_iter()
                    .map(|(id, status)| (id, (status, None)))
                    .collect(),
                parsed.at,
            ))),
            Err(err) => Err(SoothError::new(format!(
                "failed to parse PHPUnit log `{}`: {err}",
                path.display()
            ))),
        },
        None => match junit::parse_str(content) {
            Ok(report) => {
                let durations = analyzers::flaky::run_durations(&report);
                let outcomes = analyzers::flaky::run_outcomes(&report)
                    .into_iter()
                    .map(|(id, status)| {
                        let millis = durations.get(&id).map(history::whole_millis);
                        (id, (status, millis))
                    })
                    .collect();
                Ok(Some((outcomes, junit::report_timestamp(content))))
            }
            Err(err) => Err(SoothError::new(format!(
                "failed to parse JUnit-XML report `{}`: {err}",
                path.display()
            ))),
        },
    }
}

/// `--commit` as the sha local runs record, or why it is refused.
fn imported_commit(given: &str) -> Result<String, SoothError> {
    history::resolve_commit(std::path::Path::new("."), given).map_err(|error| {
        let why = match error {
            history::CommitError::NotASha => {
                "is not a sha — pass the commit CI ran on, not a branch or revision name"
            }
            history::CommitError::Unknown => {
                "names no commit this repository knows — pass the full sha, or fetch it first"
            }
        };
        SoothError::new(format!("`--commit {given}` {why}"))
    })
}

/// Handle `sooth import`: bring reports produced elsewhere into the local
/// history. Import is the sole observer of these runs, which is why it may
/// record them where `explain` must not (see `DECISIONS.md`). Validation
/// happens before any write: an invocation imports all of its new files or
/// none of them. Exit 0, or 2 when a file is unreadable or the history
/// cannot be written — writing is the job here, not a side effect.
pub fn run(args: &cli::ImportArgs) -> Outcome {
    let style = report::Style::resolved(args.color);
    let commit = args.commit.as_deref().map(imported_commit).transpose()?;
    let ledger_path = std::path::Path::new(history::IMPORTED_PATH);
    let ledger = history::imported_hashes(ledger_path);
    let (mut incoming, files) = read_imports(args, commit.as_deref(), &ledger)?;

    // Ascending time order, so the file stays readable front to back; the
    // analysis itself no longer depends on it.
    incoming.sort_by_key(|entry| entry.at_epoch_secs);
    let history_path = std::path::Path::new(history::HISTORY_PATH);
    write_imports(history_path, &incoming)?;
    // Said once it is true: the files are in the history now.
    for file in &files {
        report::print_imported(file);
    }
    record_ledger(ledger_path, &incoming);

    let loaded = load_history(history_path);
    let analysis = analyzers::history::analyze(&loaded.observations);
    report::print_history(
        &Analyses {
            history: Some(&analysis),
            ..Analyses::default()
        },
        style,
    );
    report::print_history_size(loaded.observations.len(), loaded.truncated);
    Ok(ExitCode::SUCCESS)
}

/// Read every file of an import into observations, plus what became of
/// each, to be said once it is true. Nothing is written here: a file that
/// cannot be read or parsed fails the whole import before the history is
/// touched.
fn read_imports<'args>(
    args: &'args cli::ImportArgs,
    commit: Option<&str>,
    ledger: &std::collections::BTreeSet<u64>,
) -> Result<(Vec<Incoming>, Vec<ImportedFile<'args>>), SoothError> {
    let mut incoming: Vec<Incoming> = Vec::new();
    let mut files = Vec::new();
    let mut named_before = std::collections::BTreeSet::new();
    for path in &args.reports {
        let content = std::fs::read_to_string(path).map_err(|err| {
            SoothError::new(format!("could not read `{}`: {err}", path.display()))
        })?;
        let hash = history::fnv1a64(content.as_bytes());
        // The same content twice on one command line is one file.
        if !named_before.insert(hash) {
            files.push(ImportedFile {
                path,
                fate: ImportFate::SameContent,
            });
            continue;
        }
        let replaces = ledger.contains(&hash);
        if replaces && !args.replace {
            files.push(ImportedFile {
                path,
                fate: ImportFate::AlreadyImported,
            });
            continue;
        }
        let parsed = file_outcomes(&content, args.log, path)?;
        let Some((outcomes, content_stamp)) = parsed else {
            // Said with the rest, once the import as a whole went through.
            files.push(ImportedFile {
                path,
                fate: ImportFate::NothingToRecord,
            });
            continue;
        };
        let at_epoch_secs = content_stamp
            .and_then(|stamp| history::iso8601_to_epoch(&stamp))
            .or_else(|| file_state(path).and_then(|(mtime, _)| epoch_secs(mtime)))
            .unwrap_or_else(history::now_epoch_secs);
        let observations: Vec<history::Observation> = outcomes
            .into_iter()
            .map(|(id, (status, duration_ms))| history::Observation {
                id,
                status,
                duration_ms,
                commit: commit.map(str::to_owned),
                // --commit asserts a clean checkout of that commit; without
                // it the code state is unknowable, not clean.
                dirty: commit.map(|_| false),
                tree: None,
                environment: Some(args.env.clone()),
                invocation: Some(history::import_invocation(hash)),
                at_epoch_secs,
            })
            .collect();
        files.push(ImportedFile {
            path,
            fate: ImportFate::Recorded {
                observations: observations.len(),
                replaced: replaces,
            },
        });
        incoming.push(Incoming {
            at_epoch_secs,
            observations,
            hash,
            name: path.display().to_string(),
            replaces,
        });
    }
    Ok((incoming, files))
}

/// Put an import's observations into the history: appended, or — when
/// `--replace` imports a file again — written in the place of that file's
/// earlier observations, in one rewrite.
fn write_imports(history_path: &std::path::Path, incoming: &[Incoming]) -> Result<(), SoothError> {
    let observations: Vec<history::Observation> = incoming
        .iter()
        .flat_map(|entry| entry.observations.iter().cloned())
        .collect();
    let replaced: Vec<String> = incoming
        .iter()
        .filter(|entry| entry.replaces)
        .map(|entry| history::import_invocation(entry.hash))
        .collect();
    let failed = |err: std::io::Error| {
        SoothError::new(format!("could not write {}: {err}", history_path.display()))
    };
    if !replaced.is_empty() {
        // A rewrite of a large history takes a moment: asked to stop, it
        // leaves the history as it was and nothing beside it.
        let interrupt = interrupt::Interrupt::listen();
        match history::replace_invocations(history_path, &replaced, &observations, &interrupt) {
            Ok(Some(0)) => {
                report::warn(&format!(
                    "found no observations of the earlier import in {} — nothing \
                     was removed, and the file's observations were added. An import \
                     recorded before v0.3.1 carries no id to find it by",
                    history_path.display()
                ));
                return Ok(());
            }
            Ok(Some(_)) => return Ok(()),
            // No history file: nothing to replace, so this is a plain import.
            Ok(None) => {}
            Err(err) => return Err(failed(err)),
        }
    }
    append_history(history_path, &observations).map_err(failed)
}

/// Note the newly imported files in the ledger. A replaced file is in it
/// already.
fn record_ledger(ledger_path: &std::path::Path, incoming: &[Incoming]) {
    let ledger: Vec<(u64, String)> = incoming
        .iter()
        .filter(|entry| !entry.replaces)
        .map(|entry| (entry.hash, entry.name.clone()))
        .collect();
    if let Err(err) = history::record_imported(ledger_path, &ledger) {
        // The observations are recorded; only the dedupe bookkeeping failed.
        report::warn(&format!(
            "could not write {}: {err} — re-importing these files will double-count",
            ledger_path.display()
        ));
    }
}

/// Seconds since the epoch for a file timestamp; `None` before 1970.
fn epoch_secs(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::read_imports;
    use crate::cli::{Cli, Command, ImportArgs};
    use crate::history;
    use crate::report::ImportFate;
    use clap::Parser;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    const STAMPED: &str = r#"<testsuite timestamp="2026-01-02T03:04:05"><testcase classname="c" name="t" time="0.5"/></testsuite>"#;
    const UNSTAMPED: &str = r#"<testsuite><testcase classname="c" name="u"/></testsuite>"#;

    /// A scratch directory with the given files, removed by the caller.
    fn scratch(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sooth-import-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        dir
    }

    fn import_args(dir: &Path, flags: &[&str], files: &[&str]) -> ImportArgs {
        let files = files
            .iter()
            .map(|name| dir.join(name).display().to_string());
        let cmdline = ["sooth", "import", "--env", "ci"]
            .into_iter()
            .chain(flags.iter().copied())
            .map(str::to_owned)
            .chain(files);
        let Command::Import(args) = Cli::try_parse_from(cmdline).unwrap().command else {
            panic!("expected `import`");
        };
        args
    }

    fn fates(args: &ImportArgs, ledger: &BTreeSet<u64>) -> (usize, Vec<ImportFate>) {
        let (incoming, files) = read_imports(args, None, ledger).unwrap();
        (incoming.len(), files.iter().map(|file| file.fate).collect())
    }

    #[test]
    fn a_file_named_twice_is_one_file_and_an_imported_one_waits_for_replace() {
        let dir = scratch("fates", &[("a.xml", STAMPED), ("copy.xml", STAMPED)]);
        let recorded = |replaced| ImportFate::Recorded {
            observations: 1,
            replaced,
        };
        let new = BTreeSet::new();
        let known = BTreeSet::from([history::fnv1a64(STAMPED.as_bytes())]);

        let plain = import_args(&dir, &[], &["a.xml", "copy.xml"]);
        assert_eq!(
            fates(&plain, &new),
            (1, vec![recorded(false), ImportFate::SameContent])
        );
        // In the ledger: nothing is read again unless that is asked for.
        assert_eq!(
            fates(&plain, &known),
            (
                0,
                vec![ImportFate::AlreadyImported, ImportFate::SameContent]
            )
        );
        let replace = import_args(&dir, &["--replace"], &["a.xml", "copy.xml"]);
        assert_eq!(
            fates(&replace, &known),
            (1, vec![recorded(true), ImportFate::SameContent])
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_green_log_is_a_file_with_nothing_to_record_said_with_the_rest() {
        let dir = scratch(
            "green-log",
            &[("green.log", "....\nOK (4 tests, 8 assertions)\n")],
        );
        let args = import_args(&dir, &["--log", "phpunit"], &["green.log"]);
        assert_eq!(
            fates(&args, &BTreeSet::new()),
            (0, vec![ImportFate::NothingToRecord])
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_import_is_dated_by_its_content_and_failing_that_by_its_file() {
        let dir = scratch(
            "dates",
            &[("stamped.xml", STAMPED), ("bare.xml", UNSTAMPED)],
        );
        // The file's own time, set to a known moment.
        let written = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(dir.join("bare.xml"))
            .unwrap()
            .set_modified(written)
            .unwrap();

        let args = import_args(&dir, &[], &["stamped.xml", "bare.xml"]);
        let (incoming, _) = read_imports(&args, None, &BTreeSet::new()).unwrap();
        let at: Vec<u64> = incoming.iter().map(|entry| entry.at_epoch_secs).collect();
        assert_eq!(
            at,
            [
                history::iso8601_to_epoch("2026-01-02T03:04:05").unwrap(),
                1_700_000_000
            ]
        );
        // Without `--commit` the code state is unknowable, not clean; and
        // the durations a report carries come along.
        let observation = &incoming[0].observations[0];
        assert_eq!(
            (observation.commit.as_deref(), observation.dirty),
            (None, None)
        );
        assert_eq!(observation.duration_ms, Some(500));
        assert_eq!(observation.environment.as_deref(), Some("ci"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_unreadable_file_fails_the_whole_import_before_anything_is_kept() {
        let dir = scratch(
            "junk",
            &[("good.xml", STAMPED), ("junk.xml", "not a report")],
        );
        let args = import_args(&dir, &[], &["good.xml", "junk.xml"]);
        let error = read_imports(&args, None, &BTreeSet::new())
            .err()
            .expect("junk is refused");
        assert!(
            error.lines[0].contains("failed to parse JUnit-XML report"),
            "{:?}",
            error.lines
        );
        let missing = import_args(&dir, &[], &["good.xml", "nope.xml"]);
        let error = read_imports(&missing, None, &BTreeSet::new())
            .err()
            .expect("a missing file is refused");
        assert!(
            error.lines[0].contains("could not read"),
            "{:?}",
            error.lines
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
