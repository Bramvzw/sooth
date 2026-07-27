//! Built-in presets: inject the right reporter flags into the wrapped test
//! command so it writes a JUnit-XML report sooth can parse.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli::Preset;

/// What a preset adds to the wrapped command: extra arguments and environment
/// variables for the child process.
struct Injection {
    args: Vec<String>,
    envs: Vec<(String, String)>,
}

/// A command to spawn plus the environment to add.
pub type Spawn = (Vec<String>, Vec<(String, String)>);

/// Create a fresh, private directory for a preset-managed report and return
/// the report path inside it.
///
/// Fresh-per-invocation is a truthfulness guard: a stale report left behind
/// by an earlier run (crashed before cleanup) must never be parsed as this
/// run's result. Private (mode 0700 on Unix) with an unpredictable name
/// closes the classic shared-`/tmp` pre-creation/symlink trick.
///
/// # Errors
///
/// Returns the underlying I/O error if no directory can be created.
pub fn report_path() -> io::Result<PathBuf> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    for attempt in 0..32 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        let dir = base.join(format!("sooth-{pid}-{nanos}-{attempt}"));
        match create_private_dir(&dir) {
            Ok(()) => return Ok(dir.join("report.xml")),
            // Collision: someone (or a previous iteration) got there first.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique temp directory for the report",
    ))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// The full command to spawn and the environment to add: the user's program,
/// then the preset's injected arguments, then the user's own arguments.
///
/// The injected arguments go directly after the program name on purpose: they
/// must precede any `--` separator the runner itself understands (gotestsum
/// splits its own flags from `go test`'s at `--`).
pub fn inject(command: &[String], preset: Preset, report: &Path) -> Spawn {
    let Injection { args, envs } = injection(preset, report);
    let mut full = Vec::with_capacity(command.len() + args.len());
    full.push(command[0].clone());
    full.extend(args);
    full.extend(command[1..].iter().cloned());
    (full, envs)
}

/// The full command to re-run only the tests named `names` under `preset`,
/// writing to `report`. `names` are raw `name` attributes straight from the
/// report, never joined `classname::name` identities — a name may itself
/// contain `::`, so re-splitting a joined id is lossy (#91). `None` when
/// sooth cannot restrict this preset (see `DECISIONS.md`).
pub fn inject_selected(
    command: &[String],
    preset: Preset,
    report: &Path,
    names: &[String],
) -> Option<Spawn> {
    let selection = selection_args(preset, names)?;
    let Injection { args, envs } = injection(preset, report);
    let mut full = Vec::with_capacity(command.len() + args.len() + selection.len());
    full.push(command[0].clone());
    full.extend(args);
    full.extend(selection);
    full.extend(command[1..].iter().cloned());
    Some((full, envs))
}

/// The selection flag(s) that restrict `preset` to the tests named `names`.
fn selection_args(preset: Preset, names: &[String]) -> Option<Vec<String>> {
    match preset {
        // The JUnit classname is dotted but --filter wants the backslashed
        // FQCN; only the name half survives both (#93).
        Preset::Phpunit => {
            let mut methods: Vec<String> = names.iter().map(|name| regex_escape(name)).collect();
            methods.sort_unstable();
            methods.dedup();
            Some(vec![
                "--filter".to_owned(),
                format!("/::({})/", methods.join("|")),
            ])
        }
        // A JUnit classname is not a pytest node id; select by test name.
        Preset::Pytest => {
            let mut bases: Vec<&str> = names.iter().map(|name| base_name(name)).collect();
            bases.sort_unstable();
            bases.dedup();
            Some(vec!["-k".to_owned(), bases.join(" or ")])
        }
        // jest -t matches the test name, not the classname.
        Preset::Jest => {
            let pattern = names
                .iter()
                .map(|name| regex_escape(name))
                .collect::<Vec<_>>()
                .join("|");
            Some(vec!["-t".to_owned(), format!("({pattern})")])
        }
        Preset::Go => None,
    }
}

/// Whether `preset` can be restricted to a subset of tests.
pub fn supports_selection(preset: Preset) -> bool {
    selection_args(preset, &[]).is_some()
}

/// The name without its `[parameters]` suffix (brackets break a `-k` expression).
fn base_name(name: &str) -> &str {
    name.find('[').map_or(name, |i| &name[..i])
}

/// Escape regex metacharacters so an identity matches literally.
fn regex_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if "\\^$.|?*+()[]{}/".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// The reporter flags and environment for `preset`, writing to `report`.
fn injection(preset: Preset, report: &Path) -> Injection {
    let report = report.display().to_string();
    match preset {
        Preset::Pytest => Injection {
            args: vec![format!("--junit-xml={report}")],
            envs: Vec::new(),
        },
        Preset::Phpunit => Injection {
            args: vec!["--log-junit".to_owned(), report],
            envs: Vec::new(),
        },
        // jest-junit reads its output path from the environment. The default
        // reporter is kept so the console output the user knows is unchanged.
        Preset::Jest => Injection {
            args: vec![
                "--reporters=default".to_owned(),
                "--reporters=jest-junit".to_owned(),
            ],
            envs: vec![("JEST_JUNIT_OUTPUT_FILE".to_owned(), report)],
        },
        Preset::Go => Injection {
            args: vec![format!("--junitfile={report}")],
            envs: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{inject, inject_selected, report_path, selection_args};
    use crate::cli::Preset;
    use std::path::Path;

    #[test]
    fn report_paths_are_fresh_private_directories() {
        let first = report_path().unwrap();
        let second = report_path().unwrap();

        // Fresh per invocation: a stale report from an earlier run can never
        // be parsed as this run's result.
        assert_ne!(first, second);
        assert!(!first.exists());

        let dir = first.parent().expect("report lives inside its own dir");
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        let _ = std::fs::remove_dir(dir);
        let _ = std::fs::remove_dir(second.parent().unwrap());
    }

    fn command(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn pytest_injects_the_junit_xml_flag_after_the_program() {
        let (full, envs) = inject(
            &command(&["pytest", "-k", "foo"]),
            Preset::Pytest,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&["pytest", "--junit-xml=/tmp/r.xml", "-k", "foo"])
        );
        assert!(envs.is_empty());
    }

    #[test]
    fn phpunit_injects_log_junit_as_two_arguments() {
        let (full, envs) = inject(
            &command(&["phpunit", "--testsuite", "unit"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&[
                "phpunit",
                "--log-junit",
                "/tmp/r.xml",
                "--testsuite",
                "unit"
            ])
        );
        assert!(envs.is_empty());
    }

    #[test]
    fn jest_keeps_the_default_reporter_and_sets_the_output_env() {
        let (full, envs) = inject(&command(&["jest"]), Preset::Jest, Path::new("/tmp/r.xml"));
        assert_eq!(
            full,
            command(&["jest", "--reporters=default", "--reporters=jest-junit"])
        );
        assert_eq!(
            envs,
            vec![("JEST_JUNIT_OUTPUT_FILE".to_owned(), "/tmp/r.xml".to_owned())]
        );
    }

    #[test]
    fn go_injects_junitfile_before_gotestsums_own_separator() {
        let (full, _) = inject(
            &command(&["gotestsum", "--", "./..."]),
            Preset::Go,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&["gotestsum", "--junitfile=/tmp/r.xml", "--", "./..."])
        );
    }

    fn names(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn phpunit_selection_anchors_the_deduped_names_in_a_filter_regex() {
        let (full, _) = inject_selected(
            &command(&["phpunit"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
            &names(&["test_it_journals_a_flag", "test_b"]),
        )
        .expect("phpunit supports selection");
        assert_eq!(
            full,
            command(&[
                "phpunit",
                "--log-junit",
                "/tmp/r.xml",
                "--filter",
                "/::(test_b|test_it_journals_a_flag)/",
            ])
        );
    }

    #[test]
    fn phpunit_selection_escapes_data_provider_row_names() {
        let (full, _) = inject_selected(
            &command(&["phpunit"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
            &names(&[r#"test_a with data set "x.y""#]),
        )
        .expect("phpunit supports selection");
        assert_eq!(
            full,
            command(&[
                "phpunit",
                "--log-junit",
                "/tmp/r.xml",
                "--filter",
                r#"/::(test_a with data set "x\.y")/"#,
            ])
        );
    }

    #[test]
    fn pytest_selection_uses_deduped_test_names_in_a_k_expression() {
        let (full, _) = inject_selected(
            &command(&["pytest"]),
            Preset::Pytest,
            Path::new("/tmp/r.xml"),
            &names(&["test_x", "test_x", "test_y"]),
        )
        .expect("pytest supports selection");
        assert_eq!(
            full,
            command(&["pytest", "--junit-xml=/tmp/r.xml", "-k", "test_x or test_y",])
        );
    }

    #[test]
    fn pytest_selection_strips_parameter_suffixes() {
        let (full, _) = inject_selected(
            &command(&["pytest"]),
            Preset::Pytest,
            Path::new("/tmp/r.xml"),
            &names(&["test_login[user with spaces]", "test_login[admin]"]),
        )
        .expect("pytest supports selection");
        assert_eq!(
            full,
            command(&["pytest", "--junit-xml=/tmp/r.xml", "-k", "test_login"])
        );
    }

    #[test]
    fn jest_selection_keeps_a_name_containing_double_colons_whole() {
        // A jest title may contain `::`; the selection must match the whole
        // name, never a re-split tail (#91).
        let (full, _) = inject_selected(
            &command(&["jest"]),
            Preset::Jest,
            Path::new("/tmp/r.xml"),
            &names(&["Config::load reads the env"]),
        )
        .expect("jest supports selection");
        assert_eq!(
            full,
            command(&[
                "jest",
                "--reporters=default",
                "--reporters=jest-junit",
                "-t",
                "(Config::load reads the env)",
            ])
        );
    }

    #[test]
    fn go_selection_is_declined_so_verification_refuses_loudly() {
        assert!(selection_args(Preset::Go, &names(&["TestX"])).is_none());
    }
}
