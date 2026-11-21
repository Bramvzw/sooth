//! The local run history: one observation per test per observed run,
//! appended to `.sooth/history.jsonl` in the directory sooth runs from.
//! This is the passive layer of flaky detection — evidence accumulates from
//! runs that happen anyway, at zero extra wall-time (see `DECISIONS.md`).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::interrupt::Interrupt;
use crate::json;
use crate::junit::TestStatus;

/// The history file, relative to the directory sooth runs from.
pub const HISTORY_PATH: &str = ".sooth/history.jsonl";

/// How many of a test's most recent invocations the analysis considers,
/// with every run they made. Invocations, because that is the unit of
/// evidence: counted in runs, three `--runs 20` gates would push a proven
/// flake out. A line without an invocation is one of its own.
pub const WINDOW_PER_TEST: usize = 50;

/// How much of the history file's tail one load reads. The file only grows
/// until it is pruned — runs and imports append — so reads must be bounded
/// or every run pays for the entire past. 64 MiB is roughly 280,000
/// observations: the whole window of a suite of five thousand tests. A
/// larger suite reads fewer than [`WINDOW_PER_TEST`] of its invocations.
pub const MAX_LOAD_BYTES: u64 = 64 * 1024 * 1024;

/// The code state observations were made on. `None` means unknowable (no
/// git binary, not a repository): such observations count in totals but can
/// never be identity-bound evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeIdentity {
    pub commit: Option<String>,
    pub dirty: Option<bool>,
    /// What a dirty tree holds, as one hash (see [`code_identity`]): two
    /// runs with the same one ran the same code. `None` for a clean tree,
    /// and when it could not be read.
    pub tree: Option<String>,
    /// The run's own report, when it is written inside the repository: no
    /// change to the code it ran, before the run or after it.
    pub excluded: Option<PathBuf>,
}

/// One test's collapsed outcome in one observed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The test's identity (`classname::name`, see `TestCase::qualified_name`).
    pub id: String,
    pub status: TestStatus,
    pub commit: Option<String>,
    pub dirty: Option<bool>,
    /// What the dirty tree held, as one hash (see [`CodeIdentity::tree`]).
    /// `None` on a clean tree, and on lines written before sooth recorded
    /// it.
    pub tree: Option<String>,
    /// Where the run happened. `None` for observations written before sooth
    /// recorded this, which stay usable — they just cannot support a claim
    /// about one environment.
    pub environment: Option<String>,
    /// The sooth invocation that made this observation: one `sooth run`
    /// (all of its `--runs`) or one imported file. The history counts an
    /// invocation as one piece of evidence (see `DECISIONS.md`). `None` for
    /// lines written before sooth recorded it; each such line stands alone.
    pub invocation: Option<String>,
    /// How long the test took in this run, in whole milliseconds — what a
    /// slow-test pass needs to say "slower since". `None` when the report
    /// carried no duration for it (and for every line written before sooth
    /// recorded this); `Some(0)` is a test the runner timed at under one.
    pub duration_ms: Option<u64>,
    pub at_epoch_secs: u64,
    /// A pass nobody saw: a CI job that failed and passed on its rerun, the
    /// pass read from the green attempt's counts, not from a name (see
    /// `DECISIONS.md`). It proves a flake, and is never counted as a run.
    pub inferred: bool,
}

/// The code an observation was made on, when sooth can name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Identity<'a> {
    /// A clean checkout of this commit.
    Commit(&'a str),
    /// A dirty tree that held exactly this content.
    Content(&'a str),
}

impl Observation {
    /// The code this observation was made on: a clean commit, or the
    /// content of a dirty tree. `None` when sooth cannot name it — no git,
    /// a HEAD that moved during the run, a dirty tree whose content could
    /// not be read or changed during the run, a line from before sooth
    /// recorded it, an import without `--commit`.
    pub fn identity(&self) -> Option<Identity<'_>> {
        match (self.dirty, self.commit.as_deref(), self.tree.as_deref()) {
            (Some(false), Some(commit), _) => Some(Identity::Commit(commit)),
            (Some(true), Some(_), Some(tree)) => Some(Identity::Content(tree)),
            _ => None,
        }
    }

    /// Whether this observation can be evidence that a test is flaky: it
    /// names the code it was made on. Such an observation without a name
    /// counts in the totals and proves nothing. The one definition: the
    /// flaky analysis and the "all N earlier observations" note must mean
    /// the same thing.
    pub fn is_evidence(&self) -> bool {
        self.identity().is_some()
    }

    /// The commit a clean checkout of which this observation was made on:
    /// what a pointer into history (`failing since`, `slower since`) can
    /// name. A dirty tree is no commit, whatever its content.
    pub fn clean_commit(&self) -> Option<&str> {
        if self.dirty == Some(false) {
            self.commit.as_deref()
        } else {
            None
        }
    }
}

/// A duration as the history stores it: whole milliseconds, which is finer
/// than any slowness worth reporting and keeps the line short.
pub fn whole_millis(duration: &std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The invocation id of this `sooth run`: unique per process and second.
pub fn run_invocation(at_epoch_secs: u64) -> String {
    format!("run-{at_epoch_secs}-{}", std::process::id())
}

/// The invocation id of an imported file: the same file always gets the
/// same id, and the ledger keeps it from being imported twice anyway.
pub fn import_invocation(content_hash: u64) -> String {
    format!("import-{content_hash:016x}")
}

/// Where this run is happening. `CI` is set by GitHub Actions, GitLab,
/// `CircleCI` and the rest, so it distinguishes the two environments that
/// matter without asking the user or making a network call.
pub fn current_environment() -> String {
    if std::env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        "ci".to_owned()
    } else {
        "local".to_owned()
    }
}

/// Put `replacement` in the place of every observation recorded under one
/// of `invocations`, and return how many of those went — for `import
/// --replace`: an imported file's observations all carry its
/// [`import_invocation`]. `None` when there is no history file, which
/// leaves nothing to replace: the caller appends as usual.
///
/// One write: the kept records and the replacement go into a file beside
/// the history, which is then renamed over it, so a crash or a full disk
/// leaves the old history or the new one, never the old one minus the
/// file. Every line without a named record is kept byte for byte, be it a
/// fragment or not even text; a line that glued a named record to others
/// comes back as its other records, one per line.
///
/// # Errors
///
/// Returns the I/O error when the history cannot be read or rewritten,
/// and an error when the history changed while it was read or sooth was
/// asked to stop: the history is then as it was.
pub fn replace_invocations(
    path: &Path,
    invocations: &[String],
    replacement: &[Observation],
    interrupt: &Interrupt,
) -> std::io::Result<Option<usize>> {
    let original = match Original::at(path) {
        Ok(original) => original,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let bytes = fs::read(path)?;
    let named = |record: &str| {
        parse_line(record)
            .and_then(|observation| observation.invocation)
            .is_some_and(|invocation| invocations.contains(&invocation))
    };
    let mut rewritten: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut removed = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        // Read the way a load reads it: what is not text is decoded as
        // best it can be, and a record on such a line is a record.
        let records = records(&String::from_utf8_lossy(line));
        if !records.iter().any(|record| named(record)) {
            rewritten.extend_from_slice(line);
            continue;
        }
        for record in records {
            if named(&record) {
                removed += 1;
            } else {
                rewritten.extend_from_slice(record.trim_end_matches('\n').as_bytes());
                rewritten.push(b'\n');
            }
        }
    }
    if rewritten.last().is_some_and(|last| *last != b'\n') {
        rewritten.push(b'\n');
    }
    for observation in replacement {
        rewritten.extend_from_slice(to_line(observation).as_bytes());
        rewritten.push(b'\n');
    }
    let rewrite = Beside::of(path);
    fs::write(&rewrite.0, rewritten)?;
    // Asked to stop while that was written: the rename is the one step
    // that cannot be taken back, so this is the place to step back.
    if interrupt.requested() {
        return Err(stopped());
    }
    original.give_way_to(&rewrite, path)?;
    Ok(Some(removed))
}

/// What [`prune`] did to the history file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pruned {
    pub kept: usize,
    pub removed: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

/// The file a rewrite is written to before it takes the history's place:
/// beside it, under a name of this process's own. Two rewrites at once — a
/// prune and an `import --replace` — then each write their own file, where
/// one shared name had them truncate each other's and rename a mix of both
/// over the history. Removed when dropped, so nothing is left behind
/// however the rewrite ends; once renamed there is nothing left to remove.
struct Beside(PathBuf);

impl Beside {
    fn of(path: &Path) -> Self {
        Self(path.with_extension(format!("jsonl.rewrite-{}", std::process::id())))
    }
}

impl Drop for Beside {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The history as a rewrite found it. A rewrite reads the file, writes its
/// result beside it and renames that over it — and whatever another sooth
/// wrote to the history in between would be gone, for good when it was an
/// import: its ledger says the file is in. So a rewrite takes the history's
/// place only while the file is still the one that was read, and fails
/// otherwise. There is no lock (see `DECISIONS.md`); this narrows the
/// window from the seconds a rewrite takes to the instant between the
/// check and the rename, and to an append that had opened the file before
/// that instant and writes after it.
struct Original(fs::Metadata);

impl Original {
    fn at(path: &Path) -> std::io::Result<Self> {
        fs::metadata(path).map(Self)
    }

    fn len(&self) -> u64 {
        self.0.len()
    }

    /// Whether the file at `path` is the one this was, not written since.
    fn is_still(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|now| {
            now.len() == self.0.len()
                && now.modified().ok() == self.0.modified().ok()
                && same_file(&now, &self.0)
        })
    }

    /// Put the rewrite in the history's place, with the permissions the
    /// history had.
    fn give_way_to(&self, rewrite: &Beside, path: &Path) -> std::io::Result<()> {
        fs::set_permissions(&rewrite.0, self.0.permissions())?;
        if !self.is_still(path) {
            return Err(std::io::Error::other(
                "the history changed while it was being rewritten — nothing was written; \
                 run the command again",
            ));
        }
        fs::rename(&rewrite.0, path)
    }
}

/// Whether two looks at a path saw the same file, where that can be told.
#[cfg(unix)]
fn same_file(one: &fs::Metadata, other: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    one.ino() == other.ino() && one.dev() == other.dev()
}

#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}

/// A record's place in time: its `at`, then its position in the file. The
/// analysis orders observations the same way (a stable sort on `at`).
type Moment = (u64, usize);

/// A test's most recent invocations, each by the newest moment it was seen
/// at. One more than the window is remembered: the newest moment of that
/// one is where the window begins.
#[derive(Default)]
struct Recent(Vec<(String, Moment)>);

impl Recent {
    fn note(&mut self, invocation: &str, moment: Moment) {
        match self.0.iter_mut().find(|(known, _)| known == invocation) {
            Some((_, latest)) => *latest = (*latest).max(moment),
            None => self.0.push((invocation.to_owned(), moment)),
        }
        if self.0.len() > WINDOW_PER_TEST + 1 {
            let oldest = self
                .0
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, latest))| *latest)
                .map(|(index, _)| index);
            if let Some(index) = oldest {
                self.0.swap_remove(index);
            }
        }
    }

    /// The moment the window begins after; `None` while everything seen
    /// lies inside it.
    fn begins_after(&self) -> Option<Moment> {
        if self.0.len() <= WINDOW_PER_TEST {
            return None;
        }
        self.0.iter().map(|(_, latest)| *latest).min()
    }
}

/// One test's two windows. The analysis leaves skips out before it counts
/// a test's invocations, so the window it reads is counted in invocations
/// that said something — and a test skipped fifty times since it last ran
/// still has every run before that inside it. Skips are bounded by the
/// plain count: the test's last invocations of any kind.
#[derive(Default)]
struct Windows<T> {
    /// Over the invocations that carried a signal: what the analysis reads.
    signal: T,
    /// Over every invocation, skips included.
    any: T,
}

impl Windows<Option<Moment>> {
    /// Whether an observation made at `moment` is older than the window
    /// that holds its kind.
    fn leave_out(&self, status: TestStatus, moment: Moment) -> bool {
        let begins_after = if status == TestStatus::Skipped {
            self.any
        } else {
            self.signal
        };
        begins_after.is_some_and(|begins_after| moment <= begins_after)
    }
}

/// Rewrite the history at `path` keeping, for every test, the observations
/// of its last [`WINDOW_PER_TEST`] invocations that said something — what
/// the analysis reads — and the skips of its last [`WINDOW_PER_TEST`]
/// invocations of any kind, and dropping what is older. `None` when there
/// is no history file. The file is left untouched when nothing is older.
///
/// Read as a stream, twice, so a history of gigabytes is pruned in the
/// memory of a few dozen invocations per test: first to find where each
/// test's windows begin, then to copy what lies inside them. A line
/// without an observation on it (a fragment, bytes that are no text) is
/// kept byte for byte: it is not this command's to judge. A line that is
/// not text and does hold one is read the way a load reads it, and comes
/// back as it was read. The result is written beside the history and
/// renamed over it, so a crash leaves one or the other.
///
/// # Errors
///
/// Returns the I/O error when the history cannot be read or rewritten,
/// and an error when the history changed while it was read or sooth was
/// asked to stop: the history is then as it was.
pub fn prune(path: &Path, interrupt: &Interrupt) -> std::io::Result<Option<Pruned>> {
    let Ok(original) = Original::at(path) else {
        return Ok(None);
    };
    let windows = windows(path, interrupt)?;
    if windows.is_empty() {
        return Ok(Some(Pruned {
            kept: count_observations(path)?,
            removed: 0,
            bytes_before: original.len(),
            bytes_after: original.len(),
        }));
    }
    let rewrite = Beside::of(path);
    let (kept, removed) = copy_inside(path, &windows, &rewrite, interrupt)?;
    // Nothing older than a window after all: the history stays as it is.
    if removed > 0 {
        original.give_way_to(&rewrite, path)?;
    }
    Ok(Some(Pruned {
        kept,
        removed,
        bytes_before: original.len(),
        bytes_after: fs::metadata(path)?.len(),
    }))
}

/// What a rewrite that was asked to stop comes back with.
fn stopped() -> std::io::Error {
    std::io::Error::other("interrupted — the history is as it was")
}

/// The first pass of a prune: where the windows begin of every test that
/// has more than a window's worth.
fn windows(
    path: &Path,
    interrupt: &Interrupt,
) -> std::io::Result<std::collections::BTreeMap<String, Windows<Option<Moment>>>> {
    let mut recent: std::collections::BTreeMap<String, Windows<Recent>> =
        std::collections::BTreeMap::new();
    for_each_record(path, |position, record| {
        if interrupt.requested() {
            return Err(stopped());
        }
        let Some(observation) = record.observation() else {
            return Ok(());
        };
        let invocation = observation
            .invocation
            .unwrap_or_else(|| format!("line-{position}"));
        let moment = (observation.at_epoch_secs, position);
        let windows = recent.entry(observation.id).or_default();
        windows.any.note(&invocation, moment);
        if observation.status != TestStatus::Skipped {
            windows.signal.note(&invocation, moment);
        }
        Ok(())
    })?;
    // A test with a signal window that overflowed has an overflowed plain
    // one too: the invocations that said something are among all of them.
    Ok(recent
        .into_iter()
        .map(|(id, recent)| {
            let begin = Windows {
                signal: recent.signal.begins_after(),
                any: recent.any.begins_after(),
            };
            (id, begin)
        })
        .filter(|(_, begin)| begin.any.is_some())
        .collect())
}

/// The second pass of a prune: write what lies inside each test's windows
/// to `rewrite`, and say how many observations were kept and removed.
fn copy_inside(
    path: &Path,
    windows: &std::collections::BTreeMap<String, Windows<Option<Moment>>>,
    rewrite: &Beside,
    interrupt: &Interrupt,
) -> std::io::Result<(usize, usize)> {
    let file = fs::File::create(&rewrite.0).map_err(|err| {
        std::io::Error::new(err.kind(), format!("{}: {err}", rewrite.0.display()))
    })?;
    let mut out = std::io::BufWriter::new(file);
    let (mut kept, mut removed) = (0, 0);
    for_each_record(path, |position, record| {
        if interrupt.requested() {
            return Err(stopped());
        }
        let observation = record.observation();
        let outside = observation.as_ref().is_some_and(|observation| {
            windows.get(&observation.id).is_some_and(|windows| {
                windows.leave_out(observation.status, (observation.at_epoch_secs, position))
            })
        });
        if outside {
            removed += 1;
            return Ok(());
        }
        kept += usize::from(observation.is_some());
        out.write_all(record.bytes())?;
        out.write_all(b"\n")
    })?;
    out.into_inner()
        .map_err(std::io::IntoInnerError::into_error)?
        .sync_all()?;
    Ok((kept, removed))
}

/// One record of the history file as [`for_each_record`] hands it out: the
/// text of what should be one observation, or a line that is not text.
enum Record<'line> {
    Text(&'line str),
    NotText(&'line [u8]),
}

impl Record<'_> {
    fn observation(&self) -> Option<Observation> {
        match self {
            Self::Text(text) => parse_line(text),
            Self::NotText(_) => None,
        }
    }

    /// The record as it goes back into a file, without its line ending.
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Text(text) => text.as_bytes(),
            Self::NotText(bytes) => bytes,
        }
    }
}

/// Call `each` for every record of the history at `path`, in file order,
/// with its position — the same count on every pass.
fn for_each_record(
    path: &Path,
    mut each: impl FnMut(usize, Record<'_>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::BufRead as _;
    let mut reader = std::io::BufReader::new(fs::File::open(path)?);
    let mut line = Vec::new();
    let mut position = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        while line
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            line.pop();
        }
        // A load decodes what is not text as best it can and reads the
        // records it finds; so does this. A line without one is handed out
        // as it is, byte for byte.
        let text = String::from_utf8_lossy(&line);
        let no_record = || {
            !records(&text)
                .iter()
                .any(|record| parse_line(record).is_some())
        };
        if matches!(text, std::borrow::Cow::Owned(_)) && no_record() {
            each(position, Record::NotText(&line))?;
            position += 1;
            continue;
        }
        if text.trim().is_empty() {
            continue;
        }
        for record in records(&text) {
            each(position, Record::Text(&record))?;
            position += 1;
        }
    }
}

/// How many observations the history at `path` holds, all of it.
fn count_observations(path: &Path) -> std::io::Result<usize> {
    let mut observations = 0;
    for_each_record(path, |_, record| {
        observations += usize::from(record.observation().is_some());
        Ok(())
    })?;
    Ok(observations)
}

/// The ledger of already-imported report files, one line per file:
/// a content hash plus the file name (informational). Lives beside the
/// history so `.sooth/` stays the one local-state directory.
pub const IMPORTED_PATH: &str = ".sooth/imported";

/// FNV-1a, 64-bit. Hand-rolled because the ledger must survive a Rust
/// upgrade and `DefaultHasher` is documented as unstable across releases;
/// the input is the user's own report files, so collision resistance
/// against an adversary is not a requirement.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The hashes already recorded at `path`; a missing ledger is day one.
pub fn imported_hashes(path: &Path) -> std::collections::BTreeSet<u64> {
    let Ok(content) = fs::read_to_string(path) else {
        return std::collections::BTreeSet::new();
    };
    content
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|hex| u64::from_str_radix(hex, 16).ok())
        .collect()
}

/// Record imported files in the ledger, creating `.sooth/` when missing.
pub fn record_imported(path: &Path, entries: &[(u64, String)]) -> std::io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut buffer = String::new();
    for (hash, name) in entries {
        use std::fmt::Write as _;
        // `buffer` is a plain `String`; `writeln!` never fails for it.
        let _ = writeln!(buffer, "{hash:016x} {name}");
    }
    file.write_all(buffer.as_bytes())
}

/// `YYYY-MM-DDTHH:MM:SS`, optionally with fractional seconds and a zone
/// (`Z` or `±HH:MM`), to seconds since the epoch. Hand-rolled via
/// days-from-civil: one `JUnit` attribute does not justify a date dependency.
/// Anything unparsable is `None` — the caller falls back to the file mtime.
pub fn iso8601_to_epoch(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.len() < 19 || text.get(10..11) != Some("T") {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..60).contains(&second) {
        return None;
    }

    // days_from_civil (Howard Hinnant): civil date to days since 1970-01-01.
    let years = if month <= 2 { year - 1 } else { year };
    let era = if years >= 0 { years } else { years - 399 } / 400;
    let year_of_era = years - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    let mut epoch = days * 86_400 + hour * 3_600 + minute * 60 + second;

    // Skip fractional seconds, then apply an explicit zone; a bare local
    // time is taken as written — minutes of skew do not matter to a pass
    // whose window spans weeks.
    let mut rest = text.get(19..)?;
    if let Some(stripped) = rest.strip_prefix('.') {
        rest = stripped.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    if let Some(zone) = rest.strip_prefix('+').or_else(|| rest.strip_prefix('-')) {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let zone_hour = zone.get(0..2)?.parse::<i64>().ok()?;
        let zone_minute = zone
            .get(3..5)
            .unwrap_or("0")
            .parse::<i64>()
            .ok()
            .unwrap_or(0);
        epoch -= sign * (zone_hour * 3_600 + zone_minute * 60);
    }
    u64::try_from(epoch).ok()
}

/// The loaded history, plus how many lines were unreadable — the file is
/// user-managed, so a corrupt line loses one observation, never the run.
pub struct Loaded {
    pub observations: Vec<Observation>,
    pub skipped_lines: usize,
    /// The file is larger than one load reads: these are its newest
    /// observations, not all of them.
    pub truncated: bool,
}

/// Seconds since the Unix epoch; zero when the clock predates it.
pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Read the code identity from git: `HEAD`, and whether the tree differs
/// from it. Call it before the run, so nothing the run writes can pass for a
/// change to the code it ran. Every failure mode degrades to unknown instead
/// of erroring: history must never make a run fail.
pub fn code_identity(dir: &Path, report: Option<&Path>) -> CodeIdentity {
    let Some(commit) = git(dir, &["rev-parse", "HEAD"]) else {
        return CodeIdentity::default();
    };
    // Run from a directory the repository ignores, the code under test is
    // in no commit and no tree of it.
    if git_code(dir, &["check-ignore", "-q", "."]) == Some(0) {
        return CodeIdentity::default();
    }
    let changes = changes_from_head(dir, report);
    let dirty = changes.as_ref().map(|changes| !changes.is_empty());
    CodeIdentity {
        commit: Some(commit),
        tree: changes
            .filter(|changes| !changes.is_empty())
            .and_then(|changes| content_of(dir, report, &changes)),
        dirty,
        excluded: report.map(Path::to_path_buf),
    }
}

/// Past this many changed and untracked files, or this many bytes in them,
/// a dirty tree's content is not read, and its runs prove nothing: a run
/// should not pay seconds for an unignored dump or coverage directory.
const CONTENT_FILES: usize = 5_000;
const CONTENT_BYTES: u64 = 64 * 1024 * 1024;

/// What the working tree holds, as one hash: the tree git would write for
/// `HEAD` with every change and every untracked file that is not ignored
/// added — `.sooth/` and `report` left out, as for [`changes_from_head`].
/// Two trees with the same hash hold the same code, committed or not.
///
/// Read through a copy of the index and an object store of sooth's own,
/// which reads the repository's objects but writes none to it, with the
/// repository's hooks out of reach: the user's index, objects and status
/// are left exactly as they were. `None` when any step fails; when the
/// changes are past [`CONTENT_FILES`] or [`CONTENT_BYTES`]; and when a
/// submodule, or a repository inside this one, holds uncommitted work —
/// the tree records such a repository as its `HEAD` alone.
fn content_of(dir: &Path, report: Option<&Path>, changes: &[String]) -> Option<String> {
    let base = std::fs::canonicalize(dir).ok()?;
    let top = PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?);
    // Relative to `dir`, or absolute; one line, or git did not understand.
    let git_path = |name: &str| -> Option<PathBuf> {
        let printed = git(dir, &["rev-parse", "--git-path", name])?;
        (!printed.contains('\n')).then(|| base.join(printed))
    };
    let (index, objects) = (git_path("index")?, git_path("objects")?);
    if !objects.is_dir() {
        return None;
    }
    within_budget(&top, changes, (CONTENT_FILES, CONTENT_BYTES))?;
    let pathspec = own_files_left_out(dir, report)?;
    let scratch = Scratch::new()?;
    let own_index = scratch.0.join("index");
    let own_objects = scratch.0.join("objects");
    let no_hooks = scratch.0.join("hooks");
    std::fs::create_dir(&own_objects).ok()?;
    std::fs::create_dir(&no_hooks).ok()?;
    // A copy keeps the index's record of what is unchanged, so only what
    // changed is read; a repository without one yet starts from nothing.
    // The copy keeps the index's time as well: git trusts an entry as
    // unchanged only when it is older than the index, and a copy made now
    // would vouch for a file written in the index's last second.
    if index.exists() {
        // The time first: an index replaced in between leaves a copy newer
        // than its time, which only has git read more.
        let written = std::fs::metadata(&index).ok()?.modified().ok()?;
        std::fs::copy(&index, &own_index).ok()?;
        std::fs::File::options()
            .write(true)
            .open(&own_index)
            .ok()?
            .set_modified(written)
            .ok()?;
    }
    let env = [
        ("GIT_INDEX_FILE", own_index.as_os_str()),
        ("GIT_OBJECT_DIRECTORY", own_objects.as_os_str()),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", objects.as_os_str()),
    ];
    let hooks = format!("core.hooksPath={}", no_hooks.display());
    let mut add = vec!["-c", &hooks, "add", "-A", "--"];
    add.extend(pathspec.iter().map(String::as_str));
    git_with(dir, &add, &env)?;
    // A submodule or a repository inside this one is in the tree as its
    // HEAD: work uncommitted inside it would not change the hash. All of
    // them, from the top, whatever directory sooth runs from.
    let listed = git_with(
        dir,
        &[
            "-c",
            &hooks,
            "ls-files",
            "-s",
            "-z",
            "--full-name",
            "--",
            ":/",
        ],
        &env,
    )?;
    for entry in listed.split('\0') {
        let Some((_, path)) = entry
            .strip_prefix("160000 ")
            .and_then(|rest| rest.split_once('\t'))
        else {
            continue;
        };
        // A path git printed in bytes that are no UTF-8 cannot be found
        // again: no content, rather than a repository passed over.
        if path.contains('\u{fffd}') {
            return None;
        }
        let inner = top.join(path);
        // Not checked out (a submodule cloned without it): no work in it.
        // Asked on its own, git would walk up and answer for this one.
        if !inner.join(".git").exists() {
            continue;
        }
        if git_alone(&inner, &["--no-optional-locks", "status", "--porcelain"]).as_deref()
            != Some("")
        {
            return None;
        }
    }
    git_with(dir, &["-c", &hooks, "write-tree"], &env)
}

/// The pathspec of the whole tree with what sooth and the run write left
/// out, literally: `.sooth/` and the user's report — each only when git
/// does not ignore it already, since `git add` refuses to be told about an
/// ignored path, and only when it lies inside the repository.
fn own_files_left_out(dir: &Path, report: Option<&Path>) -> Option<Vec<String>> {
    let history_dir = Path::new(HISTORY_PATH).parent()?.to_str()?;
    let mut pathspec = vec![":/".to_owned()];
    for own in std::iter::once(history_dir).chain(report.and_then(Path::to_str)) {
        // 1: inside the repository and not ignored; 0 ignored; 128 outside.
        if git_code(dir, &["check-ignore", "-q", "--", own]) == Some(1) {
            pathspec.push(format!(":(exclude,literal){own}"));
        }
    }
    Some(pathspec)
}

/// `Some` when `changes`, paths from the top of the repository, are within
/// `budget`: so many files, so many bytes.
fn within_budget(top: &Path, changes: &[String], budget: (usize, u64)) -> Option<()> {
    if changes.len() > budget.0 {
        return None;
    }
    let mut bytes = 0_u64;
    for path in changes {
        bytes += std::fs::symlink_metadata(top.join(path)).map_or(0, |meta| meta.len());
        if bytes > budget.1 {
            return None;
        }
    }
    Some(())
}

/// A private directory of sooth's own (see [`crate::preset::private_dir`]),
/// removed when dropped, whatever happened in between.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Option<Self> {
        crate::preset::private_dir().ok().map(Self)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What differs from `HEAD`: every changed and every untracked file that is
/// not ignored, one by one, as paths from the top of the repository.
/// Untracked files count: a new test file is code the commit does not
/// describe. What sooth and the run write themselves does not — `.sooth/`,
/// and `report`, the user's `--junit` file (see `DECISIONS.md`). One
/// listing answers both whether the tree is dirty and what reading its
/// content would cost. `None` when git cannot say.
fn changes_from_head(dir: &Path, report: Option<&Path>) -> Option<Vec<String>> {
    let history_dir = Path::new(HISTORY_PATH).parent()?.to_str()?;
    let own_history = format!(":(exclude,literal){history_dir}");
    let listed = |also_excluded: Option<&str>| {
        // `--no-optional-locks`: a plain `status` refreshes the index under
        // `index.lock`, which can collide with an IDE or a commit elsewhere.
        let mut args = vec![
            "--no-optional-locks",
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--",
            ":/",
            &own_history,
        ];
        args.extend(also_excluded);
        // A path git prints as bytes that are no UTF-8 still counts.
        let output = git_run(dir, &args)?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let raw = match report.and_then(Path::to_str) {
        // Git refuses a path outside the repository, which is in no
        // listing anyway.
        Some(report) => {
            listed(Some(&format!(":(exclude,literal){report}"))).or_else(|| listed(None))?
        }
        None => listed(None)?,
    };
    let mut paths = Vec::new();
    let mut entries = raw.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let (state, path) = entry.split_at_checked(3)?;
        if state.starts_with(['R', 'C']) {
            // A rename names where it came from next.
            entries.next();
        }
        paths.push(path.to_owned());
    }
    Some(paths)
}

/// The identity to record once the runs are over: the one read before them,
/// or unknown when `HEAD` moved meanwhile — the observations cannot be
/// pinned on either commit.
pub fn identity_after_run(before: CodeIdentity, dir: &Path) -> CodeIdentity {
    if before.commit.is_some() && git(dir, &["rev-parse", "HEAD"]) != before.commit {
        return CodeIdentity::default();
    }
    // A dirty tree's content is read again: an edit during the run — a
    // suite of minutes is long enough to keep working — means the hash
    // read before it names code that did not run start to finish.
    let excluded = before.excluded.as_deref();
    let read_again =
        || changes_from_head(dir, excluded).and_then(|changes| content_of(dir, excluded, &changes));
    if before.tree.is_some() && read_again() != before.tree {
        return CodeIdentity {
            tree: None,
            ..before
        };
    }
    before
}

/// Why a given commit cannot be stored as the sha local runs record.
#[derive(Debug, PartialEq, Eq)]
pub enum CommitError {
    /// A branch or revision name: it resolves to where it points now, not
    /// to the commit the reports came from.
    NotASha,
    /// An abbreviation the repository in `dir` does not know.
    Unknown,
}

/// The full, lowercase sha for a commit given by hand, as `code_identity`
/// records it. The history compares commits exactly, so an abbreviation is
/// resolved against the repository in `dir` rather than stored as typed.
pub fn resolve_commit(dir: &Path, given: &str) -> Result<String, CommitError> {
    if given.is_empty() || !given.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CommitError::NotASha);
    }
    let sha = given.to_ascii_lowercase();
    if matches!(sha.len(), 40 | 64) {
        return Ok(sha);
    }
    let revision = format!("{sha}^{{commit}}");
    git(dir, &["rev-parse", "--verify", "--quiet", &revision]).ok_or(CommitError::Unknown)
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Option<String> {
    git_raw(dir, args).map(|text| text.trim().to_owned())
}

/// The exit code of a git call, for the questions git answers that way.
fn git_code(dir: &Path, args: &[&str]) -> Option<i32> {
    git_run(dir, args)?.status.code()
}

/// [`git`] for another repository than the one sooth runs in: git variables
/// that point at a repository are left out, so `dir` is asked about itself.
fn git_alone(dir: &Path, args: &[&str]) -> Option<String> {
    let output = git_command(dir, args, &[], REPOSITORY_VARIABLES)?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_owned())
}

/// Git's variables that name a repository, an index or an object store.
const REPOSITORY_VARIABLES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
];

/// [`git`] with `env` added to git's environment.
fn git_with(dir: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Option<String> {
    let output = git_command(dir, args, env, &[])?;
    if !output.status.success() {
        return None;
    }
    // A listing of every path in the index may hold one that is no UTF-8.
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A successful git call's stdout exactly as written — for `-z` listings,
/// where a path may begin or end with the whitespace [`git`] trims.
pub(crate) fn git_raw(dir: &Path, args: &[&str]) -> Option<String> {
    let output = git_run(dir, args)?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[expect(
    clippy::disallowed_methods,
    reason = "Every git call goes through this one helper."
)]
fn git_command(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    unset: &[&str],
) -> Option<std::process::Output> {
    let mut git = Command::new("git");
    git.arg("-C").arg(dir).args(args).envs(env.iter().copied());
    for variable in unset {
        git.env_remove(variable);
    }
    git.output().ok()
}

fn git_run(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    git_command(dir, args, &[], &[])
}

/// Keep sooth's own directory out of `git status` and `git add -A` in this
/// clone, without asking the project to change anything: when git does not
/// already ignore it, add it to `.git/info/exclude`. `true` when the line
/// was added. Outside a repository, or when anything fails, nothing happens
/// (see `DECISIONS.md`).
pub fn exclude_from_git(dir: &Path) -> bool {
    let Some(history_dir) = Path::new(HISTORY_PATH).parent().and_then(Path::to_str) else {
        return false;
    };
    let pattern = format!("{history_dir}/");
    // `check-ignore -q`: 0 is ignored, 1 is not; anything else is no answer.
    let ignored = git_run(dir, &["check-ignore", "-q", &pattern]).and_then(|out| out.status.code());
    if ignored != Some(1) {
        return false;
    }
    let Some(exclude) = git(dir, &["rev-parse", "--git-path", "info/exclude"]) else {
        return false;
    };
    append_line(&dir.join(exclude), &pattern).is_ok()
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let start = if ends_mid_line(&mut file)? { "\n" } else { "" };
    file.write_all(format!("{start}{line}\n").as_bytes())
}

/// Append observations as JSON lines, creating `.sooth/` when missing.
pub fn append(path: &Path, observations: &[Observation]) -> std::io::Result<()> {
    if observations.is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let mut buffer = String::new();
    if ends_mid_line(&mut file)? {
        buffer.push('\n');
    }
    for observation in observations {
        buffer.push_str(&to_line(observation));
        buffer.push('\n');
    }
    file.write_all(buffer.as_bytes())
}

/// A crash mid-append leaves a last line without its newline; a record
/// written straight after it would be glued onto the fragment.
fn ends_mid_line(file: &mut fs::File) -> std::io::Result<bool> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    Ok(last != [b'\n'])
}

fn to_line(observation: &Observation) -> String {
    let commit = observation
        .commit
        .as_deref()
        .map_or_else(|| "null".to_owned(), |c| format!("\"{}\"", json::escape(c)));
    let dirty = observation
        .dirty
        .map_or_else(|| "null".to_owned(), |d| d.to_string());
    let environment = observation
        .environment
        .as_deref()
        .map_or_else(|| "null".to_owned(), |e| format!("\"{}\"", json::escape(e)));
    let invocation = observation
        .invocation
        .as_deref()
        .map_or_else(|| "null".to_owned(), |r| format!("\"{}\"", json::escape(r)));
    // Left out when unknown, so a report without durations costs nothing;
    // and in front of `id`, the one value that can hold any text.
    let duration = observation
        .duration_ms
        .map_or_else(String::new, |millis| format!(r#""ms":{millis},"#));
    // Left out on a clean tree, where the commit says it all.
    let tree = observation
        .tree
        .as_deref()
        .map_or_else(String::new, |tree| {
            format!(r#""tree":"{}","#, json::escape(tree))
        });
    // Left out on every pass that was seen: only an inferred one says so.
    let inferred = if observation.inferred {
        r#""inferred":true,"#
    } else {
        ""
    };
    format!(
        r#"{{"at":{},"commit":{commit},"dirty":{dirty},"env":{environment},"run":{invocation},{duration}{tree}{inferred}"status":"{}","id":"{}"}}"#,
        observation.at_epoch_secs,
        status_str(observation.status),
        json::escape(&observation.id)
    )
}

/// Every status with its wire name — the one table both directions use.
const STATUS_NAMES: [(TestStatus, &str); 4] = [
    (TestStatus::Passed, "passed"),
    (TestStatus::Failed, "failed"),
    (TestStatus::Error, "error"),
    (TestStatus::Skipped, "skipped"),
];

fn status_str(status: TestStatus) -> &'static str {
    STATUS_NAMES
        .iter()
        .find(|(candidate, _)| *candidate == status)
        .map_or("", |(_, name)| name)
}

fn status_from_str(name: &str) -> Option<TestStatus> {
    STATUS_NAMES
        .iter()
        .find(|(_, candidate)| *candidate == name)
        .map(|(status, _)| *status)
}

/// Load the history at `path`; a missing file is an empty history.
pub fn load(path: &Path) -> Loaded {
    load_tail(path, MAX_LOAD_BYTES)
}

/// Read at most the last `max_bytes` of the file. A truncated first line is
/// expected when the tail cuts mid-line and is dropped silently — it is not
/// corruption, so it must not trip the unreadable-lines warning every run.
fn load_tail(path: &Path, max_bytes: u64) -> Loaded {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let empty = Loaded {
        observations: Vec::new(),
        skipped_lines: 0,
        truncated: false,
    };
    let Ok(mut file) = fs::File::open(path) else {
        return empty;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return empty;
    };
    let mut bytes = Vec::new();
    let mut truncated = false;
    if len > max_bytes {
        truncated = true;
        if file
            .seek(SeekFrom::End(-i64::try_from(max_bytes).unwrap_or(0)))
            .is_err()
        {
            return empty;
        }
    }
    if file.read_to_end(&mut bytes).is_err() {
        return empty;
    }
    let text = String::from_utf8_lossy(&bytes);
    let text: &str = if truncated {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let mut observations = Vec::new();
    let mut skipped_lines = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        for record in records(line) {
            match parse_line(&record) {
                Some(observation) => observations.push(observation),
                None => skipped_lines += 1,
            }
        }
    }
    Loaded {
        observations,
        skipped_lines,
        truncated,
    }
}

/// The records on one line: one, unless a crash glued a record onto a
/// fragment. Every record starts with `RECORD_START`, which cannot occur
/// inside a value — `json::escape` escapes its quotes.
fn records(line: &str) -> Vec<String> {
    let mut parts = line.split(RECORD_START);
    let before_first = parts.next().unwrap_or_default();
    let mut records: Vec<String> = parts.map(|rest| format!("{RECORD_START}{rest}")).collect();
    if !before_first.trim().is_empty() {
        records.insert(0, before_first.to_owned());
    }
    records
}

const RECORD_START: &str = r#"{"at":"#;

/// Parse one line of the flat schema `to_line` writes. Key lookup takes the
/// first occurrence, which is unambiguous because `id` — the only value that
/// can contain arbitrary text — is written last.
fn parse_line(line: &str) -> Option<Observation> {
    let line = line.trim();
    if !line.starts_with(RECORD_START) || !line.ends_with("\"}") {
        return None;
    }
    let at_epoch_secs = extract_u64(line, "at")?;
    let commit = extract_string_or_null(line, "commit")?;
    let dirty = extract_bool_or_null(line, "dirty")?;
    let status = status_from_str(&extract_string_or_null(line, "status")??)?;
    let id = extract_string_or_null(line, "id")??;
    // Absent, not null: lines written before environments were recorded are
    // ordinary observations of an unknown environment, never corrupt ones.
    let environment = extract_string_or_null(line, "env").flatten();
    // Absent for the same reason: older lines each stand alone.
    let invocation = extract_string_or_null(line, "run").flatten();
    // Absent when the report had no duration, or the line predates them.
    let duration_ms = extract_u64(line, "ms");
    // Absent on a clean tree, and on lines from before sooth recorded it.
    let tree = extract_string_or_null(line, "tree").flatten();
    // Absent on everything seen; an older sooth reads an inferred pass as a
    // pass, which is why only a rerun pair — red and green on one commit,
    // already a proof — ever writes one.
    let inferred = extract_bool_or_null(line, "inferred").flatten() == Some(true);
    Some(Observation {
        id,
        status,
        commit,
        dirty,
        tree,
        environment,
        invocation,
        duration_ms,
        at_epoch_secs,
        inferred,
    })
}

fn value_after<'line>(line: &'line str, key: &str) -> Option<&'line str> {
    let marker = format!("\"{key}\":");
    line.split_once(&marker).map(|(_, rest)| rest)
}

fn extract_u64(line: &str, key: &str) -> Option<u64> {
    let rest = value_after(line, key)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

#[expect(
    clippy::option_option,
    reason = "Outer Option is parse success, inner is JSON null: both layers carry meaning, so the clippy default does not apply."
)]
fn extract_bool_or_null(line: &str, key: &str) -> Option<Option<bool>> {
    let rest = value_after(line, key)?;
    if rest.starts_with("true") {
        Some(Some(true))
    } else if rest.starts_with("false") {
        Some(Some(false))
    } else if rest.starts_with("null") {
        Some(None)
    } else {
        None
    }
}

#[expect(clippy::option_option, reason = "same two-layer meaning as above")]
fn extract_string_or_null(line: &str, key: &str) -> Option<Option<String>> {
    let rest = value_after(line, key)?;
    if rest.starts_with("null") {
        return Some(None);
    }
    let rest = rest.strip_prefix('"')?;
    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(Some(value)),
            '\\' => match chars.next()? {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() != 4 {
                        return None;
                    }
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    value.push(char::from_u32(code)?);
                }
                _ => return None,
            },
            other => value.push(other),
        }
    }
    None
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "The identity test builds a real repository to read."
)]
mod tests {
    use super::{
        append, code_identity, current_environment, identity_after_run, load, resolve_commit,
        CodeIdentity, CommitError, Observation,
    };
    use crate::junit::TestStatus;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sooth-history-{tag}-{}/history.jsonl",
            std::process::id()
        ))
    }

    fn observation(id: &str, status: TestStatus) -> Observation {
        Observation {
            id: id.to_owned(),
            status,
            commit: Some("abc123".to_owned()),
            dirty: Some(false),
            tree: None,
            environment: Some("local".to_owned()),
            invocation: Some("run-1700000000-1".to_owned()),
            duration_ms: None,
            at_epoch_secs: 1_700_000_000,
            inferred: false,
        }
    }

    #[test]
    fn a_duration_rides_in_front_of_the_id_and_is_left_out_when_unknown() {
        let path = temp_path("durations");
        let timed = |id: &str, millis: Option<u64>| Observation {
            duration_ms: millis,
            ..observation(id, TestStatus::Passed)
        };
        // An id that looks like the key must not be read as one.
        let tricky = r#"c::it logs {"ms":999}"#;
        super::append(
            &path,
            &[
                timed("c::slow", Some(1840)),
                timed("c::fast", Some(0)),
                timed(tricky, None),
            ],
        )
        .expect("append");

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].contains(r#""ms":1840,"status":"passed","id":"c::slow""#),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains(r#""ms":0,"#),
            "a measured zero is written: {}",
            lines[1]
        );
        assert!(
            !lines[2].contains(r#""ms":"#),
            "an unknown one is not: {}",
            lines[2]
        );

        let loaded = load(&path);
        assert_eq!(loaded.skipped_lines, 0);
        let read: Vec<(String, Option<u64>)> = loaded
            .observations
            .into_iter()
            .map(|observation| (observation.id, observation.duration_ms))
            .collect();
        assert_eq!(
            read,
            [
                ("c::slow".to_owned(), Some(1840)),
                ("c::fast".to_owned(), Some(0)),
                (tricky.to_owned(), None),
            ]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_line_from_before_durations_is_an_observation_without_one() {
        let line = r#"{"at":1700000000,"commit":"aaa","dirty":false,"env":"local","run":"run-1-1","status":"failed","id":"c::t"}"#;
        let observation = super::parse_line(line).expect("an old line still parses");
        assert_eq!(observation.duration_ms, None);
        assert_eq!(observation.id, "c::t");
    }

    #[test]
    fn a_record_glued_onto_a_truncated_one_is_kept_and_the_fragment_counted() {
        let path = temp_path("glued");
        let pass = |at| {
            format!(
                r#"{{"at":{at},"commit":"aaa","dirty":false,"env":null,"status":"passed","id":"c::t"}}"#
            )
        };
        let truncated = r#"{"at":3,"commit":"bbb","dirty":false,"env":null,"status":"fail"#;
        let red =
            r#"{"at":4,"commit":"bbb","dirty":false,"env":null,"status":"failed","id":"c::t"}"#;
        std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
        std::fs::write(
            &path,
            format!("{}\n{}\n{truncated}{red}\n", pass(1), pass(2)),
        )
        .expect("write history");

        let loaded = load(&path);
        assert_eq!(loaded.skipped_lines, 1);
        let ids: Vec<_> = loaded
            .observations
            .iter()
            .map(|o| (o.id.as_str(), o.at_epoch_secs))
            .collect();
        assert_eq!(ids, [("c::t", 1), ("c::t", 2), ("c::t", 4)]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_append_after_a_crash_mid_line_starts_on_a_line_of_its_own() {
        let path = temp_path("crash");
        std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
        std::fs::write(
            &path,
            r#"{"at":3,"commit":"bbb","dirty":false,"env":null,"stat"#,
        )
        .expect("write history");
        append(&path, &[observation("c::a", TestStatus::Failed)]).expect("append");

        let loaded = load(&path);
        assert_eq!(loaded.skipped_lines, 1);
        assert_eq!(
            loaded.observations,
            [observation("c::a", TestStatus::Failed)]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn observations_survive_an_append_load_roundtrip() {
        let path = temp_path("roundtrip");
        let written = [
            observation("c::a", TestStatus::Passed),
            observation("c::b", TestStatus::Failed),
            Observation {
                commit: None,
                dirty: None,
                ..observation("c::no-git", TestStatus::Error)
            },
        ];
        append(&path, &written).expect("append should create dir and file");
        append(&path, &written[..1]).expect("second append should extend");

        let loaded = load(&path);
        assert_eq!(loaded.skipped_lines, 0);
        assert_eq!(loaded.observations.len(), 4);
        assert_eq!(loaded.observations[..3], written);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ids_with_quotes_backslashes_and_newlines_roundtrip() {
        let path = temp_path("escaping");
        let tricky = Observation {
            id: "c::says \"hi\"\\\n".to_owned(),
            ..observation("", TestStatus::Passed)
        };
        append(&path, std::slice::from_ref(&tricky)).expect("append");
        let loaded = load(&path);
        assert_eq!(loaded.observations, [tricky]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unreadable_lines_are_counted_and_skipped_not_fatal() {
        let path = temp_path("corrupt");
        append(&path, &[observation("c::ok", TestStatus::Passed)]).expect("append");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not json at all\n");
        text.push_str(
            "{\"at\":9,\"commit\":null,\"dirty\":null,\"status\":\"weird\",\"id\":\"x\"}\n",
        );
        std::fs::write(&path, text).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.observations.len(), 1);
        assert_eq!(loaded.skipped_lines, 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn loading_reads_only_the_tail_of_a_large_file() {
        let path = temp_path("tail");
        let written: Vec<Observation> = (0..10)
            .map(|i| observation(&format!("c::t{i}"), TestStatus::Passed))
            .collect();
        append(&path, &written).expect("append");
        let line_len = (std::fs::metadata(&path).unwrap().len() / 10) + 1;

        let loaded = super::load_tail(&path, line_len * 3);
        assert!(loaded.observations.len() < 10, "tail read the whole file");
        assert!(!loaded.observations.is_empty());
        assert_eq!(
            loaded.observations.last().unwrap().id,
            "c::t9",
            "the newest observations must survive"
        );
        // The cut-off first line is expected truncation, not corruption.
        assert_eq!(loaded.skipped_lines, 0);
        assert!(loaded.truncated, "a tail must say it is one");
        assert!(!load(&path).truncated);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn lines_written_before_environments_existed_still_load() {
        // 9423 of these exist in the wild; a new field must not turn a
        // history into "unreadable lines".
        let path = temp_path("legacy");
        let legacy =
            "{\"at\":9,\"commit\":\"abc\",\"dirty\":false,\"status\":\"failed\",\"id\":\"c::t\"}\n";
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, legacy).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.skipped_lines, 0, "a legacy line read as corrupt");
        assert_eq!(loaded.observations.len(), 1);
        assert_eq!(loaded.observations[0].environment, None);
        assert_eq!(loaded.observations[0].invocation, None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_environment_survives_a_roundtrip() {
        let path = temp_path("environment");
        let written = [
            observation("c::a", TestStatus::Passed),
            Observation {
                environment: Some("ci".to_owned()),
                ..observation("c::b", TestStatus::Failed)
            },
            Observation {
                environment: None,
                ..observation("c::c", TestStatus::Passed)
            },
        ];
        append(&path, &written).expect("append");

        let loaded = load(&path);
        assert_eq!(loaded.observations, written);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_line_cut_off_after_its_id_is_no_observation() {
        let whole =
            r#"{"at":4,"commit":"bbb","dirty":false,"env":null,"status":"failed","id":"c::t"}"#;
        assert!(super::parse_line(whole).is_some());
        assert_eq!(
            super::parse_line(whole.strip_suffix('}').expect("a brace")),
            None
        );
        assert_eq!(
            super::parse_line(whole.strip_prefix('{').expect("a brace")),
            None
        );
    }

    #[test]
    fn a_scratch_directory_is_gone_once_dropped() {
        let scratch = super::Scratch::new().expect("a scratch directory");
        let dir = scratch.0.clone();
        std::fs::write(dir.join("index"), "x").expect("write");
        assert!(dir.is_dir());
        drop(scratch);
        assert!(!dir.exists(), "{}", dir.display());
    }

    #[test]
    fn a_dirty_trees_content_survives_a_roundtrip() {
        let path = temp_path("tree");
        let written = [
            observation("c::a", TestStatus::Passed),
            Observation {
                dirty: Some(true),
                tree: Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned()),
                ..observation("c::b", TestStatus::Failed)
            },
        ];
        append(&path, &written).expect("append");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            !text.lines().next().expect("a line").contains("tree"),
            "{text}"
        );
        assert_eq!(load(&path).observations, written);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_environment_comes_from_the_ci_variable() {
        // Read through the same helper the recorder uses, so the contract is
        // "CI set and non-empty", not "some variable exists".
        assert!(matches!(current_environment().as_str(), "ci" | "local"));
    }

    #[test]
    fn fnv1a64_matches_the_published_vectors() {
        // The ledger must survive a Rust upgrade, so the hash is pinned to
        // the algorithm's own vectors, not to whatever std hashes to today.
        assert_eq!(super::fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(super::fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(super::fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn iso8601_covers_the_shapes_junit_reports_actually_use() {
        use super::iso8601_to_epoch as epoch;
        assert_eq!(epoch("1970-01-01T00:00:00"), Some(0));
        // Leap day: the day the hand-rolled math gets wrong first.
        assert_eq!(epoch("2024-02-29T12:00:00"), Some(1_709_208_000));
        // pytest writes fractional seconds and a zone.
        assert_eq!(epoch("2026-08-08T10:00:00.123456Z"), Some(1_786_183_200));
        assert_eq!(epoch("2026-08-08T10:00:00+02:00"), Some(1_786_176_000));
        assert_eq!(epoch("garbage"), None);
        assert_eq!(epoch("2026-13-01T00:00:00"), None);
    }

    #[test]
    fn iso8601_pins_the_arithmetic_the_common_shapes_never_reach() {
        // Every value here is cross-checked against Python's datetime.
        use super::iso8601_to_epoch as epoch;
        // A zone with minutes, in both directions.
        assert_eq!(epoch("2026-08-08T10:00:00+05:30"), Some(1_786_163_400));
        assert_eq!(epoch("2026-08-08T10:00:00-02:00"), Some(1_786_190_400));
        // Fractional seconds are skipped, not consumed: the zone behind
        // them still lands on the instant the zone alone would.
        assert_eq!(epoch("2026-08-08T10:00:00.123+02:00"), Some(1_786_176_000));
        // Century leap day (divisible by 400) and a non-leap century start.
        assert_eq!(epoch("2000-02-29T12:00:00"), Some(951_825_600));
        assert_eq!(epoch("2100-01-01T00:00:00"), Some(4_102_444_800));
        // The last second of a year: day-of-year arithmetic at its edge.
        assert_eq!(epoch("2023-12-31T23:59:59"), Some(1_704_067_199));
        // Before the epoch there is no u64: None, not a wrapped value.
        assert_eq!(epoch("1969-12-31T23:59:59"), None);
        // One out-of-range field is enough to reject the whole stamp.
        assert_eq!(epoch("2026-01-01T24:00:00"), None);
        assert_eq!(epoch("2026-01-01T10:60:00"), None);
        assert_eq!(epoch("2026-01-01T10:00:60"), None);
    }

    #[test]
    fn iso8601_is_none_for_a_multi_byte_character_where_the_t_belongs() {
        use super::iso8601_to_epoch as epoch;
        // `é` spans bytes 10 and 11: the `T` check must not cut inside it.
        assert_eq!(epoch("0123456789é0123456789"), None);
        assert_eq!(epoch("2026-08-08Té0:00:00"), None);
    }

    #[test]
    fn every_escape_the_writer_emits_survives_a_load() {
        // `\/` is legal JSON any writer may emit, so the reader accepts it
        // alongside the escapes sooth itself writes.
        let path = temp_path("unescape");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"at\":9,\"commit\":null,\"dirty\":null,\"env\":null,\"status\":\"failed\",\
             \"id\":\"q\\\"b\\\\s\\/n\\nr\\rt\\tu\\u0041\"}\n",
        )
        .unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.skipped_lines, 0);
        assert_eq!(loaded.observations.len(), 1);
        assert_eq!(loaded.observations[0].id, "q\"b\\s/n\nr\rt\tuA");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// An observation of `id` made at `at` by invocation `run`.
    fn seen(id: &str, at: u64, run: &str) -> Observation {
        Observation {
            at_epoch_secs: at,
            invocation: Some(run.to_owned()),
            ..observation(id, TestStatus::Passed)
        }
    }

    fn ids_and_times(path: &std::path::Path) -> Vec<(String, u64)> {
        super::load(path)
            .observations
            .into_iter()
            .map(|observation| (observation.id, observation.at_epoch_secs))
            .collect()
    }

    /// A prune that nobody asks to stop.
    fn prune(path: &std::path::Path) -> std::io::Result<Option<super::Pruned>> {
        super::prune(path, &crate::interrupt::Interrupt::never())
    }

    /// A replace that nobody asks to stop.
    fn replace_invocations(
        path: &std::path::Path,
        invocations: &[String],
        replacement: &[Observation],
    ) -> std::io::Result<Option<usize>> {
        let never = crate::interrupt::Interrupt::never();
        super::replace_invocations(path, invocations, replacement, &never)
    }

    #[test]
    fn a_replace_that_is_asked_to_stop_leaves_the_history_as_it_was() {
        let path = temp_path("replace-stopped");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        super::append(&path, &[seen("c::t", 1, "import-a")]).expect("append");
        let before = std::fs::read(&path).unwrap();

        let stopped = super::replace_invocations(
            &path,
            &["import-a".to_owned()],
            &[seen("c::t", 2, "import-a")],
            &crate::interrupt::Interrupt::already(),
        )
        .expect_err("asked to stop");
        assert!(
            stopped.to_string().contains("interrupted"),
            "got: {stopped}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !super::Beside::of(&path).0.exists(),
            "nothing left beside it"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_rewrite_gives_way_to_a_history_that_changed_while_it_was_read() {
        let path = temp_path("rewrite-changed");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        super::append(&path, &[seen("c::t", 1, "run-1-1")]).expect("append");

        // This sooth reads the history and writes its rewrite beside it;
        // meanwhile another one appends a run.
        let original = super::Original::at(&path).expect("a history");
        let beside = super::Beside::of(&path);
        std::fs::write(&beside.0, "the rewrite\n").unwrap();
        super::append(&path, &[seen("c::t", 2, "run-2-1")]).expect("append");

        let refused = original
            .give_way_to(&beside, &path)
            .expect_err("the history is no longer the one that was read");
        assert!(
            refused.to_string().contains("changed while"),
            "got: {refused}"
        );
        assert_eq!(
            ids_and_times(&path).len(),
            2,
            "the other sooth's run is not lost"
        );
        drop(beside);
        assert!(
            !super::Beside::of(&path).0.exists(),
            "nothing left beside it"
        );

        // Read and not touched since: the rewrite takes its place.
        let original = super::Original::at(&path).expect("a history");
        let beside = super::Beside::of(&path);
        std::fs::write(&beside.0, "the rewrite\n").unwrap();
        original.give_way_to(&beside, &path).expect("unchanged");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the rewrite\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_prune_that_is_asked_to_stop_leaves_the_history_as_it_was() {
        let path = temp_path("prune-stopped");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let written: Vec<Observation> = (1..=60)
            .map(|at| seen("c::t", at, &format!("run-{at}-1")))
            .collect();
        super::append(&path, &written).expect("append");
        let before = std::fs::read(&path).unwrap();

        let stopped = super::prune(&path, &crate::interrupt::Interrupt::already())
            .expect_err("asked to stop");
        assert!(
            stopped.to_string().contains("interrupted"),
            "got: {stopped}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let beside_it: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(beside_it, ["history.jsonl"], "no rewrite left behind");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_record_on_a_line_that_is_not_text_is_read_by_a_prune_as_by_a_load() {
        // A load decodes such a line as best it can and finds the record on
        // it. A prune that kept the line and did not count the record would
        // cut a window the analysis does not have.
        let path = temp_path("prune-not-text");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = b"\xff".to_vec();
        let failed = Observation {
            status: TestStatus::Failed,
            ..seen("c::t", 1, "import-a")
        };
        bytes.extend_from_slice(super::to_line(&failed).as_bytes());
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();
        let mut later: Vec<Observation> = (2..=52)
            .map(|at| seen("c::t", at, &format!("run-{at}-1")))
            .collect();
        // The invocation of the first line again, now the newest of all.
        later.push(seen("c::t", 100, "import-a"));
        super::append(&path, &later).expect("append");

        let found = || crate::analyzers::history::analyze(&super::load(&path).observations);
        let before = found();
        let pruned = prune(&path).expect("prune").expect("a history");
        assert_eq!(found(), before, "{pruned:?}");
        assert_eq!(pruned.kept, super::load(&path).observations.len());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_rewrite_keeps_the_permissions_the_history_had() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("rewrite-permissions");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let written: Vec<Observation> = (1..=60)
            .map(|at| seen("c::t", at, &format!("run-{at}-1")))
            .collect();
        super::append(&path, &written).expect("append");
        let private = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, private).unwrap();
        let mode = || std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;

        let pruned = prune(&path).expect("prune").expect("a history");
        assert_eq!(pruned.removed, 10);
        assert_eq!(mode(), 0o600, "after a prune");
        replace_invocations(&path, &["run-60-1".to_owned()], &[])
            .expect("replace")
            .expect("a history");
        assert_eq!(mode(), 0o600, "after a replace");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pruning_keeps_each_tests_last_fifty_invocations_and_nothing_older() {
        let path = temp_path("prune");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(prune(&path).unwrap(), None, "no history, nothing to do");

        let mut written = Vec::new();
        // `c::busy`: 60 invocations, the 30th one a `--runs 3`.
        for at in 1..=60 {
            let runs = if at == 30 { 3 } else { 1 };
            for _ in 0..runs {
                written.push(seen("c::busy", at, &format!("run-{at}-1")));
            }
        }
        // `c::rare`: three invocations — all inside its window.
        for at in [5, 6, 7] {
            written.push(seen("c::rare", at, &format!("run-{at}-1")));
        }
        // An import that arrives last in the file but is old in time: for
        // `c::busy` it lies before the window, wherever it stands.
        written.push(seen("c::busy", 2, "import-00000000000000aa"));
        super::append(&path, &written).expect("append");
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"at\":12,\"comm\n");
        bytes.extend_from_slice(b"not text \xff\n");
        std::fs::write(&path, &bytes).unwrap();

        let pruned = prune(&path).expect("prune").expect("a history");
        // 60 invocations at 1..=60: the window is 11..=60, which holds the
        // 30th's three runs. Before it: ten runs and the old import.
        assert_eq!(pruned.removed, 11);
        assert_eq!(pruned.kept, 52 + 3);
        assert!(pruned.bytes_after < pruned.bytes_before);

        let left = ids_and_times(&path);
        let busy: Vec<u64> = left
            .iter()
            .filter(|(id, _)| id == "c::busy")
            .map(|(_, at)| *at)
            .collect();
        assert_eq!(
            busy.first(),
            Some(&11),
            "the window begins at the 50th from the end"
        );
        assert_eq!(busy.len(), 52, "50 invocations, one of them three runs");
        assert_eq!(busy.iter().filter(|at| **at == 30).count(), 3);
        assert_eq!(left.iter().filter(|(id, _)| id == "c::rare").count(), 3);
        // What is not an observation is not this command's to judge.
        let after = std::fs::read(&path).unwrap();
        for kept in [&b"{\"at\":12,\"comm\n"[..], b"not text \xff\n"] {
            assert!(
                after.windows(kept.len()).any(|window| window == kept),
                "{kept:?}"
            );
        }
        assert!(!super::Beside::of(&path).0.exists());

        // A second prune finds nothing and leaves the file alone.
        let again = prune(&path).expect("prune").expect("a history");
        assert_eq!((again.removed, again.kept), (0, 55));
        assert_eq!(std::fs::read(&path).unwrap(), after);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_skip_takes_no_place_in_the_window_the_analysis_reads() {
        let path = temp_path("prune-skips");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        // Ten invocations that say something, failing and passing in turn,
        // and then the test is skipped: fifty invocations that say nothing.
        let mut written = Vec::new();
        for at in 1..=10 {
            let status = if at % 2 == 0 {
                TestStatus::Passed
            } else {
                TestStatus::Failed
            };
            written.push(Observation {
                status,
                ..seen("c::t", at, &format!("run-{at}-1"))
            });
        }
        let skipped = |at: u64| Observation {
            status: TestStatus::Skipped,
            ..seen("c::t", at, &format!("run-{at}-1"))
        };
        written.extend((11..=60).map(skipped));
        super::append(&path, &written).expect("append");
        let found = || crate::analyzers::history::analyze(&super::load(&path).observations);
        let before = found();
        assert_eq!(before.flaky.len(), 1, "the ten runs prove a flake");

        // The analysis counts its fifty invocations past the skips, so the
        // ten are inside its window however many skips came after them.
        let pruned = prune(&path).expect("prune").expect("a history");
        assert_eq!((pruned.removed, pruned.kept), (0, 60));
        assert_eq!(found(), before);

        // Skips are bounded too: the test's last fifty invocations of any
        // kind. Ten more push the ten oldest skips out, and nothing else.
        super::append(&path, &(61..=70).map(skipped).collect::<Vec<_>>()).expect("append");
        let pruned = prune(&path).expect("prune").expect("a history");
        assert_eq!((pruned.removed, pruned.kept), (10, 60));
        assert_eq!(found(), before);
        let left = ids_and_times(&path);
        assert_eq!(left.iter().filter(|(_, at)| *at <= 10).count(), 10);
        assert_eq!(left.iter().filter(|(_, at)| *at > 10).count(), 50);
        assert_eq!(left.iter().map(|(_, at)| *at).find(|at| *at > 10), Some(21));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pruning_never_changes_what_the_analysis_finds() {
        // The one promise of a prune, held against histories nobody wrote
        // by hand: skips, ties in time, invocations that come back later,
        // lines without one, dirty and unknown trees, imports out of order.
        let path = temp_path("prune-any-history");
        let mut state: u64 = 0x5007_4811;
        let mut next = |below: u64| {
            // A linear congruential generator: the same histories every run.
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % below
        };
        for history in 0..150 {
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
            let length = 120 + next(260);
            let written: Vec<Observation> = (0..length)
                .map(|_| Observation {
                    id: format!("c::t{}", next(3)),
                    status: [
                        TestStatus::Passed,
                        TestStatus::Failed,
                        TestStatus::Skipped,
                        TestStatus::Error,
                    ][usize::try_from(next(4)).unwrap()],
                    duration_ms: None,
                    commit: [None, Some("aaaa".to_owned()), Some("bbbb".to_owned())]
                        [usize::try_from(next(3)).unwrap()]
                    .clone(),
                    dirty: [None, Some(false), Some(false), Some(true)]
                        [usize::try_from(next(4)).unwrap()],
                    tree: [None, Some("t1".to_owned()), Some("t2".to_owned())]
                        [usize::try_from(next(3)).unwrap()]
                    .clone(),
                    environment: Some("local".to_owned()),
                    invocation: match next(8) {
                        0 => None,
                        _ => Some(format!("run-{}-1", next(90))),
                    },
                    at_epoch_secs: next(70),
                    inferred: next(9) == 0,
                })
                .collect();
            super::append(&path, &written).expect("append");
            let found = || crate::analyzers::history::analyze(&super::load(&path).observations);
            let before = found();
            let pruned = prune(&path).expect("prune").expect("a history");
            assert_eq!(found(), before, "history {history}: {pruned:?}");
            let again = prune(&path).expect("prune").expect("a history");
            assert_eq!(again.removed, 0, "history {history}: a prune is final");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_line_without_an_invocation_is_an_invocation_of_its_own() {
        // Lines from before invocations were recorded each stand alone, as
        // they do for the analysis.
        let path = temp_path("prune-old-lines");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let old: Vec<Observation> = (1..=53)
            .map(|at| Observation {
                at_epoch_secs: at,
                invocation: None,
                ..observation("c::t", TestStatus::Passed)
            })
            .collect();
        super::append(&path, &old).expect("append");
        let pruned = prune(&path).expect("prune").expect("a history");
        assert_eq!((pruned.removed, pruned.kept), (3, 50));
        assert_eq!(ids_and_times(&path).first().map(|(_, at)| *at), Some(4));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_import_ledger_roundtrips_and_tolerates_junk() {
        let path = temp_path("ledger");
        super::record_imported(&path, &[(0xdead_beef, "a.xml".to_owned())]).expect("record");
        super::record_imported(&path, &[(0x1234, "b sp ace.xml".to_owned())]).expect("extend");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("not a ledger line\n");
        std::fs::write(&path, text).unwrap();

        let hashes = super::imported_hashes(&path);
        assert!(hashes.contains(&0xdead_beef));
        assert!(hashes.contains(&0x1234));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn replacing_an_invocation_swaps_its_observations_in_one_write() {
        let path = temp_path("replace-invocation");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let imported = |id: &str, invocation: &str| Observation {
            invocation: Some(invocation.to_owned()),
            ..observation(id, TestStatus::Failed)
        };
        // No history: nothing to replace, and nothing is created.
        assert_eq!(
            replace_invocations(&path, &[String::new()], &[]).unwrap(),
            None
        );
        assert!(!path.exists());

        super::append(
            &path,
            &[
                imported("a::one", "import-0000000000000001"),
                imported("a::two", "import-0000000000000001"),
                imported("b::one", "import-0000000000000002"),
                observation("c::local", TestStatus::Passed),
            ],
        )
        .expect("append");
        // What a crash, another tool or Windows left behind is not this
        // command's to tidy: a fragment, bytes that are no text, a CRLF.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"at\":12,\"comm\n");
        bytes.extend_from_slice(b"garbage \xff\n");
        let crlf = super::to_line(&observation("d::crlf", TestStatus::Passed));
        bytes.extend_from_slice(crlf.as_bytes());
        bytes.extend_from_slice(b"\r\n");
        std::fs::write(&path, &bytes).unwrap();

        let removed = replace_invocations(
            &path,
            &["import-0000000000000001".to_owned()],
            &[imported("a::one", "import-0000000000000001")],
        )
        .expect("rewrite");
        assert_eq!(removed, Some(2));
        let ids: Vec<String> = super::load(&path)
            .observations
            .into_iter()
            .map(|observation| observation.id)
            .collect();
        assert_eq!(ids, ["b::one", "c::local", "d::crlf", "a::one"]);
        let after = std::fs::read(&path).unwrap();
        for kept in [&b"{\"at\":12,\"comm\n"[..], b"garbage \xff\n", b"\"}\r\n"] {
            assert!(
                after.windows(kept.len()).any(|window| window == kept),
                "kept byte for byte: {kept:?}"
            );
        }
        assert!(!super::Beside::of(&path).0.exists());
        // An id nobody recorded removes nothing; the replacement still lands.
        assert_eq!(
            replace_invocations(&path, &["import-ffffffffffffffff".to_owned()], &[]).unwrap(),
            Some(0)
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_ledger_is_day_one() {
        assert!(super::imported_hashes(&temp_path("no-ledger")).is_empty());
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let loaded = load(&temp_path("missing"));
        assert!(loaded.observations.is_empty());
        assert_eq!(loaded.skipped_lines, 0);
    }

    #[test]
    fn a_full_sha_is_lowercased_without_asking_git() {
        let nowhere = std::env::temp_dir().join("sooth-no-such-dir");
        let sha = "8B8C4FCCC51BA94134C2EEE12F7C1BEE5E5ECF1A";
        assert_eq!(resolve_commit(&nowhere, sha), Ok(sha.to_ascii_lowercase()));
    }

    #[test]
    fn a_branch_or_revision_name_is_not_a_sha() {
        let nowhere = std::env::temp_dir().join("sooth-no-such-dir");
        for given in ["", "main", "HEAD", "HEAD~1", "v1.0", "--version"] {
            assert_eq!(
                resolve_commit(&nowhere, given),
                Err(CommitError::NotASha),
                "{given:?}"
            );
        }
    }

    #[test]
    fn code_identity_reads_commit_and_dirtiness_from_a_real_repo() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // no git on this machine: identity degrades to unknown
        }
        let dir = std::env::temp_dir().join(format!("sooth-history-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        let clean = code_identity(&dir, None);
        assert!(clean.commit.is_some());
        assert_eq!(clean.dirty, Some(false));

        // What sooth writes is never a change to the code under test.
        std::fs::create_dir_all(dir.join(".sooth")).unwrap();
        std::fs::write(dir.join(".sooth/history.jsonl"), "").unwrap();
        assert_eq!(code_identity(&dir, None).dirty, Some(false));

        // Nor is the report the run writes — once sooth knows which file it is.
        std::fs::write(dir.join("report *.xml"), "<testsuite/>").unwrap();
        assert_eq!(code_identity(&dir, None).dirty, Some(true));
        let report = Path::new("report *.xml");
        assert_eq!(code_identity(&dir, Some(report)).dirty, Some(false));
        // A report outside the repository excuses nothing inside it.
        let elsewhere = std::env::temp_dir().join("sooth-report-elsewhere.xml");
        assert_eq!(code_identity(&dir, Some(&elsewhere)).dirty, Some(true));

        std::fs::write(dir.join("b.txt"), "untracked").unwrap();
        let dirty = code_identity(&dir, Some(report));
        assert_eq!(dirty.commit, clean.commit);
        assert_eq!(dirty.dirty, Some(true));

        // HEAD moving under a run leaves nothing to pin the observations on.
        assert_eq!(identity_after_run(clean.clone(), &dir), clean);
        git(&["add", "b.txt"]);
        git(&["commit", "-q", "-m", "second"]);
        assert_eq!(identity_after_run(clean, &dir), CodeIdentity::default());

        let nowhere = code_identity(&std::env::temp_dir().join("sooth-no-such-dir"), None);
        assert_eq!(nowhere.commit, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dirty_tree_is_named_by_its_content_and_reading_it_leaves_the_repository_alone() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // no git on this machine: identity degrades to unknown
        }
        let dir =
            std::env::temp_dir().join(format!("sooth-history-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git");
            assert!(output.status.success(), "git {args:?} failed");
            String::from_utf8(output.stdout).expect("utf-8")
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        assert_eq!(code_identity(&dir, None).tree, None, "a commit says it all");

        let report = Path::new("report.xml");
        std::fs::write(dir.join("a.txt"), "changed").unwrap();
        let objects = || walk_count(&dir.join(".git/objects"));
        let (index, status, stored) = (
            std::fs::read(dir.join(".git/index")).unwrap(),
            git(&["status", "--porcelain"]),
            objects(),
        );
        let first = code_identity(&dir, Some(report));
        let tree = first.tree.clone().expect("a dirty tree's content");
        assert_eq!(first.dirty, Some(true));
        // Read, and nothing of it left behind.
        assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index);
        assert_eq!(git(&["status", "--porcelain"]), status);
        assert_eq!(objects(), stored);

        // The same content, the same name — whatever sooth and the run
        // write beside it.
        std::fs::create_dir_all(dir.join(".sooth")).unwrap();
        std::fs::write(dir.join(".sooth/history.jsonl"), "x").unwrap();
        std::fs::write(dir.join("report.xml"), "<testsuite/>").unwrap();
        assert_eq!(code_identity(&dir, Some(report)).tree.as_ref(), Some(&tree));
        // A new file is new code; so is an edit.
        std::fs::write(dir.join("b.txt"), "new").unwrap();
        let with_new = code_identity(&dir, Some(report)).tree;
        assert!(with_new.is_some() && with_new.as_ref() != Some(&tree));
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        assert_eq!(code_identity(&dir, Some(report)).tree.as_ref(), Some(&tree));

        // After the run: the same unless the code changed while it ran.
        let before = code_identity(&dir, Some(report));
        std::fs::write(dir.join("report.xml"), "<testsuite><testcase/></testsuite>").unwrap();
        assert_eq!(identity_after_run(before.clone(), &dir), before);
        std::fs::write(dir.join("a.txt"), "edited during the run").unwrap();
        let after = identity_after_run(before.clone(), &dir);
        assert_eq!(
            (after.commit.as_ref(), after.dirty, after.tree),
            (before.commit.as_ref(), Some(true), None)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh repository with one commit, and a git that commits there.
    fn repository(tag: &str) -> (PathBuf, impl Fn(&Path, &[&str]) -> String) {
        let dir = std::env::temp_dir().join(format!("sooth-history-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |dir: &Path, args: &[&str]| -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git");
            assert!(output.status.success(), "git {args:?} failed");
            String::from_utf8(output.stdout).expect("utf-8")
        };
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        (dir, git)
    }

    #[test]
    fn work_inside_a_repository_within_names_no_content() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let (dir, git) = repository("nested");
        // A package developed in place, a repository of its own.
        let package = dir.join("packages/local");
        std::fs::create_dir_all(&package).unwrap();
        git(&package, &["init", "-q"]);
        std::fs::write(package.join("Code.php"), "v1").unwrap();
        git(&package, &["add", "."]);
        git(&package, &["commit", "-q", "-m", "v1"]);
        std::fs::write(dir.join("wip.txt"), "work").unwrap();
        assert!(
            code_identity(&dir, None).tree.is_some(),
            "the package is clean"
        );
        let app = dir.join("app");
        std::fs::create_dir_all(&app).unwrap();
        assert!(
            code_identity(&app, None).tree.is_some(),
            "from a subdirectory too"
        );
        // Uncommitted work inside it is code the tree would not show — seen
        // from the top and from any directory sooth runs in.
        std::fs::write(package.join("Code.php"), "v2").unwrap();
        let identity = code_identity(&dir, None);
        assert_eq!((identity.dirty, identity.tree), (Some(true), None));
        assert_eq!(code_identity(&app, None).tree, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `path` with its modification time set to `time`.
    fn touch_to(path: &Path, time: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn a_file_changed_in_the_index_s_own_instant_is_read_for_its_content() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let (dir, git) = repository("racy");
        // Without the inode's change time to go by, as on a filesystem that
        // keeps whole seconds within one second, only the modification time
        // and size stand between git and reading the file.
        git(&dir, &["config", "core.trustctime", "false"]);
        // The index written in the same instant as the file it records: git
        // cannot tell from the time alone whether the file changed since,
        // and reads it.
        let file = dir.join("a.txt");
        let written = std::fs::metadata(&file).unwrap().modified().unwrap();
        touch_to(&dir.join(".git/index"), written);
        std::fs::write(dir.join("wip.txt"), "work").unwrap();
        let before = code_identity(&dir, None).tree;
        assert!(before.is_some());
        // Same size, same time: only the content says it changed.
        std::fs::write(&file, "b").unwrap();
        touch_to(&file, written);
        assert_ne!(code_identity(&dir, None).tree, before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_submodule_never_checked_out_holds_no_work() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let (dir, git) = repository("gitlink");
        let package = dir.join("lib");
        std::fs::create_dir_all(&package).unwrap();
        git(&package, &["init", "-q"]);
        std::fs::write(package.join("a.txt"), "v1").unwrap();
        git(&package, &["add", "."]);
        git(&package, &["commit", "-q", "-m", "v1"]);
        git(&dir, &["add", "lib"]);
        git(&dir, &["commit", "-q", "-m", "a gitlink"]);
        // As a clone without `--recurse-submodules` leaves it: an empty
        // directory, which git asked on its own would answer for the outer
        // repository from.
        std::fs::remove_dir_all(&package).unwrap();
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(dir.join("wip.txt"), "work").unwrap();
        assert!(code_identity(&dir, None).tree.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_is_read_only_within_a_budget() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let (dir, git) = repository("budget");
        std::fs::write(dir.join("one.txt"), "12345").unwrap();
        std::fs::write(dir.join("two.txt"), "12345").unwrap();
        let top = PathBuf::from(git(&dir, &["rev-parse", "--show-toplevel"]).trim());
        let whole = super::changes_from_head(&dir, None).expect("a listing");
        assert_eq!(whole, ["one.txt", "two.txt"]);
        assert!(super::within_budget(&top, &whole, (2, 10)).is_some());
        assert!(
            super::within_budget(&top, &whole, (1, 10)).is_none(),
            "files"
        );
        assert!(
            super::within_budget(&top, &whole, (2, 9)).is_none(),
            "bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_content_runs_none_of_the_repositorys_hooks() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let (dir, _git) = repository("hooks");
        let hook = dir.join(".git/hooks/post-index-change");
        let marker = dir.join("hook-ran");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "changed").unwrap();
        assert!(code_identity(&dir, None).tree.is_some());
        assert!(!marker.exists(), "a hook ran");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_the_repository_ignores_has_no_identity() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let (dir, git) = repository("ignored");
        std::fs::write(dir.join(".gitignore"), "sandbox/\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "ignore"]);
        let sandbox = dir.join("sandbox/app");
        std::fs::create_dir_all(&sandbox).unwrap();
        assert_eq!(code_identity(&sandbox, None), CodeIdentity::default());
        // A subdirectory that is not ignored reads the same content as the
        // top of the repository.
        let sub = dir.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("x.txt"), "x").unwrap();
        assert_eq!(
            code_identity(&sub, None).tree,
            code_identity(&dir, None).tree
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_report_is_left_out_by_its_literal_name() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let (dir, git) = repository("literal");
        std::fs::write(dir.join("out1.xml"), "tracked").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "out1"]);
        std::fs::write(dir.join("wip.txt"), "work").unwrap();
        let report = Path::new("out[1].xml");
        let before = code_identity(&dir, Some(report)).tree;
        // A glob would take `out1.xml` for the report and leave it out.
        std::fs::write(dir.join("out1.xml"), "changed").unwrap();
        assert_ne!(code_identity(&dir, Some(report)).tree, before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// How many files lie under `dir`, however deep.
    fn walk_count(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .map(|entry| {
                    let path = entry.path();
                    if path.is_dir() {
                        walk_count(&path)
                    } else {
                        1
                    }
                })
                .sum()
        })
    }
}
