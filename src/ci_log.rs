//! What a GitHub Actions job log holds for `import --github`: its steps,
//! the commit its checkout landed on, each step's test summary, and — for a
//! failure the log does not name — what it does say about the cause (see
//! `DECISIONS.md`).

use crate::phpunit_log::clean;

/// A log line as the readers see it — past the byte-order mark GitHub puts
/// in front of a job log, its timestamp, and ANSI colors.
fn plain(raw: &str, at: &mut Option<String>) -> String {
    clean(raw.trim_start_matches('\u{feff}'), at)
}

/// One step of a job log: the command its `##[group]Run` header shows, and
/// the raw lines up to the next step, as the log readers take them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub command: String,
    pub text: String,
}

/// The steps of a raw job log, in order. Lines before the first step
/// header belong to none.
pub fn steps(raw: &str) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut at = None;
    for line in raw.lines() {
        let plain = plain(line, &mut at);
        if let Some(command) = plain.strip_prefix("##[group]Run ") {
            steps.push(Step {
                command: command.trim().to_owned(),
                text: String::new(),
            });
        } else if let Some(step) = steps.last_mut() {
            step.text.push_str(line);
            step.text.push('\n');
        }
    }
    steps
}

/// The commits the job's checkouts landed on: `actions/checkout` runs
/// `git log -1 --format=%H` and the log shows the full sha on the next
/// line. One entry per checkout it can read.
pub fn checkout_shas(raw: &str) -> Vec<String> {
    let mut at = None;
    let mut shas = Vec::new();
    let mut asked = false;
    for line in raw.lines() {
        let plain = plain(line, &mut at);
        let plain = plain.trim();
        if plain.starts_with("[command]") && plain.ends_with("git log -1 --format=%H") {
            asked = true;
        } else if asked && !plain.is_empty() {
            asked = false;
            if plain.len() == 40 && plain.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                shas.push(plain.to_ascii_lowercase());
            }
        }
    }
    shas
}

/// Which runner's output a summary is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    /// `PHPUnit`'s own (`vendor/bin/phpunit`, paratest).
    Phpunit,
    /// Collision's (`php artisan test`, Pest).
    Collision,
}

/// A test summary line, in counts. Collision does not tell an error from a
/// failure: both are `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub reader: Reader,
    pub total: usize,
    pub failed: usize,
    pub errors: usize,
    pub skipped: usize,
    pub incomplete: usize,
    /// Collision's `todo`: tests written down, not run.
    pub todo: usize,
}

impl Summary {
    /// Whether every test it counts got past its assertions.
    pub fn green(&self) -> bool {
        self.failed == 0 && self.errors == 0
    }

    /// Whether `other` counts the same tests: the same reader and total,
    /// and as many in each state a test is in whatever its outcome — so
    /// the same tests ran, as far as two summaries can tell.
    pub fn same_selection(&self, other: &Self) -> bool {
        self.reader == other.reader
            && self.total == other.total
            && self.skipped == other.skipped
            && self.incomplete == other.incomplete
            && self.todo == other.todo
    }
}

/// Every test summary in `text`, in order.
pub fn summaries(text: &str) -> Vec<Summary> {
    let mut at = None;
    text.lines()
        .filter_map(|raw| {
            let line = plain(raw, &mut at);
            let line = line.trim();
            phpunit_summary(line).or_else(|| collision_summary(line))
        })
        .collect()
}

/// `OK (12 tests, 30 assertions)`, or the `Tests: 12, Assertions: 30,
/// Failures: 1, Skipped: 2.` line every other `PHPUnit` verdict is
/// followed by.
fn phpunit_summary(line: &str) -> Option<Summary> {
    let empty = Summary {
        reader: Reader::Phpunit,
        total: 0,
        failed: 0,
        errors: 0,
        skipped: 0,
        incomplete: 0,
        todo: 0,
    };
    if let Some(rest) = line.strip_prefix("OK (") {
        let (count, _) = rest.split_once(" test")?;
        return Some(Summary {
            total: count.parse().ok()?,
            ..empty
        });
    }
    if !(line.starts_with("Tests: ") && line.contains("Assertions: ")) {
        return None;
    }
    let count = |label: &str| {
        line.trim_end_matches('.')
            .split(", ")
            .find_map(|field| field.strip_prefix(label)?.parse().ok())
    };
    Some(Summary {
        total: count("Tests: ")?,
        failed: count("Failures: ").unwrap_or(0),
        errors: count("Errors: ").unwrap_or(0),
        skipped: count("Skipped: ").unwrap_or(0),
        incomplete: count("Incomplete: ").unwrap_or(0),
        ..empty
    })
}

/// `Tests:    1 failed, 644 passed (2395 assertions)`: every count is a
/// test in one state, so the total is their sum.
fn collision_summary(line: &str) -> Option<Summary> {
    let counts = line.strip_prefix("Tests:")?.trim();
    let counts = counts.split(" (").next().unwrap_or(counts);
    let mut summary = Summary {
        reader: Reader::Collision,
        total: 0,
        failed: 0,
        errors: 0,
        skipped: 0,
        incomplete: 0,
        todo: 0,
    };
    for field in counts.split(", ") {
        let (count, label) = field.trim().split_once(' ')?;
        let count: usize = count.parse().ok()?;
        match label {
            "failed" => summary.failed = count,
            "skipped" => summary.skipped = count,
            "incomplete" => summary.incomplete = count,
            "todo" | "todos" => summary.todo = count,
            "passed" | "risky" | "warnings" | "warning" | "deprecated" | "notices" | "notice"
            | "pending" => {}
            _ => return None,
        }
        summary.total += count;
    }
    (summary.total > 0).then_some(summary)
}

/// What a Collision failure block says about its cause when the test is
/// not named: the exception, the first frame in the project's own code
/// (outside `vendor/`), and a frame in a test file, when the trace shows
/// one — that names the test's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    /// The `FAILED` line as printed.
    pub printed: String,
    pub exception: Option<String>,
    /// The file of the first project frame, without its line: lines move
    /// between commits, the file is what to open.
    pub file: Option<String>,
    pub test_file: Option<String>,
}

/// The cause of each failure Collision prints in `text`, in order.
pub fn collision_causes(text: &str) -> Vec<Cause> {
    let mut at = None;
    let mut causes: Vec<Cause> = Vec::new();
    for raw in text.lines() {
        let line = plain(raw, &mut at);
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("FAILED") {
            if rest.starts_with(' ') {
                let exception = rest
                    .trim()
                    .rsplit_once("  ")
                    .map(|(_, word)| word.trim())
                    .filter(|word| is_class_name(word))
                    .map(str::to_owned);
                causes.push(Cause {
                    printed: line.to_owned(),
                    exception,
                    file: None,
                    test_file: None,
                });
            }
            continue;
        }
        let Some(cause) = causes.last_mut() else {
            continue;
        };
        // `at path:98`, or a numbered frame `24  path:103`.
        let frame = line
            .strip_prefix("at ")
            .or_else(|| {
                let (number, rest) = line.split_once(' ')?;
                number.parse::<u32>().ok().map(|_| rest.trim())
            })
            .and_then(|frame| frame.rsplit_once(':'))
            .filter(|(_, number)| number.parse::<u32>().is_ok())
            .map(|(path, _)| path.trim());
        let Some(path) = frame else {
            continue;
        };
        if path.starts_with("vendor/") || path.contains("/vendor/") {
            continue;
        }
        if cause.file.is_none() {
            cause.file = Some(path.to_owned());
        }
        if cause.test_file.is_none() && path.ends_with("Test.php") {
            cause.test_file = Some(path.to_owned());
        }
    }
    causes
}

/// Whether `word` can be a PHP class's short name.
fn is_class_name(word: &str) -> bool {
    word.starts_with(|first: char| first.is_ascii_uppercase())
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::{checkout_shas, collision_causes, steps, summaries, Reader, Summary};

    /// A job log as GitHub serves it: a byte-order mark, then every line
    /// behind a timestamp.
    fn job_log(lines: &[&str]) -> String {
        let mut log = String::from("\u{feff}");
        for line in lines {
            log.push_str("2026-11-20T16:05:02.4586855Z ");
            log.push_str(line);
            log.push('\n');
        }
        log
    }

    #[test]
    fn a_job_log_splits_into_its_steps_behind_their_run_headers() {
        let log = job_log(&[
            "##[group]Run actions/checkout@v4",
            "[command]/usr/bin/git log -1 --format=%H",
            "2f040dcffca625c6a1dab909a6a7a14c63ae3b70",
            "##[endgroup]",
            "##[group]Run php artisan test --compact",
            "\u{1b}[36;1mphp artisan test --compact\u{1b}[0m",
            "##[endgroup]",
            "  Tests:    645 passed (2395 assertions)",
        ]);
        let found = steps(&log);
        let commands: Vec<&str> = found.iter().map(|step| step.command.as_str()).collect();
        assert_eq!(commands, ["actions/checkout@v4", "php artisan test --compact"]);
        assert_eq!(summaries(&found[0].text), []);
        assert_eq!(summaries(&found[1].text).len(), 1);
        assert_eq!(
            checkout_shas(&log),
            ["2f040dcffca625c6a1dab909a6a7a14c63ae3b70"]
        );
    }

    #[test]
    fn a_checkout_sha_is_read_only_where_git_was_asked_for_it() {
        let log = job_log(&[
            "2f040dcffca625c6a1dab909a6a7a14c63ae3b70",
            "[command]/usr/bin/git log -1 --format=%H",
            "",
            "not a sha",
            "[command]git log -1 --format=%H",
            "D6868F49BA000000000000000000000000000000",
        ]);
        assert_eq!(
            checkout_shas(&log),
            ["d6868f49ba000000000000000000000000000000"]
        );
    }

    #[test]
    fn phpunit_summaries_count_every_state_a_selection_shows() {
        let red = summaries("FAILURES!\nTests: 1236, Assertions: 4551, Failures: 1, Skipped: 9.\n");
        let green =
            summaries("OK, but there were issues!\nTests: 1236, Assertions: 4553, Skipped: 9.\n");
        let plain = summaries("OK (1236 tests, 4553 assertions)\n");
        assert_eq!(
            red,
            [Summary {
                reader: Reader::Phpunit,
                total: 1236,
                failed: 1,
                errors: 0,
                skipped: 9,
                incomplete: 0,
                todo: 0,
            }]
        );
        assert!(!red[0].green() && green[0].green() && plain[0].green());
        assert!(red[0].same_selection(&green[0]));
        // Nine skipped against none: not the same tests.
        assert!(!red[0].same_selection(&plain[0]));
        assert_eq!(summaries("OK (1 test, 1 assertion)\n")[0].total, 1);
    }

    #[test]
    fn collision_summaries_add_their_states_up_to_the_total() {
        let red = summaries("  Tests:    1 failed, 644 passed (2395 assertions)\n");
        let green = summaries("  Tests:    645 passed (2397 assertions)\n");
        let skipping = summaries("  Tests:    2 skipped, 643 passed (2390 assertions)\n");
        assert_eq!(red[0].reader, Reader::Collision);
        assert_eq!((red[0].total, red[0].failed), (645, 1));
        assert!(red[0].same_selection(&green[0]) && green[0].green());
        assert!(!red[0].same_selection(&skipping[0]));
        // A PHPUnit total of the same size is another runner's count.
        let phpunit = summaries("OK (645 tests, 2397 assertions)\n");
        assert!(!red[0].same_selection(&phpunit[0]));
        // Not a summary: a word it does not know, or nothing counted.
        assert_eq!(summaries("Tests:    3 exploded\nTests:\n"), []);
    }

    #[test]
    fn a_cut_collision_failure_still_says_its_exception_and_where_to_look() {
        let log = job_log(&[
            "  \u{1b}[41;1m FAILED \u{1b}[49;22m \u{1b}[1mModules\\Shop\\Test\u{1b}[22m… \u{1b}[41;1m TenantDatabaseAlreadyExistsException \u{1b}[49;22m  ",
            "  Tenant cannot be created. Reason: Database tenant_x already exists.",
            "  at vendor/stancl/tenancy/src/Database/DatabaseManager.php:98",
            "     94▕     {",
            "      +23 vendor frames ",
            "  24  database/factories/TenantFactory.php:103",
            "      +6 vendor frames ",
            "  31  tests/Feature/ShopTest.php:20",
            "  FAILED  Tests\\Unit\\CartTest > it adds a line  ",
            "  Failed asserting that 2 is identical to 1.",
            "  at tests/Unit/CartTest.php:16",
        ]);
        let causes = collision_causes(&log);
        assert_eq!(causes.len(), 2);
        assert_eq!(
            causes[0].exception.as_deref(),
            Some("TenantDatabaseAlreadyExistsException")
        );
        assert_eq!(
            causes[0].file.as_deref(),
            Some("database/factories/TenantFactory.php")
        );
        assert_eq!(
            causes[0].test_file.as_deref(),
            Some("tests/Feature/ShopTest.php")
        );
        assert_eq!(causes[1].exception, None);
        assert_eq!(causes[1].file.as_deref(), Some("tests/Unit/CartTest.php"));
        assert_eq!(causes[1].test_file.as_deref(), Some("tests/Unit/CartTest.php"));
    }
}
