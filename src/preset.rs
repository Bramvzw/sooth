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

    /// The part of the user's command that reaches the runner: what
    /// follows the program, the `artisan test` subcommand, or Composer's `--`.
    fn runner_arguments(&self) -> &[String] {
        let from = match self.shape {
            Shape::Runner => 1,
            Shape::Artisan(artisan) => artisan + 2,
            Shape::Composer => self
                .command
                .iter()
                .position(|arg| arg == "--")
                .map_or(self.command.len(), |separator| separator + 1),
        };
        &self.command[from..]
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

    /// Why the gate cannot run this command, if it cannot. The gate hands
    /// the runner one path per changed test file; paratest — what
    /// `--parallel` runs under `artisan test` and Pest — accepts exactly one
    /// and stops on a second with "Too many arguments".
    pub fn gate_refusal(&self) -> Option<String> {
        const WHY: &str = "the gate gives the runner one path per changed test file, and \
                           paratest takes a single path";
        if self.preset != Preset::Phpunit {
            return None;
        }
        if self.shape == Shape::Runner && is_named(&self.command, 0, &["paratest"]) {
            return Some(format!(
                "{WHY}. Gate with the runner it drives (`vendor/bin/phpunit`) instead: a \
                 handful of files gains nothing from running in parallel"
            ));
        }
        let flag = self
            .runner_arguments()
            .iter()
            .find(|arg| matches!(arg.as_str(), "--parallel" | "-p"))?;
        Some(format!(
            "{WHY} — `{flag}` runs the tests through it. Drop `{flag}` from the gate's \
             command: a handful of files gains nothing from running in parallel"
        ))
    }

    /// A selection the user's command already carries, as seen from `dir`:
    /// the gate adds its files to it, so the runner would run the union
    /// while the gate names only its own. Reported only when certain — an
    /// argument that is no option, does not follow one that may be waiting
    /// for a value (`--bootstrap tests/x.php`), and names something that
    /// exists (for go: a package pattern). A selection behind a value-less
    /// flag (`pytest -q tests/`) is not told apart from a value and passes.
    pub fn own_selection(&self, dir: &Path) -> Option<&str> {
        let arguments = self.runner_arguments();
        arguments.iter().enumerate().find_map(|(index, argument)| {
            let follows_an_open_option = index
                .checked_sub(1)
                .and_then(|before| arguments.get(before))
                .is_some_and(|before| {
                    before.starts_with('-') && before != "--" && !before.contains('=')
                });
            if argument.starts_with('-') || follows_an_open_option {
                return None;
            }
            let selects = match self.preset {
                Preset::Go => argument.contains("...") || dir.join(argument).is_dir(),
                Preset::Phpunit | Preset::Pytest | Preset::Jest => dir.join(argument).exists(),
            };
            selects.then_some(argument.as_str())
        })
    }

    /// What a gate of several `files` that got no report should check
    /// first, for a Composer script: a `--parallel` inside the script is
    /// invisible to [`Plan::gate_refusal`], and paratest's own "Too many
    /// arguments" does not say which arguments. Any other shape has no
    /// script to hide one in — a visible `--parallel` was refused already.
    pub fn single_path_note(&self, files: usize) -> Option<String> {
        (self.shape == Shape::Composer && files > 1).then(|| {
            format!(
                "the gate passed {files} test files — if the script runs them through \
                 paratest (`--parallel`), that is what stopped it: paratest takes a \
                 single path"
            )
        })
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
/// or the script a PHP binary runs. Behind any other program (`sail artisan
/// test`, `ddev artisan test`) it is that program's argument: those run it
/// in a container, where a report path on this machine means nothing.
fn artisan_test(command: &[String]) -> Option<usize> {
    let artisan = script_position(command, &["artisan"])?;
    (command.get(artisan + 1).map(String::as_str) == Some("test")).then_some(artisan)
}

/// Whether the program is Composer: `composer …`, or `php composer.phar …`.
/// Only the program position counts — `phpunit tests/composer` is a path.
fn is_composer(command: &[String]) -> bool {
    script_position(command, &["composer", "composer.phar"]).is_some()
}

/// Where a script called one of `names` sits when it is what the command
/// runs: the program itself, or the first argument of a PHP binary.
fn script_position(command: &[String], names: &[&str]) -> Option<usize> {
    if is_named(command, 0, names) {
        Some(0)
    } else if command.first().is_some_and(|program| is_php(program)) && is_named(command, 1, names)
    {
        Some(1)
    } else {
        None
    }
}

/// Whether `program` is a PHP binary: `php`, `php8.4`, `/usr/bin/php`.
fn is_php(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|name| name.strip_prefix("php"))
        .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The preset a command names by its own program, with the words that name
/// it — for a run that came without `--preset` or `--junit`. A lookup, never
/// a guess: `None` for anything that only *might* run a known runner
/// (`composer test`, `npm test`, `sail artisan test`, a script), and for two
/// commands that do name one:
///
/// - `jest`, whose preset needs the `jest-junit` package: injecting its
///   reporter into a project without it breaks a command that works;
/// - a command that already asks for a report of its own (`--log-junit
///   build.xml`, or through the runner's environment): the runner writes
///   one report, and it would not be the one the user asked for.
pub fn infer(command: &[String]) -> Option<(Preset, String)> {
    infer_in(command, &|key| std::env::var(key).ok())
}

/// [`infer`], with the environment as a lookup.
fn infer_in(command: &[String], env: &dyn Fn(&str) -> Option<String>) -> Option<(Preset, String)> {
    let preset = named_runner(command)?;
    if preset == Preset::Jest || own_report(command, preset, env).is_some() {
        return None;
    }
    let words = match artisan_test(command) {
        Some(artisan) => command[..artisan + 2].join(" "),
        None => command[0].clone(),
    };
    Some((preset, words))
}

/// What would give a run without a report source one, as the flag to pass.
pub fn report_advice(command: &[String]) -> String {
    advice_in(command, &|key| std::env::var(key).ok())
}

/// [`report_advice`], with the environment as a lookup.
fn advice_in(command: &[String], env: &dyn Fn(&str) -> Option<String>) -> String {
    let named = named_runner(command);
    let written = named
        .and_then(|preset| own_report(command, preset, env))
        .filter(|path| !path.is_empty());
    if let Some(path) = written {
        return format!("pass `--junit {path}` — this command already writes that report");
    }
    if named == Some(Preset::Jest) {
        return "pass `--preset jest` (it needs the `jest-junit` package)".to_owned();
    }
    if is_composer(command) {
        return "pass `--preset phpunit` if this script runs PHPUnit or Pest".to_owned();
    }
    "pass `--junit <PATH>` with the JUnit report this command writes, or `--preset \
     <RUNNER>` (pytest, phpunit, jest, go) when the command is that runner itself"
        .to_owned()
}

/// The preset whose runner `command` is: the program's own name, or
/// Laravel's `artisan test`.
fn named_runner(command: &[String]) -> Option<Preset> {
    if artisan_test(command).is_some() || is_named(command, 0, &["phpunit", "pest", "paratest"]) {
        Some(Preset::Phpunit)
    } else if is_named(command, 0, &["pytest", "py.test"]) {
        Some(Preset::Pytest)
    } else if is_named(command, 0, &["jest"]) {
        Some(Preset::Jest)
    } else if is_named(command, 0, &["gotestsum"]) {
        Some(Preset::Go)
    } else {
        None
    }
}

/// The path of the `JUnit` report `preset`'s runner is already asked for:
/// on the command line as `--flag PATH` or `--flag=PATH`, or where the
/// runner itself reads more of them — pytest takes extra arguments from
/// `PYTEST_ADDOPTS`, gotestsum its `--junitfile` from `GOTESTSUM_JUNITFILE`.
/// Empty when the flag is there without a path. A report set up in a config
/// file (`phpunit.xml`, `pytest.ini`) is not seen: sooth reads no project
/// configuration.
fn own_report(
    command: &[String],
    preset: Preset,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let flags: &[&str] = match preset {
        Preset::Phpunit => &["--log-junit"],
        Preset::Pytest => &["--junit-xml", "--junitxml"],
        Preset::Go => &["--junitfile"],
        // jest-junit takes its path from the environment.
        Preset::Jest => &[],
    };
    let among = |arguments: &[String]| {
        arguments.iter().enumerate().find_map(|(index, arg)| {
            flags.iter().find_map(|flag| {
                if arg == flag {
                    Some(arguments.get(index + 1).cloned().unwrap_or_default())
                } else {
                    arg.strip_prefix(flag)?.strip_prefix('=').map(str::to_owned)
                }
            })
        })
    };
    among(command).or_else(|| match preset {
        Preset::Pytest => {
            let extra: Vec<String> = env("PYTEST_ADDOPTS")?
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            among(&extra)
        }
        Preset::Go => env("GOTESTSUM_JUNITFILE").filter(|path| !path.is_empty()),
        Preset::Phpunit | Preset::Jest => None,
    })
}

/// The selection flag(s) that restrict `preset` to the tests named `names`.
fn selection_args(preset: Preset, names: &[String]) -> Option<Vec<String>> {
    if names.iter().any(|name| !selectable(preset, name)) {
        return None;
    }
    match preset {
        // The JUnit classname is dotted but --filter wants the backslashed
        // FQCN; only the name half survives both (#93). And that half is
        // matched in any spelling the report may have given it: see
        // `any_spelling`.
        Preset::Phpunit => {
            let mut methods: Vec<String> = names.iter().map(|name| any_spelling(name)).collect();
            methods.sort_unstable();
            methods.dedup();
            Some(vec![
                "--filter".to_owned(),
                format!("/::(?:test_?)?({})/i", methods.join("|")),
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
        // Anchored, so a file called `--bootstrap=xTest.php` or
        // `-k_test.py` reaches the runner as the path it is and never as an
        // option. git's paths are always relative, so `./` is always right.
        Preset::Phpunit | Preset::Pytest => files.iter().map(|file| format!("./{file}")).collect(),
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

/// A `PHPUnit` test name as a pattern that also matches the method it may
/// have been prettified from. Pest reports a class-based test the way
/// `PHPUnit`'s `TestDox` prints it: `test_that_true_is_true`,
/// `testThatTrueIsTrue` and an attribute-marked `that_true_is_true` all
/// become `That true is true`. So every space may have been an underscore or
/// a camelCase hump; with the optional `test` prefix and the `i` flag of the
/// enclosing filter, the reported name finds its method again. A name that
/// was never prettified still matches itself.
fn any_spelling(name: &str) -> String {
    name.split(' ')
        .map(regex_escape)
        .collect::<Vec<_>>()
        .join("[_ ]?")
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
                "/::(?:test_?)?(test_x)/i"
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
                "/::(?:test_?)?(test_x)/i"
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
                "./a/BTest.php",
                "./c/DTest.php"
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
    fn the_gate_refuses_a_command_that_runs_through_paratest() {
        let refusal =
            |parts: &[&str], preset| Plan::resolve(&command(parts), preset).gate_refusal();
        for parallel in [
            &["php", "artisan", "test", "--parallel"][..],
            &["php", "artisan", "test", "--stop-on-failure", "-p"][..],
            &["vendor/bin/pest", "--parallel"][..],
            &["composer", "test", "--", "--parallel"][..],
            &["composer", "--", "test", "-p"][..],
        ] {
            let reason = refusal(parallel, Preset::Phpunit).expect("paratest takes one path");
            assert!(reason.contains("Drop `"), "got: {reason}");
        }
        // paratest itself has no flag to drop.
        let reason = refusal(&["vendor/bin/paratest"], Preset::Phpunit).expect("paratest");
        assert!(reason.contains("vendor/bin/phpunit") && !reason.contains("Drop"));

        for serial in [
            &["php", "artisan", "test"][..],
            &["vendor/bin/phpunit", "--stop-on-failure"][..],
            &["composer", "test"][..],
        ] {
            assert_eq!(refusal(serial, Preset::Phpunit), None);
        }
        // What a script hides cannot be refused, only named afterwards — and
        // one file is a path paratest takes.
        let hidden = Plan::resolve(&command(&["composer", "test"]), Preset::Phpunit);
        assert!(hidden
            .single_path_note(2)
            .is_some_and(|note| note.contains("2 test files")));
        assert_eq!(hidden.single_path_note(1), None);
        // Outside a script there is nowhere to hide one.
        let visible = Plan::resolve(&command(&["php", "artisan", "test"]), Preset::Phpunit);
        assert_eq!(visible.single_path_note(2), None);
        // Another runner's `-p` is another option: pytest loads a plugin with it.
        assert_eq!(
            refusal(&["pytest", "-p", "no:cacheprovider"], Preset::Pytest),
            None
        );
    }

    /// No environment: what the command says is all there is.
    fn bare(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_command_that_names_its_runner_names_its_preset() {
        use super::infer_in;
        for (parts, preset, words) in [
            (&["pytest", "-k", "x"][..], Preset::Pytest, "pytest"),
            (
                &["vendor/bin/phpunit"][..],
                Preset::Phpunit,
                "vendor/bin/phpunit",
            ),
            (
                &["vendor/bin/pest", "--parallel"][..],
                Preset::Phpunit,
                "vendor/bin/pest",
            ),
            (
                &["vendor/bin/paratest"][..],
                Preset::Phpunit,
                "vendor/bin/paratest",
            ),
            (
                &["php", "artisan", "test", "--parallel"][..],
                Preset::Phpunit,
                "php artisan test",
            ),
            (
                &["/opt/homebrew/bin/php8.4", "artisan", "test"][..],
                Preset::Phpunit,
                "/opt/homebrew/bin/php8.4 artisan test",
            ),
            (
                &["./artisan", "test"][..],
                Preset::Phpunit,
                "./artisan test",
            ),
            (&["gotestsum", "--", "./..."][..], Preset::Go, "gotestsum"),
        ] {
            assert_eq!(
                infer_in(&command(parts), &bare),
                Some((preset, words.to_owned())),
                "{parts:?}"
            );
        }
    }

    #[test]
    fn a_command_that_only_might_run_a_runner_names_nothing() {
        use super::infer_in;
        for parts in [
            // A script can hide any command.
            &["composer", "test"][..],
            &["npm", "test"][..],
            &["make", "test"][..],
            &["python", "-m", "pytest"][..],
            // `php artisan` without `test` is not a test command, and a
            // runner's name further down the line is an argument.
            &["php", "artisan", "migrate"][..],
            &["sh", "-c", "pytest"][..],
            // Sail, DDEV and Lando run `artisan test` in a container: a
            // report path on this machine means nothing there.
            &["vendor/bin/sail", "artisan", "test"][..],
            &["ddev", "artisan", "test"][..],
            &["docker", "compose", "exec", "app", "php", "artisan", "test"][..],
            // jest's preset needs a package the project may not have.
            &["jest"][..],
            &["node_modules/.bin/jest", "--ci"][..],
        ] {
            assert_eq!(infer_in(&command(parts), &bare), None, "{parts:?}");
        }
    }

    #[test]
    fn a_command_that_writes_its_own_report_is_not_given_a_second_one() {
        use super::{advice_in, infer_in};
        for (parts, path) in [
            (
                &["vendor/bin/phpunit", "--log-junit", "build/junit.xml"][..],
                "build/junit.xml",
            ),
            (
                &["php", "artisan", "test", "--log-junit=out.xml"][..],
                "out.xml",
            ),
            (&["pytest", "--junitxml=r.xml"][..], "r.xml"),
            (&["pytest", "--junit-xml", "r.xml"][..], "r.xml"),
            (
                &["gotestsum", "--junitfile", "unit.xml", "--", "./..."][..],
                "unit.xml",
            ),
        ] {
            assert_eq!(infer_in(&command(parts), &bare), None, "{parts:?}");
            assert!(
                advice_in(&command(parts), &bare).contains(&format!("`--junit {path}`")),
                "{parts:?}"
            );
        }
        // A flag that only starts the same way is another flag.
        for parts in [
            &["vendor/bin/phpunit", "--log-junit-foo", "x"][..],
            &["pytest", "--junitxml-something", "x"][..],
        ] {
            assert!(infer_in(&command(parts), &bare).is_some(), "{parts:?}");
        }
    }

    #[test]
    fn a_report_flag_without_a_path_stops_the_inference_but_names_no_path() {
        use super::{advice_in, infer_in};
        // The flag is there, so the command is left as typed; there is no
        // path to hand to `--junit`.
        for parts in [
            &["vendor/bin/phpunit", "--log-junit="][..],
            &["vendor/bin/phpunit", "--log-junit"][..],
        ] {
            assert_eq!(infer_in(&command(parts), &bare), None, "{parts:?}");
            let advice = advice_in(&command(parts), &bare);
            assert!(advice.contains("`--junit <PATH>`"), "{parts:?}: {advice}");
        }
    }

    #[test]
    fn a_report_the_environment_asks_for_counts_as_the_commands_own() {
        use super::{advice_in, infer_in};
        // pytest reads more arguments from PYTEST_ADDOPTS; gotestsum takes
        // its --junitfile default from GOTESTSUM_JUNITFILE. Sooth's flag
        // would override both, and the file the user asked for would go
        // missing.
        let pytest =
            |key: &str| (key == "PYTEST_ADDOPTS").then(|| "-q --junitxml=ci.xml".to_owned());
        assert_eq!(infer_in(&command(&["pytest"]), &pytest), None);
        assert!(advice_in(&command(&["pytest"]), &pytest).contains("`--junit ci.xml`"));

        let go = |key: &str| (key == "GOTESTSUM_JUNITFILE").then(|| "unit.xml".to_owned());
        assert_eq!(infer_in(&command(&["gotestsum"]), &go), None);
        assert!(advice_in(&command(&["gotestsum"]), &go).contains("`--junit unit.xml`"));

        // Another runner's variable is not this runner's report.
        assert!(infer_in(&command(&["vendor/bin/phpunit"]), &pytest).is_some());
        let unrelated = |key: &str| (key == "PYTEST_ADDOPTS").then(|| "-q -x".to_owned());
        assert!(infer_in(&command(&["pytest"]), &unrelated).is_some());
    }

    #[test]
    fn the_advice_names_the_flag_that_fits_the_command() {
        use super::advice_in;
        let jest = advice_in(&command(&["jest"]), &bare);
        assert!(jest.contains("`--preset jest`") && jest.contains("jest-junit"));
        // The phpunit preset can drive a Composer script, on the user's word.
        let composer = advice_in(&command(&["composer", "test"]), &bare);
        assert!(composer.contains("`--preset phpunit`") && composer.contains("if this script"));
        // Anything else: a preset only helps when the command is the runner.
        for parts in [
            &["npm", "test"][..],
            &["cargo", "test"][..],
            &["make", "test"][..],
        ] {
            let advice = advice_in(&command(parts), &bare);
            assert!(
                advice.contains("`--junit <PATH>`")
                    && advice.contains("`--preset <RUNNER>`")
                    && advice.contains("runner itself"),
                "{parts:?}: {advice}"
            );
        }
    }

    #[test]
    fn artisan_behind_another_program_is_that_programs_business() {
        // `sail artisan test` runs in a container; only `php` (any version)
        // or `artisan` itself makes the command Laravel's own.
        let (full, _) = inject(
            &command(&["vendor/bin/sail", "artisan", "test"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&[
                "vendor/bin/sail",
                "--log-junit",
                "/tmp/r.xml",
                "artisan",
                "test"
            ])
        );
        let (full, _) = inject(
            &command(&["php8.3", "artisan", "test"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
        );
        assert_eq!(
            full,
            command(&["php8.3", "artisan", "test", "--log-junit", "/tmp/r.xml"])
        );
    }

    #[test]
    fn a_selection_of_the_commands_own_is_seen_only_when_it_is_certain() {
        let report = report_path().unwrap();
        let dir = report.parent().expect("a private scratch dir");
        std::fs::create_dir_all(dir.join("tests/Unit")).unwrap();
        std::fs::write(dir.join("phpunit.xml"), "").unwrap();
        let own = |parts: &[&str], preset| {
            Plan::resolve(&command(parts), preset)
                .own_selection(dir)
                .map(str::to_owned)
        };

        for (parts, preset, selection) in [
            (
                &["vendor/bin/phpunit", "tests"][..],
                Preset::Phpunit,
                "tests",
            ),
            (
                &["php", "artisan", "test", "tests/Unit"][..],
                Preset::Phpunit,
                "tests/Unit",
            ),
            (
                &["composer", "test", "--", "tests"][..],
                Preset::Phpunit,
                "tests",
            ),
            (&["pytest", "tests", "-q"][..], Preset::Pytest, "tests"),
            (
                &["pytest", "--maxfail=1", "tests"][..],
                Preset::Pytest,
                "tests",
            ),
            (&["gotestsum", "--", "./..."][..], Preset::Go, "./..."),
        ] {
            assert_eq!(own(parts, preset).as_deref(), Some(selection), "{parts:?}");
        }
        for (parts, preset) in [
            (&["vendor/bin/phpunit"][..], Preset::Phpunit),
            // An option's value, even when it exists.
            (
                &["vendor/bin/phpunit", "-c", "phpunit.xml"][..],
                Preset::Phpunit,
            ),
            (
                &["vendor/bin/phpunit", "--testsuite", "tests"][..],
                Preset::Phpunit,
            ),
            // Not there, so not a path: a `--filter` pattern, say.
            (
                &["vendor/bin/phpunit", "--filter=x", "nothing-here"][..],
                Preset::Phpunit,
            ),
            // Composer's script name is not the runner's argument.
            (&["composer", "tests"][..], Preset::Phpunit),
            // The known blind spot: a value-less flag in front.
            (&["pytest", "-q", "tests"][..], Preset::Pytest),
            (&["gotestsum", "--", "-run", "TestX"][..], Preset::Go),
        ] {
            assert_eq!(own(parts, preset), None, "{parts:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
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
                "./tests/ATest.php"
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
                "/::(?:test_?)?(test_b|test_it_journals_a_flag)/i",
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
                r#"/::(?:test_?)?(test_a[_ ]?with[_ ]?data[_ ]?set[_ ]?"x\.y")/i"#,
            ])
        );
    }

    #[test]
    fn phpunit_selection_lets_a_space_be_an_underscore_or_a_camel_hump() {
        // Pest reports `test_that_true_is_true` as `That true is true`.
        let (full, _) = inject_selected(
            &command(&["vendor/bin/pest"]),
            Preset::Phpunit,
            Path::new("/tmp/r.xml"),
            &names(&["That true is true", "Http 2 works"]),
        )
        .expect("phpunit supports selection");
        assert_eq!(
            full.last().map(String::as_str),
            Some("/::(?:test_?)?(Http[_ ]?2[_ ]?works|That[_ ]?true[_ ]?is[_ ]?true)/i")
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
    fn phpunit_and_pytest_paths_are_anchored_so_none_can_read_as_an_option() {
        let files = vec!["a/BTest.php".to_owned(), "--bootstrap=xTest.php".to_owned()];
        assert_eq!(
            selected_paths(Preset::Phpunit, &files),
            vec![
                "./a/BTest.php".to_owned(),
                "./--bootstrap=xTest.php".to_owned()
            ]
        );
        assert_eq!(
            selected_paths(Preset::Pytest, &["-k_test.py".to_owned()]),
            vec!["./-k_test.py".to_owned()]
        );
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
