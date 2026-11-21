//! What a Composer script does with the arguments after `--`. Composer hands
//! them to every command of the script, and through a reference to another
//! script (`@test-unit`) to every command of that one, unless a command says
//! `@no_additional_args`. Under the phpunit preset sooth's report flag is
//! one of those arguments, so a script that runs several runners in turn has
//! each of them write the report over the one before (#303).
//!
//! The rules are Composer 2's own (`EventDispatcher::doDispatch`, Composer
//! 2.10):
//!
//! - a command with `@no_additional_args` anywhere in it gets none of them,
//!   and a reference marked so hands none on;
//! - `@<name> …` (other than `@php …` and `@putenv …`) calls the script
//!   `<name>` with them — unless it writes `@additional_args`, which, there,
//!   hands nothing on at all;
//! - `@composer …` starts Composer again with them, and behind a `--` of its
//!   own they reach the script it names;
//! - `@putenv …` and a PHP callback (`Class::method`) get no command line;
//! - a command class (`App\SomeCommand`) and any other command, `@php …`
//!   included, get them: at the end, or wherever it writes
//!   `@additional_args`, as many times as it writes it;
//! - a reference to a script that does not exist runs nothing, and a script
//!   that calls itself, however far round, is stopped.
//!
//! Which script a command line runs is Composer's too: its own commands and
//! their aliases come first; a script named like one of them, or like a
//! script event, is no command; an alias from `scripts-aliases` takes its
//! name from whatever had it but a command Composer runs by its own name;
//! and a name that is none of them is looked up as Symfony Console does — a
//! unique abbreviation per `:`-separated part, case-insensitively when
//! nothing matches as typed.

use std::path::Path;

use serde_json::{Map, Value};

/// Composer's own commands, each with its aliases (Composer 2.10), and
/// Symfony Console's.
const COMPOSER_COMMANDS: &[&[&str]] = &[
    &["about"],
    &["archive"],
    &["audit"],
    &["browse", "home"],
    &["bump"],
    &["check-platform-reqs"],
    &["clear-cache", "clearcache", "cc"],
    &["config"],
    &["create-project"],
    &["depends", "why"],
    &["diagnose"],
    &["dump-autoload", "dumpautoload"],
    &["exec"],
    &["fund"],
    &["global"],
    &["init"],
    &["install", "i"],
    &["licenses"],
    &["outdated"],
    &["policy"],
    &["prohibits", "why-not"],
    &["reinstall"],
    &["remove", "rm", "uninstall"],
    &["repository", "repo"],
    &["require", "r"],
    &["run-script", "run"],
    &["search"],
    &["self-update", "selfupdate"],
    &["show", "info"],
    &["status"],
    &["suggests"],
    &["update", "u", "upgrade"],
    &["validate"],
    &["help"],
    &["list"],
    &["completion"],
    &["_complete"],
];

/// The events a script can be named after: `run-script` runs them, and none
/// is a command of its own.
const SCRIPT_EVENTS: &[&str] = &[
    "pre-install-cmd",
    "post-install-cmd",
    "pre-update-cmd",
    "post-update-cmd",
    "pre-status-cmd",
    "post-status-cmd",
    "pre-autoload-dump",
    "post-autoload-dump",
    "post-root-package-install",
    "post-create-project-cmd",
    "pre-archive-cmd",
    "post-archive-cmd",
];

/// A Composer command line, read as far as sooth needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call<'a> {
    /// The project directory Composer was pointed at (`-d`,
    /// `--working-dir`), relative to where it runs.
    pub dir: Option<&'a str>,
    /// The command as typed: a script, an alias or abbreviation of one, or
    /// one of Composer's own (`run-script`).
    pub command: &'a str,
    /// The word after it: the script, under `run-script`.
    pub then: Option<&'a str>,
}

/// The command line `command` hands Composer, `composer` standing at `at`:
/// the first word that is no option of Composer's own, and the one after it.
/// Words behind a `--` are words too: `composer -- test` runs `test`. `None`
/// when there is no word.
pub fn called<S: AsRef<str>>(command: &[S], at: usize) -> Option<Call<'_>> {
    let mut dir = None;
    let mut words = Vec::new();
    let mut rest = command.get(at + 1..)?.iter().map(AsRef::as_ref);
    while let Some(arg) = rest.next() {
        if arg == "--" {
            words.extend(rest.by_ref());
            break;
        }
        if arg == "-d" || arg == "--working-dir" {
            dir = Some(rest.next()?);
        } else if let Some(value) = arg
            .strip_prefix("--working-dir=")
            .or_else(|| arg.strip_prefix("-d").filter(|value| !value.is_empty()))
        {
            dir = Some(value);
        } else if arg == "--timeout" {
            // `run-script`'s one option that takes a value.
            rest.next()?;
        } else if !arg.starts_with('-') {
            words.push(arg);
        }
    }
    let mut words = words.into_iter();
    Some(Call {
        dir,
        command: words.next()?,
        then: words.next(),
    })
}

/// A command of a Composer script that gets the arguments after `--`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receiver {
    /// The script it is written in.
    pub script: String,
    /// The command, as `composer.json` writes it.
    pub command: String,
    /// How many times it gets them: once, or once per `@additional_args`.
    pub times: usize,
}

/// What a name on Composer's command line runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target<'a> {
    Script(&'a str),
    Composer(&'static str),
    /// An alias two scripts claim: Composer runs the later in the file, an
    /// order sooth does not keep, so neither is assumed.
    Unsure,
}

impl<'a> Target<'a> {
    /// The name the thing it runs goes by; none for an alias in doubt.
    fn canonical(self) -> Option<&'a str> {
        match self {
            Target::Script(script) => Some(script),
            Target::Composer(command) => Some(command),
            Target::Unsure => None,
        }
    }
}

/// A `composer.json`, read for its scripts.
pub struct Manifest<'a> {
    scripts: &'a Map<String, Value>,
    /// Every name Composer answers to, with what it runs.
    names: Vec<(&'a str, Target<'a>)>,
    /// Names of Composer's own commands an alias took: typed in full they
    /// still run Composer's command, since then it registers no scripts.
    own_in_full: Vec<&'static str>,
}

impl<'a> Manifest<'a> {
    /// The scripts of `json`, a parsed `composer.json`; `None` without any.
    /// Composer's own commands come first; a script named like one of them,
    /// or like a script event, is no command. Then the aliases, which
    /// Symfony adds over whatever had the name — a script's own, an alias of
    /// Composer's (`u`), and for an abbreviation even one of Composer's own
    /// names (`inst` for an alias `install`). Typed in full, Composer's own
    /// name runs its own command: Composer then registers no scripts.
    pub fn of(json: &'a Value) -> Option<Self> {
        let scripts = json.get("scripts")?.as_object()?;
        let mut names: Vec<(&str, Target)> = COMPOSER_COMMANDS
            .iter()
            .flat_map(|names| names.iter().map(|name| (*name, Target::Composer(names[0]))))
            .collect();
        let mut registered = Vec::new();
        for script in scripts.keys() {
            if is_event(script) || names.iter().any(|(name, _)| *name == script) {
                continue;
            }
            names.push((script, Target::Script(script)));
            registered.push(script.as_str());
        }
        let aliases = json.get("scripts-aliases").and_then(Value::as_object);
        let claims: Vec<(&str, &str)> = registered
            .iter()
            .flat_map(|script| {
                let its = aliases
                    .and_then(|aliases| aliases.get(*script))
                    .and_then(Value::as_array);
                its.into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(move |alias| (alias, *script))
            })
            .collect();
        let mut own_in_full = Vec::new();
        for &(alias, script) in &claims {
            if let Some(names) = COMPOSER_COMMANDS.iter().find(|names| names[0] == alias) {
                own_in_full.push(names[0]);
            }
            let doubted = claims
                .iter()
                .any(|&(other, by)| other == alias && by != script);
            names.retain(|(name, _)| *name != alias);
            names.push((
                alias,
                if doubted {
                    Target::Unsure
                } else {
                    Target::Script(script)
                },
            ));
        }
        Some(Self {
            scripts,
            names,
            own_in_full,
        })
    }

    /// The script `call` runs, if it runs one.
    pub fn script(&self, call: &Call<'_>) -> Option<&'a str> {
        match self.target(call.command)? {
            Target::Script(script) => Some(script),
            Target::Composer("run-script") => {
                let (script, _) = self.scripts.get_key_value(call.then?)?;
                // An event spelled otherwise is one `run-script` refuses.
                (!is_event(script) || SCRIPT_EVENTS.contains(&script.as_str()))
                    .then_some(script.as_str())
            }
            Target::Composer(_) | Target::Unsure => None,
        }
    }

    /// What `word` names, as Symfony Console finds it: the name itself, or
    /// the one thing every name it abbreviates runs.
    fn target(&self, word: &str) -> Option<Target<'a>> {
        if let Some(own) = self.own_in_full.iter().find(|own| **own == word) {
            return Some(Target::Composer(own));
        }
        if let Some((_, target)) = self.names.iter().find(|(name, _)| *name == word) {
            return Some(*target);
        }
        let abbreviated = |exactly: bool| -> Vec<(&'a str, Target<'a>)> {
            self.names
                .iter()
                .filter(|(name, _)| abbreviates(word, name, exactly))
                .copied()
                .collect()
        };
        let mut found = abbreviated(true);
        if found.is_empty() {
            found = abbreviated(false);
        }
        if !found.iter().any(|(name, _)| fills(word, name)) {
            return None;
        }
        // An alias counts apart only when what it names does not match too.
        let kept: Vec<Target> = found
            .iter()
            .filter(|(name, target)| {
                target
                    .canonical()
                    .is_none_or(|own| *name == own || !found.iter().any(|(other, _)| *other == own))
            })
            .map(|(_, target)| *target)
            .collect();
        match kept[..] {
            [target] => Some(target),
            _ => None,
        }
    }

    /// Every command that gets the arguments when Composer runs `script`,
    /// in the order they run.
    pub fn receivers(&self, script: &str) -> Vec<Receiver> {
        let mut found = Vec::new();
        self.walk(script, &mut Vec::new(), &mut found);
        found
    }

    fn walk(&self, script: &str, calling: &mut Vec<String>, found: &mut Vec<Receiver>) {
        if calling.iter().any(|called| called == script) {
            return;
        }
        let commands: Vec<&str> = match self.scripts.get(script) {
            Some(Value::String(command)) => vec![command],
            Some(Value::Array(commands)) => commands.iter().filter_map(Value::as_str).collect(),
            _ => return,
        };
        calling.push(script.to_owned());
        for command in commands {
            match Kind::of(command) {
                Kind::Deaf => {}
                Kind::Reference(name) => self.walk(name, calling, found),
                Kind::Command(times) => match self.restarted(command) {
                    Some(inner) => self.walk(inner, calling, found),
                    None => found.push(Receiver {
                        script: script.to_owned(),
                        command: command.to_owned(),
                        times,
                    }),
                },
            }
        }
        calling.pop();
    }

    /// The script of this manifest that `command` runs by starting
    /// Composer again, when the arguments reach that script: behind a `--`
    /// of its own. Without one, the new Composer reads them as its own
    /// options, and stops.
    fn restarted(&self, command: &str) -> Option<&'a str> {
        let words: Vec<&str> = command.split_whitespace().collect();
        let composer = |word: &&str| {
            let name = word.trim_start_matches('@');
            matches!(
                name.rsplit('/').next().unwrap_or(name),
                "composer" | "composer.phar"
            )
        };
        let at = if words.first().is_some_and(composer) {
            0
        } else if matches!(words.first(), Some(&("php" | "@php")))
            && words.get(1).is_some_and(composer)
        {
            1
        } else {
            return None;
        };
        if !words.contains(&"--") {
            return None;
        }
        let call = called(&words, at)?;
        if call.dir.is_some() {
            return None;
        }
        self.script(&call)
    }
}

/// How one command of a script treats the arguments.
enum Kind<'a> {
    /// It gets none: marked, `@putenv`, a PHP callback, or a reference that
    /// hands none on.
    Deaf,
    /// It calls the script of this name with them.
    Reference(&'a str),
    /// It gets them this many times.
    Command(usize),
}

impl<'a> Kind<'a> {
    fn of(command: &'a str) -> Self {
        if command.contains("@no_additional_args") {
            return Kind::Deaf;
        }
        let reference = command
            .strip_prefix('@')
            .filter(|_| !command.starts_with("@php ") && !command.starts_with("@putenv "));
        if let Some(reference) = reference {
            // Composer keeps what `array_splice` removed — nothing — as the
            // arguments of a reference that writes `@additional_args`.
            if command.contains("@additional_args") {
                return Kind::Deaf;
            }
            if command.starts_with("@composer ") {
                return Kind::Command(1);
            }
            return Kind::Reference(reference.split(' ').next().unwrap_or_default());
        }
        if command.starts_with("@putenv ") || (!command.contains(' ') && command.contains("::")) {
            return Kind::Deaf;
        }
        Kind::Command(command.matches("@additional_args").count().max(1))
    }
}

/// Whether a script named `script` is named after a script event, as
/// Composer checks it: upper-cased, with `-` read as `_`.
fn is_event(script: &str) -> bool {
    let key = |name: &str| name.to_ascii_uppercase().replace('-', "_");
    SCRIPT_EVENTS.iter().any(|event| key(event) == key(script))
}

/// Whether `word` abbreviates `name` part by part, the way Symfony matches
/// `^part[^:]*:part[^:]*…`: each `:`-separated part of `word` starts the
/// matching part of `name` — as typed, or ignoring case.
fn abbreviates(word: &str, name: &str, as_typed: bool) -> bool {
    let mut names = name.split(':');
    word.split(':').all(|part| {
        names.next().is_some_and(|own| {
            own.get(..part.len()).is_some_and(|start| {
                if as_typed {
                    start == part
                } else {
                    start.eq_ignore_ascii_case(part)
                }
            })
        })
    })
}

/// Whether `word` abbreviates `name` with nothing left over: as many parts,
/// ignoring case.
fn fills(word: &str, name: &str) -> bool {
    word.split(':').count() == name.split(':').count() && abbreviates(word, name, false)
}

/// Why sooth cannot run the Composer command line `call` against
/// `manifest` in `dir`: the script it runs hands the arguments after `--`,
/// and sooth's report flag with them, to more than one command. `None` when
/// it hands them to one or to none, and when the manifest cannot be read or
/// the line runs no script of it — sooth refuses only what it is sure of.
pub fn refusal(dir: &Path, manifest: &Path, call: &Call<'_>) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(manifest)).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    let manifest = Manifest::of(&json)?;
    let script = manifest.script(call)?;
    let typed = match call.then {
        Some(then) if script == then => format!("{} {then}", call.command),
        _ => call.command.to_owned(),
    };
    refusal_of(&manifest, &typed, script)
}

/// Whether the script the Composer command line `call` runs, from
/// `manifest` in `dir`, hands the arguments after `--` to a command that is
/// a shell line of more than one command — an `&&`, `||`, `;`, `|`, `&` or
/// line break in it, quoted or not, so a doubt counts as a chain. Composer
/// appends the arguments to the end of that line: only its last command gets
/// them. `false` when the manifest cannot be read or the line runs no script.
pub fn chained(dir: &Path, manifest: &Path, call: &Call<'_>) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(manifest)) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let Some(manifest) = Manifest::of(&json) else {
        return false;
    };
    manifest.script(call).is_some_and(|script| {
        manifest
            .receivers(script)
            .iter()
            .any(|receiver| receiver.command.contains(['&', '|', ';', '\n']))
    })
}

/// The refusal for `script`, typed as `composer {typed}`: none when its
/// commands get the arguments once in all, or never.
fn refusal_of(manifest: &Manifest<'_>, typed: &str, script: &str) -> Option<String> {
    let receivers = manifest.receivers(script);
    let count: usize = receivers.iter().map(|receiver| receiver.times).sum();
    if count < 2 {
        return None;
    }
    let list = receivers
        .iter()
        .map(|receiver| {
            let mut notes = Vec::new();
            if receiver.script != script {
                notes.push(format!("in `{}`", receiver.script));
            }
            if receiver.times > 1 {
                notes.push(format!("{} times", receiver.times));
            }
            if notes.is_empty() {
                format!("`{}`", receiver.command)
            } else {
                format!("`{}` ({})", receiver.command, notes.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let example = instead(manifest, script, &receivers)
        .map(|other| format!(" (`composer {other}`)"))
        .unwrap_or_default();
    Some(format!(
        "`composer {typed}` hands the arguments after `--`, and sooth's report flag \
         with them, to {count} commands: {list}. A runner among them would write the \
         report over the one before it, and sooth would take the last one's tests for \
         the whole run; a command that runs no tests stops at a flag it does not know. \
         Give sooth one runner at a time — a script that runs only it{example}, or the \
         runner itself — and mark a command that runs no tests with `@no_additional_args`"
    ))
}

/// A script among `receivers` that sooth could run instead of `script`: one
/// whose only command that gets the arguments looks like a test runner.
fn instead<'r>(
    manifest: &Manifest<'_>,
    script: &str,
    receivers: &'r [Receiver],
) -> Option<&'r str> {
    receivers
        .iter()
        .filter(|receiver| receiver.script != script && looks_like_a_runner(&receiver.command))
        .map(|receiver| receiver.script.as_str())
        .find(|other| {
            let theirs = manifest.receivers(other);
            theirs.iter().map(|receiver| receiver.times).sum::<usize>() == 1
        })
}

/// Whether `command` starts `PHPUnit`, Pest, paratest or `artisan test`.
fn looks_like_a_runner(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    words.iter().enumerate().any(|(at, word)| {
        let name = word.rsplit('/').next().unwrap_or(word);
        matches!(name, "phpunit" | "pest" | "paratest")
            || (name == "artisan" && words.get(at + 1) == Some(&"test"))
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{json, Value};

    use super::{called, looks_like_a_runner, refusal, Call, Manifest, Receiver};

    fn words(command: &str) -> Vec<&str> {
        command.split(' ').collect()
    }

    fn call_of(command: &str) -> Option<(Option<String>, String, Option<String>)> {
        let words = words(command);
        let at = usize::from(words[0] == "php");
        called(&words, at).map(|Call { dir, command, then }| {
            (
                dir.map(str::to_owned),
                command.to_owned(),
                then.map(str::to_owned),
            )
        })
    }

    fn line(command: &str, then: Option<&str>) -> (Option<String>, String, Option<String>) {
        (None, command.to_owned(), then.map(str::to_owned))
    }

    #[test]
    fn the_command_is_the_first_word_that_is_no_option() {
        assert_eq!(call_of("composer test"), Some(line("test", None)));
        assert_eq!(
            call_of("composer test -- --filter=x"),
            Some(line("test", Some("--filter=x")))
        );
        assert_eq!(
            call_of("composer --no-ansi test --"),
            Some(line("test", None))
        );
        assert_eq!(
            call_of("composer -- test --log-junit"),
            Some(line("test", Some("--log-junit")))
        );
        assert_eq!(call_of("php composer.phar test"), Some(line("test", None)));
        assert_eq!(
            call_of("composer run-script --timeout 0 test"),
            Some(line("run-script", Some("test")))
        );
        assert_eq!(
            call_of("composer run-script --timeout=0 test"),
            Some(line("run-script", Some("test")))
        );
        assert_eq!(call_of("composer"), None);
        assert_eq!(call_of("composer --"), None);
        assert_eq!(call_of("composer --timeout"), None);
    }

    #[test]
    fn a_working_dir_is_where_the_manifest_is() {
        let at = |dir: &str| Some((Some(dir.to_owned()), "test".to_owned(), None));
        assert_eq!(call_of("composer -d app test"), at("app"));
        assert_eq!(call_of("composer -dapp test"), at("app"));
        assert_eq!(call_of("composer --working-dir app test"), at("app"));
        assert_eq!(call_of("composer --working-dir=app test"), at("app"));
        assert_eq!(call_of("composer test -d"), None);
    }

    fn manifest(json: &Value) -> Manifest<'_> {
        Manifest::of(json).expect("scripts")
    }

    fn script_of(json: &Value, command: &str) -> Option<String> {
        let words = words(command);
        let call = called(&words, 0).expect("a word");
        manifest(json).script(&call).map(str::to_owned)
    }

    #[test]
    fn a_command_names_a_script_as_composer_finds_it() {
        let json = json!({
            "scripts": {
                "test": "phpunit",
                "test:unit": "phpunit tests/Unit",
                "format": "pint",
                "status": "phpunit",
                "post-install-cmd": "phpunit",
                "rubocop": "phpunit"
            },
            "scripts-aliases": { "test": ["t", "check"], "rubocop": ["rc"] }
        });
        let named = |command| script_of(&json, command);
        assert_eq!(named("composer test").as_deref(), Some("test"));
        assert_eq!(named("composer run-script test").as_deref(), Some("test"));
        assert_eq!(named("composer run test").as_deref(), Some("test"));
        // Aliases, as a command only.
        assert_eq!(named("composer t").as_deref(), Some("test"));
        assert_eq!(named("composer check").as_deref(), Some("test"));
        assert_eq!(named("composer run-script t"), None);
        // Abbreviations, part by part, and case; `f` is also `fund`.
        assert_eq!(named("composer f"), None);
        assert_eq!(named("composer fo").as_deref(), Some("format"));
        assert_eq!(named("composer t:u").as_deref(), Some("test:unit"));
        assert_eq!(named("composer FORMAT").as_deref(), Some("format"));
        assert_eq!(named("composer Form").as_deref(), Some("format"));
        assert_eq!(named("composer run-s test").as_deref(), Some("test"));
        // Ambiguous: `test` and `test:unit`; `run-script` and `rubocop`.
        assert_eq!(named("composer tes"), None);
        assert_eq!(named("composer ru test"), None);
        assert_eq!(named("composer rub").as_deref(), Some("rubocop"));
        // Composer's own commands win, and an event is no command.
        assert_eq!(named("composer status"), None);
        assert_eq!(named("composer r test"), None);
        assert_eq!(named("composer post-install-cmd"), None);
        assert_eq!(
            named("composer run-script post-install-cmd").as_deref(),
            Some("post-install-cmd")
        );
        assert_eq!(named("composer nothing"), None);
        assert_eq!(named("composer run-script nothing"), None);
        assert_eq!(named("composer run-script"), None);
        // An abbreviation must fill a name to the last part: `tes` starts
        // `test:unit` but leaves `:unit` over.
        let only_parts = json!({ "scripts": { "test:unit": "phpunit" } });
        assert_eq!(script_of(&only_parts, "composer tes"), None);
        assert_eq!(
            script_of(&only_parts, "composer tes:u").as_deref(),
            Some("test:unit")
        );
    }

    #[test]
    fn an_alias_takes_its_name_from_whatever_had_it_but_composers_own_commands() {
        let json = json!({
            "scripts": { "test": "phpunit", "lint": "pint", "unit": "phpunit tests/Unit" },
            "scripts-aliases": {
                "test": ["lint", "install"],
                "unit": ["u", "test"]
            }
        });
        let named = |command| script_of(&json, command);
        // Over a script's own name, both ways round.
        assert_eq!(named("composer lint").as_deref(), Some("test"));
        assert_eq!(named("composer test").as_deref(), Some("unit"));
        // Over an alias of Composer's own; over its own name only when
        // abbreviated.
        assert_eq!(named("composer u").as_deref(), Some("unit"));
        assert_eq!(named("composer install"), None);
        assert_eq!(named("composer inst").as_deref(), Some("test"));
        // Under `run-script`, a script's own name only.
        assert_eq!(named("composer run-script test").as_deref(), Some("test"));
    }

    #[test]
    fn an_alias_two_scripts_claim_runs_neither_for_sure() {
        let json = json!({
            "scripts": { "alpha": "phpunit", "zeta": "phpunit" },
            "scripts-aliases": { "alpha": ["t"], "zeta": ["t"] }
        });
        assert_eq!(script_of(&json, "composer t"), None);
        assert_eq!(script_of(&json, "composer alpha").as_deref(), Some("alpha"));
    }

    #[test]
    fn an_abbreviation_of_two_aliases_of_one_script_is_ambiguous_as_in_symfony() {
        let json = json!({
            "scripts": { "verify": "phpunit" },
            "scripts-aliases": { "verify": ["tx", "ty", "verify-all"] }
        });
        let named = |command| script_of(&json, command);
        assert_eq!(named("composer t"), None);
        assert_eq!(named("composer tx").as_deref(), Some("verify"));
        // An alias whose script matches too counts as that script.
        assert_eq!(named("composer veri").as_deref(), Some("verify"));
        // One whose script does not counts apart.
        let json = json!({
            "scripts": { "verify": "phpunit", "other": "phpunit" },
            "scripts-aliases": { "other": ["verx"] }
        });
        assert_eq!(script_of(&json, "composer ver"), None);
    }

    #[test]
    fn an_event_name_is_no_command_in_any_case_or_spelling() {
        let json = json!({
            "scripts": { "post_install_cmd": "phpunit", "Pre-Update-Cmd": "phpunit" }
        });
        let named = |command| script_of(&json, command);
        assert_eq!(named("composer post_install_cmd"), None);
        assert_eq!(named("composer Pre-Update-Cmd"), None);
        // `run-script` refuses it as well.
        assert_eq!(named("composer run-script post_install_cmd"), None);
    }

    fn commands(scripts: &Value, name: &str) -> Vec<(String, String, usize)> {
        let json = json!({ "scripts": scripts });
        manifest(&json)
            .receivers(name)
            .into_iter()
            .map(
                |Receiver {
                     script,
                     command,
                     times,
                 }| (script, command, times),
            )
            .collect()
    }

    fn of(script: &str, command: &str) -> (String, String, usize) {
        (script.to_owned(), command.to_owned(), 1)
    }

    #[test]
    fn one_runner_is_one_receiver() {
        assert_eq!(
            commands(&json!({ "test": "phpunit" }), "test"),
            [of("test", "phpunit")]
        );
        assert_eq!(
            commands(&json!({ "test": ["@php artisan test"] }), "test"),
            [of("test", "@php artisan test")]
        );
    }

    #[test]
    fn a_command_marked_no_additional_args_gets_none() {
        // Laravel 13's skeleton: one receiver.
        let laravel_13 = json!({ "test": [
            "@php artisan config:clear --ansi @no_additional_args",
            "@php artisan test"
        ]});
        assert_eq!(
            commands(&laravel_13, "test"),
            [of("test", "@php artisan test")]
        );
        // Laravel 12's: the `config:clear` line gets them too.
        let laravel_12 =
            json!({ "test": ["@php artisan config:clear --ansi", "@php artisan test"] });
        assert_eq!(commands(&laravel_12, "test").len(), 2);
    }

    #[test]
    fn a_reference_hands_them_to_every_command_of_the_script_it_names() {
        let scripts = json!({
            "test": "@test-all --stop-on-failure",
            "test-all": ["@test-unit", "@test-feature", "@lint @no_additional_args"],
            "test-unit": "@php artisan test --parallel --testsuite=unit",
            "test-feature": "vendor/bin/phpunit --testsuite=feature",
            "lint": ["phpstan analyse", "php-cs-fixer check"]
        });
        assert_eq!(
            commands(&scripts, "test"),
            [
                of("test-unit", "@php artisan test --parallel --testsuite=unit"),
                of("test-feature", "vendor/bin/phpunit --testsuite=feature"),
            ]
        );
    }

    #[test]
    fn a_reference_that_writes_additional_args_hands_on_nothing() {
        // Composer keeps what `array_splice` removed — nothing — as the
        // reference's arguments.
        let scripts = json!({
            "test": ["@unit @additional_args", "phpunit tests/Feature"],
            "unit": "phpunit tests/Unit"
        });
        assert_eq!(
            commands(&scripts, "test"),
            [of("test", "phpunit tests/Feature")]
        );
    }

    #[test]
    fn commands_without_a_command_line_get_none() {
        let scripts = json!({ "test": [
            "Composer\\Config::disableProcessTimeout",
            "@putenv XDEBUG_MODE=coverage",
            "phpunit"
        ]});
        assert_eq!(commands(&scripts, "test"), [of("test", "phpunit")]);
    }

    #[test]
    fn a_command_class_gets_them_as_its_input() {
        let scripts = json!({ "test": ["App\\Console\\RunUnitCommand", "phpunit"] });
        assert_eq!(commands(&scripts, "test").len(), 2);
    }

    #[test]
    fn each_additional_args_is_a_place_that_gets_them() {
        let scripts = json!({ "test": [
            "phpunit a @additional_args && phpunit b @additional_args"
        ]});
        assert_eq!(
            commands(&scripts, "test"),
            [(
                "test".to_owned(),
                "phpunit a @additional_args && phpunit b @additional_args".to_owned(),
                2
            )]
        );
    }

    #[test]
    fn composer_started_again_runs_the_script_it_names_behind_a_double_dash() {
        let scripts = json!({
            "test": "@composer run-script both --",
            "plain": "composer both --",
            "bare": "@composer run-script both",
            "elsewhere": "@composer -d sub both --",
            "both": ["phpunit tests/Unit", "phpunit tests/Feature"]
        });
        assert_eq!(commands(&scripts, "test").len(), 2);
        assert_eq!(commands(&scripts, "plain").len(), 2);
        // Without a `--` the new Composer stops at the flag: one command.
        assert_eq!(
            commands(&scripts, "bare"),
            [of("bare", "@composer run-script both")]
        );
        assert_eq!(commands(&scripts, "elsewhere").len(), 1);
        let scripts = json!({
            "phar": "@php composer.phar run-script both --",
            "local": "./composer.phar both --",
            "spliced": "@composer run-script both -- @additional_args",
            "both": ["phpunit tests/Unit", "phpunit tests/Feature"]
        });
        assert_eq!(commands(&scripts, "phar").len(), 2);
        assert_eq!(commands(&scripts, "local").len(), 2);
        // `array_splice` again: Composer starts with nothing at all.
        assert_eq!(commands(&scripts, "spliced"), []);
        // PHP running something else, or a word `composer` further on, is
        // no new Composer.
        let scripts = json!({
            "php": "php run.php -- both",
            "echo": "echo composer both --",
            "both": ["phpunit tests/Unit", "phpunit tests/Feature"]
        });
        assert_eq!(
            commands(&scripts, "php"),
            [of("php", "php run.php -- both")]
        );
        assert_eq!(
            commands(&scripts, "echo"),
            [of("echo", "echo composer both --")]
        );
    }

    #[test]
    fn what_cannot_run_runs_nothing() {
        let scripts = json!({
            "test": ["@missing", "@test", "@loop", "@composer", 7, "phpunit"],
            "loop": "@test"
        });
        assert_eq!(commands(&scripts, "test"), [of("test", "phpunit")]);
        assert_eq!(commands(&scripts, "absent"), []);
        assert_eq!(commands(&json!({ "test": 3 }), "test"), []);
    }

    #[test]
    fn a_command_that_only_looks_like_a_class_or_callback_gets_them() {
        let scripts = json!({ "test": [
            "vendor\\bin\\phpunit tests",
            "phpunit --filter=ImportCommand",
            "php -r Foo::bar()"
        ]});
        assert_eq!(commands(&scripts, "test").len(), 3);
    }

    static SCRATCH: AtomicUsize = AtomicUsize::new(0);

    /// The refusal for `command` against a `composer.json` of `json`, kept
    /// in a scratch directory of its own.
    fn refused(json: &Value, command: &str) -> Option<String> {
        let dir = std::env::temp_dir().join(format!(
            "sooth-composer-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("composer.json"), json.to_string()).expect("write");
        let words = words(command);
        let call = called(&words, 0).expect("a word");
        let found = refusal(&dir, Path::new("composer.json"), &call);
        std::fs::remove_dir_all(&dir).expect("rm");
        found
    }

    /// Whether `command` runs a chained line, against a `composer.json` of
    /// `json` in a scratch directory of its own.
    fn chain_in(json: &Value, command: &str) -> bool {
        let dir = std::env::temp_dir().join(format!(
            "sooth-composer-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("composer.json"), json.to_string()).expect("write");
        let words = words(command);
        let call = called(&words, 0).expect("a word");
        let found = super::chained(&dir, Path::new("composer.json"), &call);
        std::fs::remove_dir_all(&dir).expect("rm");
        found
    }

    #[test]
    fn a_shell_line_of_several_commands_is_a_chain_wherever_the_script_calls_it() {
        let json = json!({ "scripts": {
            "test": "phpunit tests/Unit && phpunit tests/Feature",
            "either": "phpunit || true",
            "seq": "phpunit tests/Unit; phpunit tests/Feature",
            "piped": "phpunit | tee out.txt",
            "via": "@test",
            "plain": "@php artisan test --parallel --exclude-group=central",
            "quiet": ["phpunit tests/Unit && echo done @no_additional_args", "phpunit"]
        }});
        for script in ["test", "either", "seq", "piped", "via"] {
            assert!(chain_in(&json, &format!("composer {script}")), "{script}");
        }
        assert!(!chain_in(&json, "composer plain"));
        // A chain that gets no arguments is not where they land.
        assert!(!chain_in(&json, "composer quiet"));
        // No manifest, or no script of it: nothing is known, nothing assumed.
        assert!(!chain_in(&json, "composer install"));
        assert!(!chain_in(&json!({}), "composer test"));
    }

    #[test]
    fn a_refusal_names_every_command_and_a_runner_script_to_run_instead() {
        let json = json!({ "scripts": {
            "test": ["@test:lint", "@test:unit", "@test:feature"],
            "test:lint": "pint --test",
            "test:unit": "pest --parallel",
            "test:feature": ["@test:unit", "vendor/bin/phpunit tests/Feature"]
        }});
        let refusal = refused(&json, "composer test").expect("four commands");
        assert!(
            refusal.starts_with("`composer test` hands the arguments after `--`"),
            "{refusal}"
        );
        assert!(
            refusal.contains(
                "to 4 commands: `pint --test` (in `test:lint`), `pest --parallel` (in \
                 `test:unit`), `pest --parallel` (in `test:unit`), \
                 `vendor/bin/phpunit tests/Feature` (in `test:feature`)."
            ),
            "{refusal}"
        );
        // Not the linter, and not a script that would be refused itself.
        assert!(
            refusal.contains("a script that runs only it (`composer test:unit`)"),
            "{refusal}"
        );
        assert!(refusal.ends_with("with `@no_additional_args`"), "{refusal}");
        assert!(refused(&json, "composer run-script test")
            .expect("the same script")
            .starts_with("`composer run-script test` hands"),);
    }

    #[test]
    fn without_a_runner_script_no_script_is_named() {
        let json = json!({ "scripts": {
            "test": ["@php artisan config:clear --ansi", "@php artisan test"]
        }});
        let refusal = refused(&json, "composer test").expect("two commands");
        assert!(
            refusal.contains(
                "to 2 commands: `@php artisan config:clear --ansi`, `@php artisan test`."
            ),
            "{refusal}"
        );
        assert!(
            refusal.contains("a script that runs only it, or the runner itself"),
            "{refusal}"
        );
        let json = json!({ "scripts": { "test": "phpunit a @additional_args; phpunit b @additional_args" }});
        let refusal = refused(&json, "composer test").expect("twice");
        assert!(refusal.contains("(2 times)"), "{refusal}");
    }

    #[test]
    fn a_runner_is_phpunit_pest_paratest_or_artisan_test() {
        for runner in [
            "phpunit",
            "vendor/bin/phpunit tests",
            "pest --parallel",
            "paratest",
            "@php artisan test --parallel",
            "php ./artisan test",
        ] {
            assert!(looks_like_a_runner(runner), "{runner}");
        }
        for other in [
            "pint --test",
            "@php artisan config:clear",
            "echo test",
            "@php artisan",
            "phpstan analyse",
        ] {
            assert!(!looks_like_a_runner(other), "{other}");
        }
    }

    #[test]
    fn a_refusal_names_the_script_as_typed() {
        let json = json!({ "scripts": { "test": ["phpunit a", "phpunit b"] }});
        let refusal = refused(&json, "composer test -- --filter=x").expect("two");
        assert!(refusal.starts_with("`composer test` hands"), "{refusal}");
    }

    #[test]
    fn one_receiver_or_none_is_no_refusal() {
        assert_eq!(
            refused(&json!({ "scripts": { "test": "phpunit" }}), "composer test"),
            None
        );
        assert_eq!(
            refused(
                &json!({ "scripts": { "test": "@putenv A=1" }}),
                "composer test"
            ),
            None
        );
    }

    #[test]
    fn a_manifest_sooth_cannot_read_refuses_nothing() {
        assert_eq!(refused(&json!({ "name": "x" }), "composer test"), None);
        assert_eq!(refused(&json!({ "scripts": [] }), "composer test"), None);
        let words = words("composer test");
        let call = called(&words, 0).expect("a word");
        let dir =
            std::env::temp_dir().join(format!("sooth-composer-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("composer.json"), "{ not json").expect("write");
        assert_eq!(refusal(&dir, Path::new("composer.json"), &call), None);
        assert_eq!(refusal(&dir, Path::new("missing.json"), &call), None);
        std::fs::remove_dir_all(&dir).expect("rm");
    }
}
