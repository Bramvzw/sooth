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

/// What sooth knows about the wrapped command under a preset, worked out
/// once: where its own arguments go, which program names the runner's
/// version, what a missing report most likely means. Injection, the
/// `--verify` selection and the gate all ask this, so a new command shape is
/// one entry here.
pub struct Plan {
    preset: Preset,
    shape: Shape,
    /// The user's command, with a `--` added when the shape needs one.
    command: Vec<String>,
    /// Sooth's arguments go in front of `command[point]`.
    point: usize,
}

/// The command shapes the presets know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// The program is the runner itself: the arguments go right after it.
    /// They must precede any `--` the runner understands (gotestsum splits
    /// its own flags from `go test`'s there).
    Runner,
    /// Laravel's `php artisan test` or `./artisan test`, with the index of
    /// `artisan`: Laravel forwards what follows the subcommand to Pest or
    /// `PHPUnit`, while `php` would parse `--log-junit` as its own option.
    Artisan(usize),
    /// A Composer script (`composer test`): Composer reads every option in
    /// front of a `--` as its own and hands what follows it to the script.
    /// The arguments go at the end, behind the user's own.
    Composer,
}

impl Plan {
    /// Read `command` as `preset` will run it.
    pub fn resolve(command: &[String], preset: Preset) -> Self {
        let mut command = command.to_vec();
        let shape = shape_of(&command, preset);
        let point = match shape {
            Shape::Runner => 1,
            Shape::Artisan(artisan) => artisan + 2,
            // Behind a `--`, wherever the user put theirs: `composer -- test`
            // is as valid as `composer test --`, and the end of the command
            // lies behind the `--` either way.
            Shape::Composer => {
                if !command.iter().any(|arg| arg == "--") {
                    command.push("--".to_owned());
                }
                command.len()
            }
        };
        Self {
            preset,
            shape,
            command,
            point,
        }
    }

    /// The command for one run and the environment to add: the reporter
    /// flags at the injection point, then the user's own arguments, then the
    /// gate's `files` when it selected any.
    pub fn run(&self, report: &Path, files: &[String]) -> Spawn {
        let Injection { args, envs } = injection(self.preset, report);
        let mut full = self.with(args);
        if !files.is_empty() {
            full.extend(selected_paths(self.preset, files));
        }
        (full, envs)
    }

    /// The command to re-run only the tests named `names`. `names` are raw
    /// `name` attributes straight from the report, never joined
    /// `classname::name` identities — a name may itself contain `::`, so
    /// re-splitting a joined id is lossy (#91). `None` when sooth cannot
    /// restrict this preset (see `DECISIONS.md`).
    pub fn rerun(&self, report: &Path, names: &[String]) -> Option<Spawn> {
        let selection = selection_args(self.preset, names)?;
        let Injection { mut args, envs } = injection(self.preset, report);
        args.extend(selection);
        Some((self.with(args), envs))
    }

    /// The preset this command runs under.
    pub fn preset(&self) -> Preset {
        self.preset
    }

    /// The command with `extra` at the injection point.
    fn with(&self, extra: Vec<String>) -> Vec<String> {
        let (head, tail) = self.command.split_at(self.point);
        head.iter()
            .cloned()
            .chain(extra)
            .chain(tail.iter().cloned())
            .collect()
    }

    /// `PHPUnit`'s major version, probed from `<version_probe> --version`.
    /// Pest prints its own banner, so for a program named `pest` the
    /// `phpunit` next to it — the one Pest runs on — is asked instead.
    /// `None` when the probe fails or no `PHPUnit` banner comes back — the
    /// caller decides how loud to be about not knowing.
    pub fn phpunit_major(&self) -> Option<u32> {
        let program = self.version_probe();
        probe_phpunit_major(&program).or_else(|| {
            let phpunit = pest_sibling(&program)?;
            probe_phpunit_major(&phpunit)
        })
    }

    /// The program whose `--version` names the `PHPUnit` that will run: the
    /// command's own program, or behind a wrapper the project's
    /// `vendor/bin/phpunit` — `artisan test --version` prints Laravel's
    /// version and `composer --version` Composer's.
    pub fn version_probe(&self) -> PathBuf {
        match self.shape {
            Shape::Runner => PathBuf::from(&self.command[0]),
            Shape::Artisan(artisan) => {
                Path::new(&self.command[artisan]).with_file_name("vendor/bin/phpunit")
            }
            Shape::Composer => PathBuf::from("vendor/bin/phpunit"),
        }
    }

    /// What a run that wrote no report most likely ran into, in this
    /// command's own terms.
    pub fn no_report_hint(&self) -> String {
        match self.shape {
            Shape::Runner => format!(
                "is the reporter available, and is `{}` the test runner itself rather \
                 than a wrapper like `python -m` or `npm`?",
                self.command[0]
            ),
            Shape::Artisan(_) => "`artisan test` hands `--log-junit` to PHPUnit or Pest, \
                                  so their output above should say why none was written"
                .to_owned(),
            Shape::Composer => "Composer hands the arguments after `--` to every command of \
                                the script, so the ones that do not run the tests need \
                                `@no_additional_args` — or wrap the runner itself instead \
                                of the script"
                .to_owned(),
        }
    }
}

/// Which shape `command` has. Only the phpunit preset knows wrappers: both
/// are PHP's, and each is one fixed, documented way of forwarding arguments.
fn shape_of(command: &[String], preset: Preset) -> Shape {
    if preset != Preset::Phpunit {
        return Shape::Runner;
    }
    if let Some(artisan) = artisan_test(command) {
        return Shape::Artisan(artisan);
    }
    if is_composer(command) {
        return Shape::Composer;
    }
    Shape::Runner
}

/// Whether the argument at `index` is a path to a file called one of `names`.
fn is_named(command: &[String], index: usize, names: &[&str]) -> bool {
    command
        .get(index)
        .and_then(|arg| Path::new(arg).file_name())
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| names.contains(&name))
}

/// The index of `artisan` in an `artisan test` command: the program itself,
/// or the script `php` runs.
fn artisan_test(command: &[String]) -> Option<usize> {
    let artisan = (0..2).find(|&index| is_named(command, index, &["artisan"]))?;
    (command.get(artisan + 1).map(String::as_str) == Some("test")).then_some(artisan)
}

/// Whether the program is Composer: `composer …`, or `php composer.phar …`.
/// Only the program position counts — `phpunit tests/composer` is a path.
fn is_composer(command: &[String]) -> bool {
    const COMPOSER: &[&str] = &["composer", "composer.phar"];
    is_named(command, 0, COMPOSER)
        || (is_named(command, 0, &["php"]) && is_named(command, 1, COMPOSER))
}

/// The selection flag(s) that restrict `preset` to the tests named `names`.
fn selection_args(preset: Preset, names: &[String]) -> Option<Vec<String>> {
    if names.iter().any(|name| !selectable(preset, name)) {
        return None;
    }
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

/// Whether a runner can be pointed at the test with this raw `name`. An
/// empty one — or, for pytest, one that is empty before its `[parameters]`
/// — turns `--filter '/::()/'`, `-t '()'` or `-k ''` into "every test".
pub fn selectable(preset: Preset, name: &str) -> bool {
    match preset {
        Preset::Pytest => !base_name(name).is_empty(),
        Preset::Phpunit | Preset::Jest | Preset::Go => !name.is_empty(),
    }
}

/// Whether `preset` can be restricted to a subset of tests.
pub fn supports_selection(preset: Preset) -> bool {
    selection_args(preset, &[]).is_some()
}

/// Whether `path` is a test file under `preset`'s conventions. Changed
/// source files are out of scope on purpose: the gate is about tests being
/// born, not test-impact analysis.
pub fn is_test_file(preset: Preset, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    match preset {
        Preset::Phpunit => name.ends_with("Test.php"),
        Preset::Pytest => match name.strip_suffix(".py") {
            Some(stem) => stem.starts_with("test_") || stem.ends_with("_test"),
            None => false,
        },
        // jest's default testMatch is `**/__tests__/**/*.[jt]s?(x)` plus
        // `.test.`/`.spec.` names anywhere.
        Preset::Jest => {
            name.contains(".test.")
                || name.contains(".spec.")
                || (path.split('/').any(|part| part == "__tests__") && has_jest_extension(name))
        }
        Preset::Go => name.ends_with("_test.go"),
    }
}

/// The extensions jest's default testMatch runs.
fn has_jest_extension(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, extension)| matches!(extension, "js" | "jsx" | "ts" | "tsx"))
}

/// The runner arguments selecting exactly the test files `files` — paths,
/// not name filters: every supported runner takes them, go included (as its
/// packages).
fn selected_paths(preset: Preset, files: &[String]) -> Vec<String> {
    match preset {
        Preset::Go => {
            let mut dirs: Vec<String> = files
                .iter()
                .map(|file| match file.rsplit_once('/') {
                    Some((dir, _)) => format!("./{dir}"),
                    None => "./.".to_owned(),
                })
                .collect();
            dirs.sort_unstable();
            dirs.dedup();
            dirs
        }
        // jest reads positional arguments as regexes matched against test
        // paths: unescaped, `app/(shop)/cart.test.tsx` matches nothing. The
        // `--` ends the injected `--reporters`, a yargs array option that
        // would otherwise take the first path as its next value.
        Preset::Jest => std::iter::once("--".to_owned())
            .chain(files.iter().map(|file| regex_escape(file)))
            .collect(),
        Preset::Phpunit | Preset::Pytest => files.to_vec(),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "The PHPUnit version probe runs the runner itself."
)]
fn probe_phpunit_major(program: &Path) -> Option<u32> {
    let output = std::process::Command::new(program)
        .arg("--version")
        .output()
        .ok()?;
    parse_phpunit_major(&String::from_utf8_lossy(&output.stdout))
}

/// `vendor/bin/pest` → `vendor/bin/phpunit`: Composer installs the `PHPUnit`
/// Pest depends on into the same bin directory.
fn pest_sibling(program: &Path) -> Option<PathBuf> {
    (program.file_name()? == "pest").then(|| program.with_file_name("phpunit"))
}

/// The major out of a `PHPUnit 9.6.11 by Sebastian Bergmann …` banner.
fn parse_phpunit_major(banner: &str) -> Option<u32> {
    let rest = banner.trim_start().strip_prefix("PHPUnit ")?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// The name without its `[parameters]` suffix (brackets break a `-k` expression).
fn base_name(name: &str) -> &str {
    name.split_once('[').map_or(name, |(base, _)| base)
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
        // jest-junit's default templates are `{classname} {title}` for both
        // attributes, which gives a test outside any `describe` a leading
        // space that `-t` then cannot match.
        Preset::Jest => Injection {
            args: vec![
                "--reporters=default".to_owned(),
                "--reporters=jest-junit".to_owned(),
            ],
            envs: vec![
                ("JEST_JUNIT_OUTPUT_FILE".to_owned(), report),
                ("JEST_JUNIT_CLASSNAME".to_owned(), "{classname}".to_owned()),
                ("JEST_JUNIT_TITLE".to_owned(), "{title}".to_owned()),
            ],
        },
        Preset::Go => Injection {
            args: vec![format!("--junitfile={report}")],
            envs: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{is_test_file, report_path, selected_paths, selection_args, Plan, Spawn};
    use crate::cli::Preset;
    use std::path::Path;

    fn inject(command: &[String], preset: Preset, report: &Path) -> Spawn {
        Plan::resolve(command, preset).run(report, &[])
    }

    fn inject_selected(
        command: &[String],
        preset: Preset,
        report: &Path,
        names: &[String],
    ) -> Option<Spawn> {
        Plan::resolve(command, preset).rerun(report, names)
    }

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
    fn under_php_artisan_test_the_flags_follow_the_subcommand() {
        // `php --log-junit …` is PHP's own option parser; Laravel forwards
        // what comes after `artisan test` to Pest or PHPUnit.
        let (full, _) = inject(
            &command(&["php", "artisan", "test", "--parallel"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&[
                "php",
                "artisan",
                "test",
                "--log-junit",
                "/tmp/r.xml",
                "--parallel"
            ])
        );
        let (full, _) = inject(
            &command(&["./artisan", "test"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&["./artisan", "test", "--log-junit", "/tmp/r.xml"])
        );
    }

    #[test]
    fn a_verify_re_run_under_artisan_selects_after_the_subcommand_too() {
        let (full, _) = inject_selected(
            &command(&["php", "artisan", "test"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
            &["test_x".to_owned()],
        )
        .expect("phpunit can select");
        assert_eq!(
            full,
            command(&[
                "php",
                "artisan",
                "test",
                "--log-junit",
                "/tmp/r.xml",
                "--filter",
                "/::(test_x)/"
            ])
        );
    }

    #[test]
    fn a_wrapper_is_asked_about_through_the_projects_phpunit() {
        let probe =
            |parts: &[&str]| Plan::resolve(&command(parts), Preset::Phpunit).version_probe();
        assert_eq!(
            probe(&["php", "artisan", "test"]),
            Path::new("vendor/bin/phpunit")
        );
        assert_eq!(
            probe(&["php", "app/artisan", "test"]),
            Path::new("app/vendor/bin/phpunit")
        );
        assert_eq!(
            probe(&["composer", "test"]),
            Path::new("vendor/bin/phpunit")
        );
        assert_eq!(
            probe(&["vendor/bin/phpunit"]),
            Path::new("vendor/bin/phpunit")
        );
    }

    #[test]
    fn under_a_composer_script_the_flags_follow_a_double_dash() {
        // Composer reads an option in front of `--` as its own; what follows
        // it goes to the script.
        let (full, _) = inject(
            &command(&["composer", "test"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&["composer", "test", "--", "--log-junit", "/tmp/r.xml"])
        );
        // The user's own `--` is the one that counts; sooth's arguments
        // follow theirs.
        let (full, _) = inject(
            &command(&[
                "composer",
                "run-script",
                "test-central",
                "--",
                "--group",
                "slow",
            ]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&[
                "composer",
                "run-script",
                "test-central",
                "--",
                "--group",
                "slow",
                "--log-junit",
                "/tmp/r.xml"
            ])
        );
        // Composer also takes the `--` in front of the script name: the
        // flags must not land where it looks for that name.
        let (full, _) = inject(
            &command(&["composer", "--", "test"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&["composer", "--", "test", "--log-junit", "/tmp/r.xml"])
        );
        let (full, _) = inject(
            &command(&["php", "composer.phar", "test"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&[
                "php",
                "composer.phar",
                "test",
                "--",
                "--log-junit",
                "/tmp/r.xml"
            ])
        );
    }

    #[test]
    fn a_composer_script_gets_the_selection_and_the_gates_files_after_the_double_dash() {
        let plan = Plan::resolve(&command(&["composer", "test"]), Preset::Phpunit);
        let (full, _) = plan
            .rerun(Path::new("/tmp/r.xml"), &["test_x".to_owned()])
            .expect("phpunit can select");
        assert_eq!(
            full,
            command(&[
                "composer",
                "test",
                "--",
                "--log-junit",
                "/tmp/r.xml",
                "--filter",
                "/::(test_x)/"
            ])
        );
        let (full, _) = plan.run(
            Path::new("/tmp/r.xml"),
            &["a/BTest.php".to_owned(), "c/DTest.php".to_owned()],
        );
        assert_eq!(
            full,
            command(&[
                "composer",
                "test",
                "--",
                "--log-junit",
                "/tmp/r.xml",
                "a/BTest.php",
                "c/DTest.php"
            ])
        );
    }

    #[test]
    fn composer_is_only_composer_in_the_program_position() {
        // A directory called `composer` is a path for the runner.
        let (full, _) = inject(
            &command(&["vendor/bin/phpunit", "tests/composer"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&[
                "vendor/bin/phpunit",
                "--log-junit",
                "/tmp/r.xml",
                "tests/composer"
            ])
        );
        // And only the phpunit preset reads `composer` as a wrapper.
        let (full, _) = inject(
            &command(&["composer", "test"]),
            Preset::Pytest,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&["composer", "--junit-xml=/tmp/r.xml", "test"])
        );
    }

    #[test]
    fn a_missing_report_is_explained_in_the_commands_own_terms() {
        let hint =
            |parts: &[&str]| Plan::resolve(&command(parts), Preset::Phpunit).no_report_hint();
        assert!(
            hint(&["vendor/bin/phpunit"]).contains("`vendor/bin/phpunit` the test runner itself")
        );
        assert!(hint(&["composer", "test"]).contains("@no_additional_args"));
        // `php` is not what the user thinks of as their runner.
        assert!(!hint(&["php", "artisan", "test"]).contains("`php`"));
    }

    #[test]
    fn the_gates_files_follow_the_users_own_arguments() {
        let (full, _) = Plan::resolve(
            &command(&["php", "artisan", "test", "--stop-on-failure"]),
            Preset::Phpunit,
        )
        .run(Path::new("/tmp/r.xml"), &["tests/ATest.php".to_owned()]);
        assert_eq!(
            full,
            command(&[
                "php",
                "artisan",
                "test",
                "--log-junit",
                "/tmp/r.xml",
                "--stop-on-failure",
                "tests/ATest.php"
            ])
        );
        // No files, no trace of the gate — jest's separator included.
        let (full, _) = inject(&command(&["jest"]), Preset::Jest, Path::new("/tmp/r.xml"));
        assert!(!full.contains(&"--".to_owned()));
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
            vec![
                ("JEST_JUNIT_OUTPUT_FILE".to_owned(), "/tmp/r.xml".to_owned()),
                ("JEST_JUNIT_CLASSNAME".to_owned(), "{classname}".to_owned()),
                ("JEST_JUNIT_TITLE".to_owned(), "{title}".to_owned()),
            ]
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

    #[test]
    fn the_phpunit_major_is_read_from_its_version_banner() {
        use super::parse_phpunit_major;
        assert_eq!(
            parse_phpunit_major("PHPUnit 9.6.11 by Sebastian Bergmann and contributors.\n"),
            Some(9)
        );
        assert_eq!(
            parse_phpunit_major("PHPUnit 13.2.4 by Sebastian Bergmann and contributors.\n"),
            Some(13)
        );
        // A foreign banner is not knowing, never a guess.
        assert_eq!(parse_phpunit_major("Pest 2.34.1\n"), None);
        assert_eq!(parse_phpunit_major(""), None);
    }

    #[test]
    fn pest_is_asked_about_through_the_phpunit_next_to_it() {
        use super::pest_sibling;
        assert_eq!(
            pest_sibling(Path::new("vendor/bin/pest")),
            Some(Path::new("vendor/bin/phpunit").to_path_buf())
        );
        assert_eq!(
            pest_sibling(Path::new("pest")),
            Some(Path::new("phpunit").to_path_buf())
        );
        assert_eq!(pest_sibling(Path::new("vendor/bin/phpunit")), None);
        assert_eq!(pest_sibling(Path::new("vendor/bin/pest-like")), None);
    }

    #[test]
    fn each_preset_recognizes_its_own_test_files() {
        assert!(is_test_file(Preset::Phpunit, "Modules/Hub/tests/ATest.php"));
        assert!(!is_test_file(Preset::Phpunit, "Modules/Hub/src/A.php"));
        assert!(is_test_file(Preset::Pytest, "tests/test_orders.py"));
        assert!(is_test_file(Preset::Pytest, "tests/orders_test.py"));
        assert!(!is_test_file(Preset::Pytest, "tests/conftest.py"));
        assert!(is_test_file(Preset::Jest, "src/cart.test.tsx"));
        assert!(is_test_file(Preset::Jest, "src/cart.spec.js"));
        assert!(!is_test_file(Preset::Jest, "src/cart.ts"));
        assert!(is_test_file(Preset::Go, "pkg/cart/cart_test.go"));
        assert!(!is_test_file(Preset::Go, "pkg/cart/cart.go"));
    }

    #[test]
    fn jest_recognizes_its_default_dunder_tests_convention() {
        assert!(is_test_file(Preset::Jest, "src/__tests__/cart.js"));
        assert!(is_test_file(Preset::Jest, "src/__tests__/deep/cart.tsx"));
        assert!(!is_test_file(Preset::Jest, "src/__tests__/fixtures.json"));
        assert!(!is_test_file(Preset::Jest, "src/__tests_almost__/cart.js"));
    }

    #[test]
    fn a_name_that_selects_nothing_in_particular_selects_nothing_at_all() {
        // `--filter '/::()/'`, `-t '()'` and `-k ''` all match every test.
        let named = "test_x".to_owned();
        for preset in [Preset::Phpunit, Preset::Jest, Preset::Pytest] {
            assert!(selection_args(preset, &[named.clone(), String::new()]).is_none());
            assert!(selection_args(preset, std::slice::from_ref(&named)).is_some());
        }
        // pytest selects by the name before its parameters.
        assert!(selection_args(Preset::Pytest, &["[only-params]".to_owned()]).is_none());
    }

    #[test]
    fn most_runners_take_the_paths_verbatim() {
        let files = vec!["a/BTest.php".to_owned(), "c/DTest.php".to_owned()];
        assert_eq!(selected_paths(Preset::Phpunit, &files), files);
    }

    #[test]
    fn jest_paths_are_escaped_because_jest_reads_them_as_regexes() {
        let files = vec!["app/(shop)/cart.test.tsx".to_owned()];
        assert_eq!(
            selected_paths(Preset::Jest, &files),
            vec![
                "--".to_owned(),
                r"app\/\(shop\)\/cart\.test\.tsx".to_owned()
            ]
        );
    }

    #[test]
    fn go_selects_deduplicated_package_dirs() {
        let files = vec![
            "pkg/cart/cart_test.go".to_owned(),
            "pkg/cart/totals_test.go".to_owned(),
            "root_test.go".to_owned(),
        ];
        assert_eq!(
            selected_paths(Preset::Go, &files),
            vec!["./.".to_owned(), "./pkg/cart".to_owned()]
        );
    }
}
