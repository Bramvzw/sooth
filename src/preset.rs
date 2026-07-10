//! Built-in presets: inject the right reporter flags into the wrapped test
//! command so it writes a JUnit-XML report sooth can parse.

use std::path::{Path, PathBuf};

use crate::cli::Preset;

/// What a preset adds to the wrapped command: extra arguments and environment
/// variables for the child process.
struct Injection {
    args: Vec<String>,
    envs: Vec<(String, String)>,
}

/// Where a preset-managed report goes: a per-process file in the system temp
/// directory, overwritten on every run and removed after parsing.
pub fn report_path() -> PathBuf {
    std::env::temp_dir().join(format!("sooth-junit-{}.xml", std::process::id()))
}

/// The full command to spawn and the environment to add: the user's program,
/// then the preset's injected arguments, then the user's own arguments.
///
/// The injected arguments go directly after the program name on purpose: they
/// must precede any `--` separator the runner itself understands (gotestsum
/// splits its own flags from `go test`'s at `--`).
pub fn inject(
    command: &[String],
    preset: Preset,
    report: &Path,
) -> (Vec<String>, Vec<(String, String)>) {
    let Injection { args, envs } = injection(preset, report);
    let mut full = Vec::with_capacity(command.len() + args.len());
    full.push(command[0].clone());
    full.extend(args);
    full.extend(command[1..].iter().cloned());
    (full, envs)
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
    use super::inject;
    use crate::cli::Preset;
    use std::path::Path;

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
}
