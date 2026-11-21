//! Command-line interface definitions for `sooth`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Parsed top-level command line.
#[derive(Debug, Parser)]
#[command(name = "sooth", version, about = "The truth about your tests")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands `sooth` understands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a test command and report what your tests actually do.
    Run(RunArgs),

    /// Classify the failures in a JUnit-XML report against what sooth already
    /// knows: the run history and the quarantine list. Runs no tests.
    Explain(ExplainArgs),

    /// Bring JUnit-XML reports produced elsewhere (a CI artifact, a
    /// colleague's run) into the local run history. Runs no tests.
    Import(ImportArgs),

    /// Classify the accumulated run history and print it. Runs no tests,
    /// records nothing.
    History(HistoryArgs),

    /// Run the suite in its own order and in a shuffled one, and report the
    /// tests whose result depends on the order.
    Order(OrderArgs),
}

/// Arguments for `sooth order`.
#[derive(Debug, Args)]
pub struct OrderArgs {
    /// Replay the shuffled order of an earlier pass.
    ///
    /// Every pass prints the seed of its shuffle. The same seed gives the
    /// same order again, so a finding can be looked at twice. Left out, a
    /// new seed is picked.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=2_147_483_647))]
    pub seed: Option<u32>,

    /// Preset for a known runner.
    ///
    /// The order pass asks the runner itself for a shuffled order, so it
    /// needs one sooth knows how to ask: phpunit (also Pest, `php artisan
    /// test` and Composer scripts) or jest. Left out, the preset is read
    /// from a command that names its runner.
    #[arg(long, value_enum)]
    pub preset: Option<Preset>,

    /// Emit JSON: bare as the last stdout line, or `--json=PATH` to a file.
    ///
    /// Bare `--json` prints the document as sooth's only stdout line;
    /// `--json=PATH` writes it to a file and keeps the human report on
    /// stdout (note the `=`).
    #[expect(
        clippy::option_option,
        reason = "Option<Option<_>> is clap's flag-with-optional-value shape, as on `run`."
    )]
    #[arg(long, value_name = "PATH", num_args = 0..=1, require_equals = true)]
    pub json: Option<Option<PathBuf>>,

    /// Do not record the run in the suite's own order, and do not read the
    /// history.
    ///
    /// That run is an ordinary run and is recorded like one; the shuffled
    /// runs never are. The history is also what tells a known flake from a
    /// test that depends on the order.
    #[arg(long)]
    pub no_history: bool,

    /// Color the report.
    ///
    /// `auto` respects `NO_COLOR` and whether stdout is a terminal.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorChoice,

    /// The test command to run, given after `--` (e.g. `sooth order -- php
    /// artisan test`).
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

/// Arguments for `sooth import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Where the reports were produced, in your own words (`ci`, `staging`).
    ///
    /// Nothing in a JUnit-XML file says where it ran, so sooth records your
    /// label rather than a guess. It is what tells "fails only in ci" from
    /// "fails everywhere". With `--github` it defaults to `ci`.
    #[arg(long, value_name = "LABEL", required_unless_present = "github")]
    pub env: Option<String>,

    /// Read the repository's GitHub Actions reruns instead of files.
    ///
    /// A job that failed and passed when it was run again, on the same
    /// commit, is the strongest flake evidence a CI holds. sooth reads the
    /// runs that had more than one attempt through your own `gh` and its
    /// login, checks that both attempts ran the same tests on the same code,
    /// and records the failures and — marked as read from counts, not seen
    /// by name — the passes. Push runs only, for now.
    #[arg(long, conflicts_with_all = ["commit", "replace", "log"])]
    pub github: bool,

    /// The repository `--github` reads, as `OWNER/NAME`; by default the one
    /// `gh` finds for this directory.
    #[arg(long, value_name = "OWNER/NAME", requires = "github")]
    pub repo: Option<String>,

    /// How far back `--github` reads, as `YYYY-MM-DD`; by default 90 days,
    /// GitHub's own default for keeping logs.
    #[arg(long, value_name = "DATE", requires = "github")]
    pub since: Option<String>,

    /// With `--github`: say what would be recorded, and record nothing.
    #[arg(long, requires = "github")]
    pub dry_run: bool,

    /// The commit the reports were produced on, from a clean checkout.
    ///
    /// Without it the observations count in the totals but can never be
    /// evidence: proving a flake needs to know which code ran, and for a
    /// report that is its commit. An abbreviated sha is fine; one your clone
    /// does not know is refused.
    #[arg(long, value_name = "SHA")]
    pub commit: Option<String>,

    /// Import a file again, in place of its earlier observations.
    ///
    /// A file already imported is skipped. With this flag the observations
    /// of that earlier import are removed from the history and the file is
    /// recorded anew: the way to correct a wrong --commit or --env.
    #[arg(long)]
    pub replace: bool,

    /// Read the files as console logs of this format, not JUnit-XML reports.
    ///
    /// A log names only its failures, so only failures are recorded; the
    /// passes stay anonymous dots.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub log: Option<LogFormat>,

    /// Color the report.
    ///
    /// `auto` respects `NO_COLOR` and whether stdout is a terminal.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorChoice,

    /// The JUnit-XML report files (or, with `--log`, console logs) to import.
    #[arg(
        value_name = "REPORT",
        required_unless_present = "github",
        conflicts_with = "github"
    )]
    pub reports: Vec<PathBuf>,
}

/// Console-log formats `import --log` can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    /// `PHPUnit`'s own console output (`vendor/bin/phpunit`, paratest, and
    /// `php artisan test --parallel` in a `PHPUnit` project, which runs
    /// paratest).
    Phpunit,
    /// Collision's console output: `php artisan test`, and Pest with or
    /// without `--parallel`. Collision prints a test's description, not its
    /// name, so a failure is named only when the history already knows
    /// exactly one test it describes.
    Collision,
}

/// Arguments for `sooth history`.
#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// Drop the observations the analysis no longer reads.
    ///
    /// Rewrites `.sooth/history.jsonl`, keeping each test's last 50
    /// invocations in which it ran — the window every analysis looks at —
    /// and removing what is older. The history only grows otherwise.
    #[arg(long)]
    pub prune: bool,

    /// Color the report.
    ///
    /// `auto` respects `NO_COLOR` and whether stdout is a terminal.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorChoice,
}

/// Arguments for `sooth explain`.
#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// The JUnit-XML report to explain.
    ///
    /// The one your failing run wrote. An older file is fine here, unlike
    /// `sooth run --junit`.
    #[arg(long, value_name = "PATH", required = true)]
    pub junit: PathBuf,

    /// Emit JSON instead of the human report.
    ///
    /// The document is the whole output: no wrapped command shares this
    /// stdout.
    #[arg(long)]
    pub json: bool,

    /// Color the report.
    ///
    /// `auto` respects `NO_COLOR` and whether stdout is a terminal.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorChoice,
}

/// Arguments for `sooth run`.
#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each bool is one independent command-line flag, which is how clap's derive models a switch."
)]
pub struct RunArgs {
    /// Preset for a known runner.
    ///
    /// A preset injects the right reporter flags and reads the report the
    /// runner writes (mutually exclusive with --junit). The command after
    /// `--` must be the runner itself (pytest, phpunit, pest, jest,
    /// gotestsum) — not a wrapper like `python -m pytest` or `npm test`. The
    /// exceptions are Laravel's `php artisan test` and Composer scripts
    /// (`composer test`), which the phpunit preset understands. Left out,
    /// the preset is read from a command that names its runner (`pytest`,
    /// `vendor/bin/pest`, `php artisan test`); `pest` is accepted as a name
    /// for `phpunit`.
    #[arg(long, value_enum, conflicts_with = "junit")]
    pub preset: Option<Preset>,

    /// How many times to run the suite, in a fixed order.
    ///
    /// With a report source and more than one run, mixed outcomes are
    /// reported as flaky; the totals line reflects the final run. Repeated
    /// runs are kept quiet (see --show-output). How many you need for a
    /// given failure rate: see "How many runs do I need?" in the README.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    pub runs: u32,

    /// Emit JSON: bare as the last stdout line, or `--json=PATH` to a file.
    ///
    /// Requires --junit or a preset. Bare `--json` prints the document as
    /// sooth's final stdout line; `--json=PATH` writes it to a file and keeps
    /// the human report on stdout (note the `=`; `--json PATH` with a space
    /// is rejected).
    #[expect(
        clippy::option_option,
        reason = "Option<Option<_>> is clap's canonical shape for a flag with an optional value: None = absent, Some(None) = bare, Some(Some(p)) = path."
    )]
    #[arg(long, value_name = "PATH", num_args = 0..=1, require_equals = true)]
    pub json: Option<Option<PathBuf>>,

    /// Show the test command's output for every run of `--runs N`.
    ///
    /// Without it, repeated runs are quiet: one line per run, and the output
    /// of the first run that failed. A single run always shows its output.
    #[arg(long)]
    pub show_output: bool,

    /// Do not record this run, and do not report from the history.
    ///
    /// Nothing is written to `.sooth/history.jsonl` and the accumulated
    /// history is not read (recording needs a report source; without one
    /// nothing is recorded either way).
    #[arg(long)]
    pub no_history: bool,

    /// The pre-push gate: run only the test files changed against BASE.
    ///
    /// Selects the test files that are new or changed against BASE (default:
    /// `@{upstream}`, else `origin/HEAD`) and repeats them `--runs` times. A
    /// handful of tests 20 times is seconds, and catches a flake where it is
    /// born. Needs a preset and an explicit --runs. Note the `=`:
    /// `--changed=origin/main`.
    #[expect(
        clippy::option_option,
        reason = "Option<Option<_>> is clap's flag-with-optional-value shape, as --json."
    )]
    #[arg(long, value_name = "BASE", num_args = 0..=1, require_equals = true)]
    pub changed: Option<Option<String>>,

    /// After a failing run, re-run only the failed tests.
    ///
    /// Classifies each failure as real or flaky/order-dependent. Needs a
    /// preset and a single run.
    #[arg(long)]
    pub verify: bool,

    /// Exit 0 when only tests listed in `.sooth-quarantine` failed.
    ///
    /// Failures of quarantined tests are pardoned — reported loudly, but the
    /// run exits 0 when nothing else failed (requires a report source).
    #[arg(long)]
    pub fail_on_flaky: bool,

    /// Color the report.
    ///
    /// `auto` respects `NO_COLOR` and whether stdout is a terminal.
    #[arg(long, value_enum, default_value = "auto")]
    pub color: ColorChoice,

    /// How many of the slowest tests to show.
    ///
    /// Default 10, listed when the suite has more tests than that; requires
    /// --junit or a preset.
    #[arg(long, value_parser = positive_usize)]
    pub slowest: Option<usize>,

    /// Path to the JUnit-XML report the test command writes.
    ///
    /// sooth parses it after the run and reports totals, status counts and
    /// the slowest tests; a file that predates the run is rejected as stale.
    #[arg(long, value_name = "PATH")]
    pub junit: Option<PathBuf>,

    /// The test command to run, given after `--` (e.g. `sooth run -- pytest`).
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

/// A usage error with what the user most likely meant, for the two slips
/// clap can only call an "unexpected argument": a test command without the
/// `--` in front of it, and a value given to `--json` or `--changed` with a
/// space instead of an `=`. `argv` is the command line as typed.
pub fn with_tip(mut error: clap::Error, argv: &[String]) -> clap::Error {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if error.kind() != ErrorKind::UnknownArgument {
        return error;
    }
    let Some(ContextValue::String(unexpected)) = error.get(ContextKind::InvalidArg).cloned() else {
        return error;
    };
    if let Some(tip) = tip_for(argv, &unexpected) {
        error.insert(
            ContextKind::Suggested,
            ContextValue::StyledStrs(vec![tip.into()]),
        );
    }
    error
}

/// The tip for the argument clap did not expect, if it is one of the two
/// slips [`with_tip`] names. Only `sooth run` and `sooth order` take a
/// command or these flags, an unknown `--flag` is neither slip, and the
/// word may stand in more than one place on the line (`--junit pytest
/// pytest`): every place is tried, and a corrected command is offered only
/// if it parses.
fn tip_for(argv: &[String], unexpected: &str) -> Option<String> {
    let takes_a_command = matches!(argv.get(1).map(String::as_str), Some("run" | "order"));
    if !takes_a_command || unexpected.starts_with('-') {
        return None;
    }
    let places: Vec<usize> = (2..argv.len())
        .filter(|&at| argv[at] == unexpected)
        .collect();
    let spaced_value = places.iter().find_map(|&at| match argv[at - 1].as_str() {
        flag @ ("--json" | "--changed") => Some(flag),
        _ => None,
    });
    if let Some(flag) = spaced_value {
        return Some(format!(
            "`{flag}` takes its value with an `=`: `{flag}={unexpected}`"
        ));
    }
    if argv.iter().any(|arg| arg == "--") {
        return None;
    }
    places
        .iter()
        .map(|&at| {
            let mut corrected = argv[..at].to_vec();
            corrected.push("--".to_owned());
            corrected.extend_from_slice(&argv[at..]);
            corrected
        })
        .find(|corrected| Cli::try_parse_from(corrected).is_ok())
        .map(|corrected| {
            format!(
                "the test command goes after `--`: `sooth {}`",
                corrected[1..].join(" ")
            )
        })
}

/// Parse a strictly positive count: `--slowest 0` would silently hide the
/// ranking, and silent is not this tool's style (mirrors the `--runs` guard).
fn positive_usize(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(0) => Err("must be at least 1".to_owned()),
        Ok(parsed) => Ok(parsed),
        Err(err) => Err(err.to_string()),
    }
}

/// A built-in preset for a known test runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Preset {
    Pytest,
    /// Pest runs on `PHPUnit` and takes the same flags, so `--preset pest`
    /// is this preset under the name its users look for.
    #[value(alias = "pest")]
    Phpunit,
    Jest,
    Go,
}

impl Preset {
    /// The name `--preset` takes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Pytest => "pytest",
            Self::Phpunit => "phpunit",
            Self::Jest => "jest",
            Self::Go => "go",
        }
    }
}

/// When to color the human report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[cfg(test)]
mod tests {
    use super::{Cli, ColorChoice, Command, Preset};
    use clap::Parser;

    fn parse_run_args(cmdline: &[&str]) -> super::RunArgs {
        let parsed = Cli::try_parse_from(cmdline).unwrap();
        let Command::Run(args) = parsed.command else {
            panic!("expected `run`");
        };
        args
    }

    #[test]
    fn captures_the_command_after_double_dash() {
        let args = parse_run_args(&["sooth", "run", "--", "pytest", "-k", "foo"]);
        assert_eq!(args.command, ["pytest", "-k", "foo"].map(String::from));
        assert_eq!(args.runs, 1);
        assert_eq!(args.slowest, None);
        assert_eq!(args.preset, None);
        assert_eq!(args.json, None);
        assert_eq!(args.junit, None);
        assert_eq!(args.color, ColorChoice::Auto);
    }

    #[test]
    fn parses_flags_and_preset() {
        let args = parse_run_args(&[
            "sooth",
            "run",
            "--preset",
            "pytest",
            "--runs",
            "5",
            "--json",
            "--slowest",
            "3",
            "--color",
            "never",
            "--",
            "pytest",
        ]);
        assert_eq!(args.preset, Some(Preset::Pytest));
        assert_eq!(args.runs, 5);
        assert_eq!(args.slowest, Some(3));
        assert_eq!(args.json, Some(None));
        assert_eq!(args.color, ColorChoice::Never);
    }

    fn typed(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn a_command_without_its_double_dash_is_shown_where_it_goes() {
        use super::tip_for;
        assert_eq!(
            tip_for(&typed("sooth run php artisan test"), "php").as_deref(),
            Some("the test command goes after `--`: `sooth run -- php artisan test`")
        );
        // Flags in front of the command stay in front of the `--`.
        assert_eq!(
            tip_for(&typed("sooth run --runs 3 pytest -k x"), "pytest").as_deref(),
            Some("the test command goes after `--`: `sooth run --runs 3 -- pytest -k x`")
        );
        // With a `--` already there, the stray word is something else.
        assert_eq!(tip_for(&typed("sooth run oops -- pytest"), "oops"), None);
        // Other subcommands take no command — not even when the word `run`
        // is on the line as a value.
        assert_eq!(tip_for(&typed("sooth history extra"), "extra"), None);
        assert_eq!(
            tip_for(&typed("sooth explain --junit run extra"), "extra"),
            None
        );
        assert_eq!(
            tip_for(&typed("sooth import --env run a.xml --bogus"), "--bogus"),
            None
        );
        // An unknown flag is not a test command.
        assert_eq!(tip_for(&typed("sooth run --foo pytest"), "--foo"), None);
    }

    #[test]
    fn the_corrected_command_is_one_that_parses() {
        use super::tip_for;
        // The first `pytest` is the value of `--junit`; the `--` belongs in
        // front of the second.
        assert_eq!(
            tip_for(&typed("sooth run --junit pytest pytest"), "pytest").as_deref(),
            Some("the test command goes after `--`: `sooth run --junit pytest -- pytest`")
        );
        // A command that is itself called `run`.
        assert_eq!(
            tip_for(&typed("sooth run --runs 2 run"), "run").as_deref(),
            Some("the test command goes after `--`: `sooth run --runs 2 -- run`")
        );
        // The word after `--json`, not the same word after `--runs`.
        assert_eq!(
            tip_for(&typed("sooth run --runs 3 --json 3 -- pytest"), "3").as_deref(),
            Some("`--json` takes its value with an `=`: `--json=3`")
        );
        // The binary's own path is not part of what to type.
        assert_eq!(
            tip_for(&typed("/usr/local/bin/sooth run pytest"), "pytest").as_deref(),
            Some("the test command goes after `--`: `sooth run -- pytest`")
        );
    }

    #[test]
    fn a_value_given_with_a_space_is_shown_with_its_equals_sign() {
        use super::tip_for;
        assert_eq!(
            tip_for(&typed("sooth run --json out.json -- pytest"), "out.json").as_deref(),
            Some("`--json` takes its value with an `=`: `--json=out.json`")
        );
        assert_eq!(
            tip_for(
                &typed("sooth run --changed origin/main --runs 20 -- pytest"),
                "origin/main"
            )
            .as_deref(),
            Some("`--changed` takes its value with an `=`: `--changed=origin/main`")
        );
    }

    #[test]
    fn the_tip_rides_on_claps_own_error() {
        use super::with_tip;
        let argv = typed("sooth run php artisan test");
        let error = Cli::try_parse_from(&argv).expect_err("no `--`, no command");
        let shown = with_tip(error, &argv).to_string();
        assert!(shown.contains("unexpected argument 'php'"), "got: {shown}");
        assert!(
            shown
                .contains("tip: the test command goes after `--`: `sooth run -- php artisan test`"),
            "got: {shown}"
        );
        // Any other usage error passes through untouched.
        let argv = typed("sooth run --runs 0 -- true");
        let error = Cli::try_parse_from(&argv).expect_err("zero runs");
        assert!(!with_tip(error, &argv).to_string().contains("tip:"));
    }

    #[test]
    fn short_help_is_one_line_per_flag_and_long_help_keeps_the_detail() {
        use clap::CommandFactory;
        let mut command = Cli::command();
        let run = command
            .find_subcommand_mut("run")
            .expect("the run subcommand");
        let short = run.render_help().to_string();
        let long = run.render_long_help().to_string();
        for line in short.lines() {
            assert!(line.len() <= 100, "`-h` is for scanning: {line:?}");
        }
        assert!(!short.contains("mutually exclusive"), "got: {short}");
        assert!(long.contains("mutually exclusive"), "got: {long}");
        // Every flag says something in the short form too.
        let mut import = Cli::command();
        let import = import
            .find_subcommand_mut("import")
            .expect("the import subcommand")
            .render_help()
            .to_string();
        assert!(
            import.contains("Where the reports were produced"),
            "got: {import}"
        );
        assert!(
            import.contains("The commit the reports were produced on"),
            "got: {import}"
        );
    }

    #[test]
    fn pest_names_the_phpunit_preset() {
        let args = parse_run_args(&["sooth", "run", "--preset", "pest", "--", "vendor/bin/pest"]);
        assert_eq!(args.preset, Some(Preset::Phpunit));
    }

    #[test]
    fn a_preset_name_is_the_value_the_flag_takes() {
        use clap::ValueEnum;
        for preset in Preset::value_variants() {
            assert_eq!(
                Preset::from_str(preset.name(), false).ok(),
                Some(*preset),
                "`--preset {}` must parse back to itself",
                preset.name()
            );
        }
    }

    #[test]
    fn json_takes_an_optional_file_path_with_equals() {
        let args = parse_run_args(&["sooth", "run", "--json=out.json", "--", "true"]);
        assert_eq!(args.json, Some(Some(std::path::PathBuf::from("out.json"))));
    }

    #[test]
    fn a_command_is_required() {
        assert!(Cli::try_parse_from(["sooth", "run"]).is_err());
    }

    #[test]
    fn rejects_slowest_zero() {
        // Mirrors the --runs guard: a silent empty ranking is a surprise,
        // not a feature.
        assert!(Cli::try_parse_from([
            "sooth",
            "run",
            "--junit",
            "r.xml",
            "--slowest",
            "0",
            "--",
            "true",
        ])
        .is_err());
    }

    #[test]
    fn rejects_runs_below_one() {
        // `--runs 0` would run nothing and report a vacuous success; reject it early.
        assert!(Cli::try_parse_from(["sooth", "run", "--runs", "0", "--", "true"]).is_err());
    }

    #[test]
    fn preset_and_junit_are_mutually_exclusive() {
        // A preset manages its own report; pointing sooth at another file at
        // the same time is contradictory input. clap exits 2 on usage errors,
        // matching the "sooth itself failed" exit code.
        assert!(Cli::try_parse_from([
            "sooth", "run", "--preset", "pytest", "--junit", "r.xml", "--", "pytest",
        ])
        .is_err());
    }

    #[test]
    fn parses_the_junit_path() {
        let args = parse_run_args(&["sooth", "run", "--junit", "target/report.xml", "--", "true"]);
        assert_eq!(
            args.junit,
            Some(std::path::PathBuf::from("target/report.xml"))
        );
    }

    #[test]
    fn explain_takes_a_report_and_runs_no_command() {
        let parsed =
            Cli::try_parse_from(["sooth", "explain", "--junit", "report.xml", "--json"]).unwrap();
        let Command::Explain(args) = parsed.command else {
            panic!("expected `explain`");
        };
        assert_eq!(args.junit, std::path::PathBuf::from("report.xml"));
        assert!(args.json);
        assert_eq!(args.color, ColorChoice::Auto);
    }

    #[test]
    fn import_takes_an_environment_a_commit_and_reports() {
        let parsed = Cli::try_parse_from([
            "sooth", "import", "--env", "ci", "--commit", "abc123", "a.xml", "b.xml",
        ])
        .unwrap();
        let Command::Import(args) = parsed.command else {
            panic!("expected `import`");
        };
        assert_eq!(args.env.as_deref(), Some("ci"));
        assert_eq!(args.commit.as_deref(), Some("abc123"));
        assert_eq!(args.reports.len(), 2);
    }

    #[test]
    fn import_requires_an_environment_and_at_least_one_report() {
        // Sooth cannot tell where a downloaded file came from; guessing
        // would poison the environment evidence.
        assert!(Cli::try_parse_from(["sooth", "import", "a.xml"]).is_err());
        assert!(Cli::try_parse_from(["sooth", "import", "--env", "ci"]).is_err());
    }

    #[test]
    fn explain_requires_a_report() {
        assert!(Cli::try_parse_from(["sooth", "explain"]).is_err());
    }
}
