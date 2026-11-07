//! `fix`: the check, then the input written again without the rows that break a constraint.
//!
//! The output is always the original bytes minus a growing set of removed ranges, each replaced by a comment with
//! `annotate`, or with `--on-fk null` a row's foreign key values replaced by NULL, so everything else stays
//! byte-identical and the original bytes, kept in `rejects.sql`, restore the input exactly. Rounds repeat the check on
//! the input with the removed rows skipped and the changed values read as NULL: a removed parent can orphan its
//! children, which the next round removes.
//! Once a round removes nothing, the output is written and checked again, and that check is the verified result.
//! A data file whose last rows have no commit after them also gets one added at its end.
//!
//! Rows are found again by file and line: the fix re-reads each file it changes anyway, so findings carry no spans.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, Kind, Table};
use crate::check::{self, Report};
use crate::dialect::Pass;
use crate::finding::{Finding, FindingKind};
use crate::input::{self, InputError, InputFile, Role};
use crate::source::Span;
use crate::statement::{InsertRow, Statement};
use crate::value;

/// What happens to a row with NULL in a NOT NULL column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnRow {
    #[default]
    Drop,
    Keep,
}

/// What happens to a row with a missing parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnFk {
    #[default]
    Drop,
    /// Every column of the foreign key is set to NULL; the row is dropped when one of them can't be NULL.
    Null,
    Keep,
}

/// What happens to the later rows of a duplicate key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnDuplicate {
    /// The first row stays; the later ones go.
    #[default]
    KeepFirst,
    /// The last row in run order stays; the first and every other one go.
    KeepLast,
    Keep,
}

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub on_fk: OnFk,
    pub on_duplicate: OnDuplicate,
    pub on_not_null: OnRow,
    /// Act on findings whose key rests on an assumption, such as a date written as text, instead of leaving them for
    /// review.
    pub fix_assumed: bool,
    /// The most rounds that remove rows; what a further round would act on is left for review.
    pub max_rounds: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            on_fk: OnFk::default(),
            on_duplicate: OnDuplicate::default(),
            on_not_null: OnRow::default(),
            fix_assumed: false,
            max_rounds: 30,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FixError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error("cannot write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the output folder {} isn't empty; give a new or empty folder", .0.display())]
    OutNotEmpty(PathBuf),
    #[error("{} would be written where the report goes; give the report a folder of its own", .0.display())]
    ReportCollides(PathBuf),
    #[error("the report folder {} isn't empty; give a new or empty folder", .0.display())]
    ReportNotEmpty(PathBuf),
    #[error("{} already exists; move it away or give --backup another suffix", .0.display())]
    BackupExists(PathBuf),
    #[error("an in-place fix with no backups and no report would keep the removed rows nowhere")]
    NothingKept,
}

/// A removed row, or with `nulled` a row kept with its foreign key set to NULL: where it was in the input, what it
/// was, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// The input's name, as findings show it.
    pub file: String,
    pub line: u64,
    pub span: Span,
    /// Whether the cut is one row of a multi-row statement rather than a whole statement.
    pub clause: bool,
    pub reasons: Vec<Reason>,
    pub round: usize,
    pub text: Vec<u8>,
    /// What the output has in its place: nothing, or with `annotate` a comment; for a changed row, the row with
    /// NULL for each of `nulled`.
    pub replacement: Vec<u8>,
    /// For a changed row: each column set to NULL, with its value's place in the input.
    pub nulled: Vec<(String, Span)>,
}

/// Why a row went: the finding's kind, the finding as a line, and the finding without its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub kind: FindingKind,
    pub text: String,
    /// `missing parent for APP.FK_EMP_DEPT`.
    pub brief: String,
}

/// A finding the fix would act on but doesn't, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    pub finding: Finding,
    pub why: &'static str,
}

/// Where the fixed files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// Copies under a new or empty folder, at the inputs' paths relative to the folder they share.
    Out(PathBuf),
    /// The inputs themselves. With a `backup` suffix, each changed input is first kept beside itself as
    /// `<file><suffix>`.
    InPlace { backup: Option<String> },
}

/// Where `rejects.sql` and the fix reports go.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ReportPlace {
    /// `sqlca-report/` in the output folder; for an in-place fix, a new `sqlca-fix-<UTC time>/` in the working folder.
    #[default]
    Default,
    /// A new or empty folder.
    At(PathBuf),
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub destination: Destination,
    pub report: ReportPlace,
    /// Run the rounds and tell what would change, writing nothing.
    pub dry_run: bool,
    /// Leave a comment where each row or statement was removed.
    pub annotate: bool,
}

impl Target {
    /// Fixed copies under `dir`, with the report in it.
    pub fn out(dir: impl Into<PathBuf>) -> Self {
        Self {
            destination: Destination::Out(dir.into()),
            report: ReportPlace::Default,
            dry_run: false,
            annotate: false,
        }
    }
}

#[derive(Debug)]
pub struct FixReport {
    /// The check of the first round: what the input had.
    pub input: Report,
    /// The check of the written output: what it still has. For a dry run, the last round's, as it would be.
    pub output: Report,
    pub rounds: usize,
    pub removed: Vec<Removal>,
    /// The commits added at the end of files.
    pub committed: Vec<Commit>,
    /// Fixable findings the fix left in place: assumed values, rows it couldn't tell apart, and rows past the round
    /// limit.
    pub left: Vec<Left>,
    /// Where each input went, by input position.
    pub files: Vec<Output>,
    /// The report folder, unless there is none.
    pub report: Option<PathBuf>,
    pub target: Target,
}

/// One input's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The input's name, as findings show it.
    pub input: String,
    pub path: PathBuf,
    /// Whether the fix changed it.
    pub changed: bool,
    /// Where an in-place fix kept the original.
    pub backup: Option<PathBuf>,
    /// The SHA-256 of the file's bytes before and after, for a file the fix wrote.
    pub sha256: Option<Digests>,
}

/// A commit added at the end of a file whose last rows had none after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// The file's name, as findings show it.
    pub file: String,
    /// The input's end, where the text goes.
    pub at: u64,
    /// A line end when the file has none at its end, the annotation with `annotate`, and the commit.
    pub text: Vec<u8>,
    /// The finding it settles.
    pub finding: Finding,
}

/// Lowercase hex SHA-256 digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digests {
    pub before: String,
    pub after: String,
}

/// Fixes `files` to `target`. Nothing is written before every round is done and every place to write is known to be
/// free.
pub fn fix(
    files: &[InputFile],
    options: &check::Options,
    policy: Policy,
    target: &Target,
) -> Result<FixReport, FixError> {
    fix_with(files, options, policy, target, &mut check::Quiet)
}

/// `fix`, telling `observer` how it goes.
pub fn fix_with(
    files: &[InputFile],
    options: &check::Options,
    policy: Policy,
    target: &Target,
    observer: &mut dyn check::Observer,
) -> Result<FixReport, FixError> {
    let backup = match &target.destination {
        Destination::InPlace { backup: None } if target.report == ReportPlace::Off => {
            return Err(FixError::NothingKept);
        }
        Destination::InPlace { backup } => backup.as_deref(),
        Destination::Out(_) => None,
    };
    let originals: Vec<PathBuf> = files
        .iter()
        .map(|f| fs::canonicalize(&f.path).map_err(|source| write_error(&f.path, source)))
        .collect::<Result<_, _>>()?;
    let (paths, out) = match &target.destination {
        Destination::Out(out) => {
            let (paths, out) = destinations(&originals, out)?;
            (paths, Some(out))
        }
        Destination::InPlace { .. } => (originals.clone(), None),
    };
    let folder = report_folder(target, out.as_deref(), &paths)?;
    let names: Vec<String> = files.iter().map(|f| check::display(&f.path)).collect();

    let Plan {
        input,
        last,
        rounds,
        mut removed,
        mut left,
        defers_checks,
    } = run_rounds(files, options, policy, &names, target.dry_run, observer)?;
    let committed = if defers_checks {
        let uncommitted = input
            .findings
            .iter()
            .filter(|f| f.kind == FindingKind::Uncommitted);
        left.extend(uncommitted.map(|finding| Left {
            finding: finding.clone(),
            why: "a constraint is checked at COMMIT, so an added COMMIT could roll the load back",
        }));
        Vec::new()
    } else {
        commits(files, &names, &input.findings, options, target.annotate)?
    };

    let outputs: Vec<Output> = names
        .into_iter()
        .zip(&paths)
        .zip(&originals)
        .map(|((input, path), original)| {
            let changed = removed.iter().any(|r| r.file == input)
                || committed.iter().any(|c| c.file == input);
            Output {
                path: path.clone(),
                changed,
                backup: backup
                    .filter(|_| changed)
                    .map(|suffix| beside(original, suffix)),
                sha256: None,
                input,
            }
        })
        .collect();
    if let Some(taken) = outputs
        .iter()
        .filter_map(|o| o.backup.as_ref())
        .find(|b| fs::symlink_metadata(b).is_ok())
    {
        return Err(FixError::BackupExists(taken.clone()));
    }
    describe_removals(&mut removed, files, target.annotate)?;
    // Without a last round, the verify supplies it; the last round reads the input without the commits added.
    let mut output = last.unwrap_or_default();
    output.findings.retain(|f| {
        f.kind != FindingKind::Uncommitted
            || !committed.iter().any(|c| f.file.as_ref() == Some(&c.file))
    });
    let mut report = FixReport {
        input,
        output,
        rounds,
        removed,
        committed,
        left,
        files: Vec::new(),
        report: folder,
        target: target.clone(),
    };
    if target.dry_run {
        report.files = outputs;
        return Ok(report);
    }
    write(files, options, &originals, outputs, report, observer)
}

/// Writes the outputs and the report folder, and checks what was written. An in-place fix writes `rejects.sql` before
/// it changes an input.
fn write(
    files: &[InputFile],
    options: &check::Options,
    originals: &[PathBuf],
    mut outputs: Vec<Output>,
    mut report: FixReport,
    observer: &mut dyn check::Observer,
) -> Result<FixReport, FixError> {
    let target = report.target.clone();
    let rejects = |report: &FixReport| -> Result<(), FixError> {
        let Some(folder) = &report.report else {
            return Ok(());
        };
        new_folder(folder)?;
        write_file(&folder.join("rejects.sql"), None, |w| {
            write_rejects(&report.removed, w)
        })
    };
    match target.destination {
        // The copies first: on a file system that ignores case, a copied folder can take the report's name.
        Destination::Out(_) => {
            write_outputs(files, originals, &report, &mut outputs, observer)?;
            rejects(&report)?;
        }
        Destination::InPlace { .. } => {
            rejects(&report)?;
            write_outputs(files, originals, &report, &mut outputs, observer)?;
        }
    }
    let written: Vec<InputFile> = files
        .iter()
        .zip(&outputs)
        .map(|(f, o)| {
            let path = match target.destination {
                Destination::Out(_) => o.path.clone(),
                Destination::InPlace { .. } => f.path.clone(),
            };
            // The verify's progress counts the bytes as written; a file that can't be read fails the check itself.
            let bytes = fs::metadata(&path).map_or(f.bytes, |m| m.len());
            InputFile {
                path,
                bytes,
                ..f.clone()
            }
        })
        .collect();
    observer.stage("verify");
    report.output = check::run_with(&written, options, observer)?;
    report.files = outputs;
    if let Some(folder) = &report.report {
        write_file(&folder.join("fix-report.txt"), None, |w| {
            write_report(&report, w)
        })?;
        write_file(&folder.join("fix-report.json"), None, |w| {
            write_json_report(&report, w)
        })?;
    }
    Ok(report)
}

/// What the rounds found and decided; nothing is written.
struct Plan {
    /// The first round's check.
    input: Report,
    /// The check of the input as the output will be, unless the rounds ended once nothing more could be found.
    last: Option<Report>,
    rounds: usize,
    removed: Vec<Removal>,
    left: Vec<Left>,
    /// Whether an enabled constraint is checked only at commit.
    defers_checks: bool,
}

/// Checks the input, removes what the findings call for, and checks again without it, until a round removes nothing.
fn run_rounds(
    files: &[InputFile],
    options: &check::Options,
    policy: Policy,
    names: &[String],
    confirm: bool,
    observer: &mut dyn check::Observer,
) -> Result<Plan, FixError> {
    let mut cuts: Vec<Vec<Span>> = vec![Vec::new(); files.len()];
    let mut removed = Vec::new();
    let mut first_report = None;
    let mut rounds = 0;
    observer.stage("check 1");
    let schema = check::schema(files, options, observer)?;
    let (last, left) = loop {
        if rounds > 0 {
            observer.stage(&format!("check {}", rounds + 1));
        }
        let nulls = nulled_values(&removed, names);
        let report = check::run_without(files, options, &schema, &cuts, &nulls, observer)?;
        let mut scans = BTreeMap::new();
        for (index, lines) in named_rows(&report.findings, names, policy) {
            let (Some(file), Some(file_cuts)) = (files.get(index), cuts.get(index)) else {
                continue;
            };
            if file.role != Role::Reference {
                scans.insert(index, scan(file, options, &lines, file_cuts)?);
            }
        }
        let crowded: BTreeSet<Row> = scans
            .iter()
            .flat_map(|(index, scan)| scan.crowded.iter().map(move |line| (*index, *line)))
            .collect();
        let Decision {
            targets,
            left: skipped,
            waiting,
        } = targets(&report.findings, files, names, policy, &crowded);
        if rounds == policy.max_rounds && !targets.is_empty() {
            let left = past_limit(&report, policy, skipped);
            break (Some(report), left);
        }
        // A value set to NULL can make a unique key a duplicate, or take a key children point at.
        let mut settled = !waiting
            && !can_orphan(&targets, names, &report)
            && !takes_a_first_row(&targets, names, &report);
        let mut changed = false;
        for (index, lines) in &targets {
            let (Some(file), Some(file_cuts), Some(scanned)) =
                (files.get(*index), cuts.get_mut(*index), scans.get(index))
            else {
                continue;
            };
            for cut in locate(scanned, *index, lines, schema.catalog(), policy) {
                let name = names.get(*index).cloned().unwrap_or_default();
                if !cut.nulled.is_empty() {
                    if change(&mut removed, name, cut, rounds + 1) {
                        changed = true;
                        settled = false;
                    }
                    continue;
                }
                changed = true;
                let span = extend_to_line_end(&file.path, cut.span)?;
                insert_cut(file_cuts, span);
                // A statement going whole takes the rows an earlier round cut from it, reasons and all.
                let mut reasons = Vec::new();
                removed.retain(|r: &Removal| {
                    let inside =
                        r.file == name && span.start <= r.span.start && r.span.end <= span.end;
                    if inside {
                        reasons.extend(r.reasons.iter().cloned());
                    }
                    !inside
                });
                reasons.extend(cut.reasons);
                removed.push(Removal {
                    file: name,
                    line: 0,
                    span,
                    clause: cut.clause,
                    reasons,
                    round: rounds + 1,
                    text: Vec::new(),
                    replacement: Vec::new(),
                    nulled: Vec::new(),
                });
            }
        }
        if !changed {
            break (Some(report), skipped);
        }
        if first_report.is_none() {
            first_report = Some(report);
        }
        rounds += 1;
        // Another round would find nothing new; the verify checks the output instead.
        if settled && !confirm {
            break (None, skipped);
        }
    };
    Ok(Plan {
        input: first_report.or_else(|| last.clone()).unwrap_or_default(),
        last,
        rounds,
        removed,
        left,
        defers_checks: schema.catalog().defers_checks(),
    })
}

/// Records a row kept with values set to NULL, and whether that sets any value the changes so far didn't; a row
/// changed in an earlier round, for another foreign key, takes the new values into the same change.
fn change(removed: &mut Vec<Removal>, file: String, cut: Cut, round: usize) -> bool {
    if let Some(earlier) = removed
        .iter_mut()
        .find(|r| r.file == file && r.span == cut.span)
    {
        let before = earlier.nulled.len();
        earlier.nulled.extend(cut.nulled);
        earlier.nulled.sort_by_key(|(_, span)| *span);
        earlier.nulled.dedup_by_key(|(_, span)| *span);
        if earlier.nulled.len() == before {
            return false;
        }
        earlier.reasons.extend(cut.reasons);
        return true;
    }
    removed.push(Removal {
        file,
        line: 0,
        span: cut.span,
        clause: cut.clause,
        reasons: cut.reasons,
        round,
        text: Vec::new(),
        replacement: Vec::new(),
        nulled: cut.nulled,
    });
    true
}

/// By file position, the sorted places of the values the changes so far set to NULL.
fn nulled_values(removed: &[Removal], names: &[String]) -> Vec<Vec<Span>> {
    names
        .iter()
        .map(|name| {
            let mut spans: Vec<Span> = removed
                .iter()
                .filter(|r| r.file == *name)
                .flat_map(|r| r.nulled.iter().map(|(_, span)| *span))
                .collect();
            spans.sort();
            spans
        })
        .collect()
}

/// A commit at the end of each data file whose last rows the input check found with none after them.
fn commits(
    files: &[InputFile],
    names: &[String],
    findings: &[Finding],
    options: &check::Options,
    annotate: bool,
) -> Result<Vec<Commit>, FixError> {
    let mut commits = Vec::new();
    for finding in findings
        .iter()
        .filter(|f| f.kind == FindingKind::Uncommitted)
    {
        let Some(file) = names
            .iter()
            .position(|n| finding.file.as_ref() == Some(n))
            .and_then(|i| files.get(i))
        else {
            continue;
        };
        let (at, text) = commit_text(&file.path, options.dialect.commit_statement(), annotate)
            .map_err(|source| write_error(&file.path, source))?;
        commits.push(Commit {
            file: check::display(&file.path),
            at,
            text,
            finding: finding.clone(),
        });
    }
    Ok(commits)
}

/// The end of the file at `path`, and what goes there to commit its last rows, each line ended as the file's last
/// line is.
fn commit_text(path: &Path, commit: &str, annotate: bool) -> io::Result<(u64, Vec<u8>)> {
    const TAIL: u64 = 4096;
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL)))?;
    let mut tail = Vec::new();
    file.take(TAIL).read_to_end(&mut tail)?;
    let crlf = tail
        .iter()
        .rposition(|&b| b == b'\n')
        .and_then(|i| i.checked_sub(1))
        .is_some_and(|i| tail.get(i) == Some(&b'\r'));
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    let mut text = Vec::new();
    if !tail.is_empty() && !tail.ends_with(b"\n") {
        text.extend_from_slice(eol);
    }
    if annotate {
        text.extend_from_slice(
            b"-- sqlca fix added this COMMIT: no COMMIT followed the rows before it",
        );
        text.extend_from_slice(eol);
    }
    text.extend_from_slice(commit.as_bytes());
    text.extend_from_slice(eol);
    Ok((size, text))
}

/// Writes each output that differs from its input. Every backup is made before any input is rewritten, so a backup
/// that fails leaves all inputs as they were.
fn write_outputs(
    files: &[InputFile],
    originals: &[PathBuf],
    report: &FixReport,
    outputs: &mut [Output],
    observer: &mut dyn check::Observer,
) -> Result<(), FixError> {
    for (original, output) in originals.iter().zip(outputs.iter()) {
        if let Some(backup) = &output.backup {
            keep_backup(original, backup)?;
        }
    }
    observer.stage("write");
    for (index, (original, output)) in originals.iter().zip(outputs).enumerate() {
        if !output.changed && output.path == *original {
            continue;
        }
        let bytes = files.get(index).map_or(0, |f| f.bytes);
        observer.file(index, &output.input, bytes);
        let removals = report.removed.iter().filter(|r| r.file == output.input);
        let commits = report.committed.iter().filter(|c| c.file == output.input);
        let edits: Vec<Edit> = removals
            .map(|r| Edit {
                span: r.span,
                with: &r.replacement,
            })
            .chain(commits.map(|c| Edit {
                span: Span {
                    start: c.at,
                    end: c.at,
                },
                with: &c.text,
            }))
            .collect();
        output.sha256 = splice(original, &edits, &output.path)?;
    }
    Ok(())
}

/// `path` with `suffix` added to its file name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(suffix);
    path.with_file_name(name)
}

/// Keeps `original` as `backup`, a name nothing has: a hard link where the file system allows, else a copy.
fn keep_backup(original: &Path, backup: &Path) -> Result<(), FixError> {
    match fs::hard_link(original, backup) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(FixError::BackupExists(backup.to_owned()))
        }
        Err(_) => {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(backup)
                .and_then(|_| fs::copy(original, backup))
                .map_err(|source| write_error(backup, source))?;
            Ok(())
        }
    }
}

fn write_error(path: &Path, source: io::Error) -> FixError {
    FixError::Write {
        path: path.to_owned(),
        source,
    }
}

/// The folder in the output that holds `rejects.sql` and the fix reports.
pub const REPORT_FOLDER: &str = "sqlca-report";

/// Each input's path under `out`, relative to the folder all inputs share, and `out` as they are under. `out` must be
/// new or empty, so every folder in it is one the fix made: nothing in it can lead back to an input.
fn destinations(originals: &[PathBuf], out: &Path) -> Result<(Vec<PathBuf>, PathBuf), FixError> {
    let out = resolved(out)?;
    require_empty(&out, FixError::OutNotEmpty)?;
    let folders: Vec<PathBuf> = originals
        .iter()
        .map(|p| p.parent().map(Path::to_path_buf).unwrap_or_default())
        .collect();
    let common = check::shared_folder(&folders);
    let paths = originals
        .iter()
        .map(|p| out.join(p.strip_prefix(&common).unwrap_or(p)))
        .collect();
    Ok((paths, out))
}

/// `path` absolute, with the links of the part that exists resolved and the rest applied to it, `..` included: a
/// folder that doesn't exist yet is no link, so this is the path it will have once made.
fn resolved(path: &Path) -> Result<PathBuf, FixError> {
    let absolute = std::path::absolute(path).map_err(|source| write_error(path, source))?;
    let mut missing = Vec::new();
    let mut existing = absolute.as_path();
    let mut found = loop {
        if let Ok(found) = fs::canonicalize(existing) {
            break found;
        }
        match (existing.components().next_back(), existing.parent()) {
            (Some(last), Some(parent)) => {
                missing.push(last);
                existing = parent;
            }
            _ => return Ok(absolute),
        }
    };
    for component in missing.into_iter().rev() {
        match component {
            Component::ParentDir => {
                found.pop();
            }
            Component::Normal(name) => found.push(name),
            _ => {}
        }
    }
    Ok(found)
}

/// Refuses a folder that exists with something in it, or that can't be listed to tell.
fn require_empty(folder: &Path, refusal: fn(PathBuf) -> FixError) -> Result<(), FixError> {
    match fs::read_dir(folder) {
        Ok(mut entries) => match entries.next() {
            Some(_) => Err(refusal(folder.to_owned())),
            None => Ok(()),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(write_error(folder, source)),
    }
}

/// Makes the report folder, refusing it if something got there since the run began.
fn new_folder(folder: &Path) -> Result<(), FixError> {
    fs::create_dir_all(folder).map_err(|source| write_error(folder, source))?;
    require_empty(folder, FixError::ReportNotEmpty)
}

/// Where the report goes, if anywhere: refused when it holds something, or when an output would be written into it.
fn report_folder(
    target: &Target,
    out: Option<&Path>,
    outputs: &[PathBuf],
) -> Result<Option<PathBuf>, FixError> {
    let folder = match (&target.report, out) {
        (ReportPlace::Off, _) => return Ok(None),
        (ReportPlace::At(folder), _) => resolved(folder)?,
        (ReportPlace::Default, Some(out)) => out.join(REPORT_FOLDER),
        (ReportPlace::Default, None) => resolved(Path::new(&format!(
            "sqlca-fix-{}",
            utc_stamp(SystemTime::now())
        )))?,
    };
    require_empty(&folder, FixError::ReportNotEmpty)?;
    if let Some(clash) = outputs.iter().find(|p| p.starts_with(&folder)) {
        return Err(FixError::ReportCollides(clash.clone()));
    }
    Ok(Some(folder))
}

/// `20261106T143005Z`: a time as a name that sorts.
fn utc_stamp(at: SystemTime) -> String {
    let seconds = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (year, month, day) = value::civil_from_days(i64::try_from(seconds / 86_400).unwrap_or(0));
    let time = seconds % 86_400;
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        time / 3600,
        time % 3600 / 60,
        time % 60
    )
}

/// The findings a round acts on, by file position and line, and the fixable ones it leaves for review.
type Targets<'a> = BTreeMap<usize, BTreeMap<u64, Vec<&'a Finding>>>;

/// A row by file position and line.
type Row = (usize, u64);

const ASSUMED: &str = "its key rests on a date written as text";
const CROWDED: &str = "the line holds several rows, so the fix can't tell which";

const PAST_LIMIT: &str = "the fix reached its round limit (--max-rounds) before this row's round";

/// Once the rounds reach their limit: what the fix leaves, which is every finding it would act on.
fn past_limit(report: &check::Report, policy: Policy, mut left: Vec<Left>) -> Vec<Left> {
    let reviewed: BTreeSet<&Finding> = left.iter().map(|l| &l.finding).collect();
    let more: Vec<Left> = report
        .findings
        .iter()
        .filter(|f| acted_on(f.kind, policy) && f.line.is_some() && !reviewed.contains(f))
        .map(|finding| Left {
            finding: finding.clone(),
            why: PAST_LIMIT,
        })
        .collect();
    left.extend(more);
    left
}

/// Whether `policy` acts on findings of `kind`.
fn acted_on(kind: FindingKind, policy: Policy) -> bool {
    match kind {
        FindingKind::FkMissingParent => policy.on_fk != OnFk::Keep,
        FindingKind::DuplicateKey => policy.on_duplicate != OnDuplicate::Keep,
        FindingKind::NotNull => policy.on_not_null == OnRow::Drop,
        _ => false,
    }
}

/// By file position, the lines of the rows the findings `policy` acts on name: their own, and a duplicate's first row.
fn named_rows(
    findings: &[Finding],
    names: &[String],
    policy: Policy,
) -> BTreeMap<usize, BTreeSet<u64>> {
    let mut rows: BTreeMap<usize, BTreeSet<u64>> = BTreeMap::new();
    for finding in findings.iter().filter(|f| acted_on(f.kind, policy)) {
        let own = finding.file.as_deref().zip(finding.line);
        let first = finding.first.as_ref().map(|l| (l.file.as_str(), l.line));
        for (file, line) in own.into_iter().chain(first) {
            if let Some(index) = names.iter().position(|n| n == file) {
                rows.entry(index).or_default().insert(line);
            }
        }
    }
    rows
}

/// What one round removes, and what it leaves.
struct Decision<'a> {
    targets: Targets<'a>,
    /// Findings the fix leaves for review, with why.
    left: Vec<Left>,
    /// Some rows wait for a later round to show which row of their key to keep.
    waiting: bool,
}

/// Whether the rows going can leave another row without its parent: a row goes from a table a foreign key points
/// at, other than as a duplicate of each key pointed at, which a kept row still holds; and it holds a key some
/// child row found, or its table has keys the check couldn't know or assumed.
/// Whether a row going is the first row of a duplicated key. The key's next row then comes first, with duplicates no
/// finding named, such as rows written before the constraint was enabled.
fn takes_a_first_row(targets: &Targets, names: &[String], report: &check::Report) -> bool {
    report
        .findings
        .iter()
        .filter(|f| f.kind == FindingKind::DuplicateKey)
        .filter_map(|f| f.first.as_ref())
        .any(|first| {
            names
                .iter()
                .position(|n| *n == first.file)
                .and_then(|file| targets.get(&file))
                .is_some_and(|lines| lines.contains_key(&first.line))
        })
}

fn can_orphan(targets: &Targets, names: &[String], report: &check::Report) -> bool {
    targets.iter().any(|(&file, lines)| {
        lines.iter().any(|(&line, findings)| {
            let Some(table) = findings.first().map(|f| &f.table) else {
                return false;
            };
            let pointed_at = report
                .referenced
                .iter()
                .filter(|(parent, _)| parent == table)
                .any(|(_, columns)| {
                    !findings
                        .iter()
                        .any(|f| f.kind == FindingKind::DuplicateKey && f.columns == *columns)
                });
            let found = report.uncertain_tables.contains(table)
                || names
                    .get(file)
                    .is_some_and(|name| report.referenced_rows.contains(&(name.clone(), line)));
            pointed_at && found
        })
    })
}

fn targets<'a>(
    findings: &'a [Finding],
    files: &[InputFile],
    names: &[String],
    policy: Policy,
    crowded: &BTreeSet<Row>,
) -> Decision<'a> {
    let row_of = |file: &str, line: u64| Some((names.iter().position(|n| n == file)?, line));
    let keep_last = policy.on_duplicate == OnDuplicate::KeepLast;
    let mut going: Vec<(Row, &Finding)> = Vec::new();
    let mut groups: BTreeMap<_, Vec<&Finding>> = BTreeMap::new();
    let mut left = Vec::new();
    for finding in findings.iter().filter(|f| acted_on(f.kind, policy)) {
        if keep_last && finding.kind == FindingKind::DuplicateKey {
            // The columns tell unnamed constraints apart; no two keys of a table have the same columns.
            let constraint = (&finding.table, &finding.constraint, &finding.columns);
            groups
                .entry((constraint, &finding.key))
                .or_default()
                .push(finding);
            continue;
        }
        if finding.assumed && !policy.fix_assumed {
            left.push(Left {
                finding: finding.clone(),
                why: ASSUMED,
            });
            continue;
        }
        let Some(row) = finding
            .file
            .as_deref()
            .zip(finding.line)
            .and_then(|(file, line)| row_of(file, line))
        else {
            continue;
        };
        if crowded.contains(&row) {
            left.push(Left {
                finding: finding.clone(),
                why: CROWDED,
            });
            continue;
        }
        going.push((row, finding));
    }
    let mut lasts = Vec::new();
    for group in groups.into_values() {
        match all_but_last(&group, files, &row_of, policy, crowded) {
            Ok(last) => lasts.push(last),
            Err(why) => left.extend(group.iter().map(|f| Left {
                finding: (*f).clone(),
                why: if f.assumed && !policy.fix_assumed {
                    ASSUMED
                } else {
                    why
                },
            })),
        }
    }
    let acting = going.len() + lasts.len();
    // Duplicates wait while anything else goes: the row they would keep may go itself, or lose its parent in a later
    // round, and only the rounds after show which row is left to keep.
    if going
        .iter()
        .any(|(_, f)| f.kind != FindingKind::DuplicateKey)
    {
        going.retain(|(_, f)| f.kind != FindingKind::DuplicateKey);
        lasts.clear();
    }
    let doomed: BTreeSet<Row> = going
        .iter()
        .map(|(row, _)| *row)
        .chain(
            lasts
                .iter()
                .flat_map(|(_, rows)| rows.iter().map(|(row, _)| *row)),
        )
        .collect();
    // A row can be in several keys. A duplicate whose first row goes this round, as a duplicate of another key, waits:
    // once the first row is gone, it may be the one to keep.
    going.retain(|(_, f)| {
        let first = f.first.as_ref().and_then(|l| row_of(&l.file, l.line));
        f.kind != FindingKind::DuplicateKey || first.is_none_or(|first| !doomed.contains(&first))
    });
    let mut taken = going.len();
    // Likewise a key whose last row goes this round: once the last row is gone, the one before it is the one to keep.
    for (last, rows) in lasts {
        if !doomed.contains(&last) {
            taken += 1;
            going.extend(rows);
        }
    }
    let waiting = taken < acting;
    let mut targets: Targets = BTreeMap::new();
    for ((index, line), finding) in going {
        targets
            .entry(index)
            .or_default()
            .entry(line)
            .or_default()
            .push(finding);
    }
    Decision {
        targets,
        left,
        waiting,
    }
}

/// The rows of one duplicate key that keep-last removes, each with a finding on its key, after the last row in run
/// order; or why the key is left whole.
#[expect(
    clippy::type_complexity,
    reason = "the last row, then each other row with its finding"
)]
fn all_but_last<'a>(
    group: &[&'a Finding],
    files: &[InputFile],
    row_of: &dyn Fn(&str, u64) -> Option<Row>,
    policy: Policy,
    crowded: &BTreeSet<Row>,
) -> Result<(Row, Vec<(Row, &'a Finding)>), &'static str> {
    if !policy.fix_assumed && group.iter().any(|f| f.assumed) {
        return Err("another row of its key rests on a date written as text");
    }
    let mut rows: Vec<(Row, &Finding)> = Vec::new();
    for finding in group {
        let first = finding.first.as_ref().and_then(|l| row_of(&l.file, l.line));
        let own = finding
            .file
            .as_deref()
            .zip(finding.line)
            .and_then(|(file, line)| row_of(file, line));
        let (Some(first), Some(own)) = (first, own) else {
            return Err("the first row of its key is in no input");
        };
        if files.get(first.0).is_none_or(|f| f.role == Role::Reference) {
            return Err(
                "the first row of its key is in a reference file, which the fix doesn't change",
            );
        }
        if !rows.iter().any(|(row, _)| *row == first) {
            rows.push((first, finding));
        }
        rows.push((own, finding));
    }
    rows.sort_by_key(|(row, _)| *row);
    let shared = rows.windows(2).any(|pair| match pair {
        [(a, _), (b, _)] => a == b,
        _ => false,
    });
    if shared || rows.iter().any(|(row, _)| crowded.contains(row)) {
        return Err(
            "a row of its key shares its line with another row, so the fix can't tell which",
        );
    }
    let Some((last, _)) = rows.pop() else {
        return Err("the first row of its key is in no input");
    };
    Ok((last, rows))
}

/// A cut in a file's current output, or with `nulled` a row kept with those values set to NULL; with why.
struct Cut {
    span: Span,
    clause: bool,
    reasons: Vec<Reason>,
    /// Each column set to NULL, with its value's place; empty for a cut.
    nulled: Vec<(String, Span)>,
}

/// The statements of a file that hold one of the lines asked for, each with its rows left after the earlier cuts, and
/// which of those lines hold several rows.
struct Scan {
    statements: Vec<(Span, Vec<InsertRow<'static>>)>,
    crowded: BTreeSet<u64>,
}

fn scan(
    file: &InputFile,
    options: &check::Options,
    lines: &BTreeSet<u64>,
    cuts: &[Span],
) -> Result<Scan, FixError> {
    let opened = input::open(&file.path)?;
    let mut rows_per_line: BTreeMap<u64, usize> = BTreeMap::new();
    let mut statements = Vec::new();
    for piece in options
        .dialect
        .pieces(opened.reader, options.runner, opened.base_offset)
    {
        let Ok(piece) = piece else {
            break;
        };
        let last_line = piece.line + newlines(&piece.text);
        if lines.range(piece.line..=last_line).next().is_none() {
            continue;
        }
        let Ok(Some(Statement::Insert(mut rows))) =
            options.dialect.parse(&piece, options.encoding, Pass::Rows)
        else {
            continue;
        };
        rows.retain(|r| !check::is_removed(cuts, r.span));
        for row in rows.iter().filter(|r| lines.contains(&r.line)) {
            *rows_per_line.entry(row.line).or_default() += 1;
        }
        if rows.iter().any(|r| lines.contains(&r.line)) {
            let rows = rows.into_iter().map(InsertRow::into_owned).collect();
            statements.push((piece.span, rows));
        }
    }
    let crowded = rows_per_line
        .into_iter()
        .filter(|(_, rows)| *rows > 1)
        .map(|(line, _)| line)
        .collect();
    Ok(Scan {
        statements,
        crowded,
    })
}

/// What happens to the target rows of a scanned file: a row's clause goes, or its whole statement when every row of
/// the statement left after the earlier cuts goes, or a row stays with its foreign key set to NULL.
fn locate(
    scan: &Scan,
    file: usize,
    lines: &BTreeMap<u64, Vec<&Finding>>,
    catalog: &Catalog,
    policy: Policy,
) -> Vec<Cut> {
    let mut cuts = Vec::new();
    for (statement, rows) in &scan.statements {
        let actions: Vec<Cut> = rows
            .iter()
            .filter_map(|row| {
                let findings = lines.get(&row.line)?;
                Some(action(row, (file, *statement), findings, catalog, policy))
            })
            .collect();
        let going = actions.iter().filter(|a| a.nulled.is_empty()).count();
        if going > 0 && going == rows.len() {
            cuts.push(Cut {
                span: *statement,
                clause: false,
                reasons: actions.into_iter().flat_map(|a| a.reasons).collect(),
                nulled: Vec::new(),
            });
        } else {
            cuts.extend(actions);
        }
    }
    cuts
}

/// What happens to one target row. With `--on-fk null` and only missing parents against it, it stays with every
/// column of those foreign keys set to NULL, unless one can't hold NULL; then it goes, and its reasons say why.
fn action(
    row: &InsertRow,
    (file, statement): (usize, Span),
    findings: &[&Finding],
    catalog: &Catalog,
    policy: Policy,
) -> Cut {
    let mut reasons: Vec<Reason> = findings.iter().map(|f| reason(f)).collect();
    let null_wanted = policy.on_fk == OnFk::Null
        && findings
            .iter()
            .all(|f| f.kind == FindingKind::FkMissingParent);
    let mut nulled = Vec::new();
    if null_wanted {
        match nullable_values(row, file, findings, catalog) {
            Ok(values) => nulled = values,
            Err(why) => {
                for reason in &mut reasons {
                    reason.text.push_str("; the row went, since ");
                    reason.text.push_str(&why);
                }
            }
        }
    }
    Cut {
        span: row.span,
        clause: row.span != statement,
        reasons,
        nulled,
    }
}

/// The values of the findings' foreign key columns in `row`, a row of the file at `file` in the run, each with its
/// column, when every one of them can be set to NULL; else why not.
fn nullable_values(
    row: &InsertRow,
    file: usize,
    findings: &[&Finding],
    catalog: &Catalog,
) -> Result<Vec<(String, Span)>, String> {
    let resolved = check::resolve_row(catalog, row, file)
        .map_err(|_| "its table can't be matched".to_owned())?;
    let table = catalog
        .table(resolved.table)
        .ok_or_else(|| "its table can't be matched".to_owned())?;
    let mut nulled: Vec<(String, Span)> = Vec::new();
    for column in findings.iter().flat_map(|f| &f.columns) {
        let position = table
            .column(column)
            .ok_or_else(|| format!("{column} isn't a column of its table"))?;
        if let Some(why) = not_nullable(table, position, file, row.span.start) {
            return Err(format!("{column} {why}"));
        }
        let value = resolved
            .values
            .get(position)
            .copied()
            .flatten()
            .ok_or_else(|| format!("the row leaves {column} out, so its default applies"))?;
        if !nulled.iter().any(|(c, _)| c == column) {
            nulled.push((column.clone(), value.span));
        }
    }
    nulled.sort_by_key(|(_, span)| *span);
    Ok(nulled)
}

/// Why the column at `position` can't hold NULL in a row at `offset` in the file at `file`, if it can't: the same
/// constraints the check judges the row by.
fn not_nullable(table: &Table, position: usize, file: usize, offset: u64) -> Option<&'static str> {
    let column = table.columns.get(position)?;
    if column.identity {
        return Some("is an identity column");
    }
    if column.default_on_null {
        return Some("takes its default in place of NULL");
    }
    table
        .forbids_null(position, file, offset)
        .map(|c| match c.kind {
            Kind::PrimaryKey { .. } => "is part of the primary key",
            _ => "is NOT NULL",
        })
}

/// A finding as a removal or change gives it, without places: the removal says where the row was.
fn reason(finding: &Finding) -> Reason {
    Reason {
        kind: finding.kind,
        text: crate::report::describe(&Finding {
            file: None,
            line: None,
            first: None,
            ..finding.clone()
        }),
        brief: brief(finding),
    }
}

/// A finding as an annotation names it: what is wrong and the constraint, without the row's values.
fn brief(finding: &Finding) -> String {
    let table = &finding.table;
    match (finding.kind, &finding.constraint) {
        (FindingKind::FkMissingParent, Some(constraint)) => {
            format!("missing parent for {constraint}")
        }
        (FindingKind::FkMissingParent, None) => {
            format!("missing parent for a foreign key of {table}")
        }
        (FindingKind::DuplicateKey, Some(constraint)) => format!("duplicate key of {constraint}"),
        (FindingKind::DuplicateKey, None) => format!("duplicate key in {table}"),
        (FindingKind::NotNull, _) => format!("NULL in {table}.{}", finding.columns.join(", ")),
        (kind, _) => crate::report::kind_name(kind).to_owned(),
    }
}

/// Adds a cut to the sorted, non-overlapping set, merging any it touches.
fn insert_cut(cuts: &mut Vec<Span>, mut span: Span) {
    cuts.retain(|c| {
        let touches = c.start <= span.end && span.start <= c.end;
        if touches {
            span.start = span.start.min(c.start);
            span.end = span.end.max(c.end);
        }
        !touches
    });
    let at = cuts.partition_point(|c| c.start < span.start);
    cuts.insert(at, span);
}

/// The cut, widened to whole lines when it is the only thing on them: through the line end when only blanks follow it,
/// and then back over the blanks before it, so no empty or doubly indented line is left behind.
fn extend_to_line_end(path: &Path, span: Span) -> Result<Span, FixError> {
    const NEAR: u64 = 256;
    let read_error = |source| write_error(path, source);
    let mut file = File::open(path).map_err(read_error)?;
    let from = span.start.saturating_sub(NEAR);
    file.seek(SeekFrom::Start(from)).map_err(read_error)?;
    let mut before = Vec::new();
    (&mut file)
        .take(span.start - from)
        .read_to_end(&mut before)
        .map_err(read_error)?;
    file.seek(SeekFrom::Start(span.end)).map_err(read_error)?;
    let mut after = Vec::new();
    file.take(NEAR)
        .read_to_end(&mut after)
        .map_err(read_error)?;
    let blank = |b: &&u8| **b == b' ' || **b == b'\t';
    let trailing = after.iter().take_while(blank).count();
    let rest = after.get(trailing..).unwrap_or_default();
    let eol = if rest.starts_with(b"\r\n") {
        2
    } else if rest.starts_with(b"\n") {
        1
    } else {
        return Ok(span);
    };
    let leading = before.iter().rev().take_while(blank).count();
    let at_line_start =
        before.len() == leading && from == 0 || before.iter().rev().nth(leading) == Some(&b'\n');
    let back = if at_line_start { leading } else { 0 };
    Ok(Span {
        start: span.start - u64::try_from(back).unwrap_or(0),
        end: span.end + u64::try_from(trailing + eol).unwrap_or(0),
    })
}

/// A span of the original, and what the output has in its place.
struct Edit<'a> {
    span: Span,
    with: &'a [u8],
}

/// Writes `original` with the sorted, separate `edits` applied to `to`, with the original's permissions, and gives
/// both files' digests. An unchanged file is a hard link where it can be, and has no digests; anything else goes
/// through a new temporary file renamed into place, since `to` may be a hard link to the original, or the original
/// itself.
fn splice(original: &Path, edits: &[Edit], to: &Path) -> Result<Option<Digests>, FixError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|source| write_error(parent, source))?;
    }
    if edits.is_empty() && fs::hard_link(original, to).is_ok() {
        return Ok(None);
    }
    let permissions = fs::metadata(original)
        .map_err(|source| write_error(original, source))?
        .permissions();
    let mut digests = None;
    write_file(to, Some(permissions), |writer| {
        let mut reader = Hashing::new(BufReader::with_capacity(1 << 16, File::open(original)?));
        let mut writer = Hashing::new(writer);
        let mut at = 0;
        for edit in edits {
            let Span { start, end } = edit.span;
            io::copy(&mut (&mut reader).take(start - at), &mut writer)?;
            io::copy(&mut (&mut reader).take(end - start), &mut io::sink())?;
            writer.write_all(edit.with)?;
            at = end;
        }
        io::copy(&mut reader, &mut writer)?;
        digests = Some(Digests {
            before: reader.hex(),
            after: writer.hex(),
        });
        Ok(())
    })?;
    Ok(digests)
}

/// A reader or writer that hashes the bytes passing through it.
struct Hashing<T> {
    inner: T,
    hasher: Sha256,
}

impl<T> Hashing<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn hex(&self) -> String {
        self.hasher
            .clone()
            .finalize()
            .iter()
            .flat_map(|b| [b >> 4, b & 0xf])
            .filter_map(|n| char::from_digit(u32::from(n), 16))
            .collect()
    }
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(buf.get(..n).unwrap_or_default());
        Ok(n)
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(buf.get(..n).unwrap_or_default());
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes `path` through a new temporary file beside it, renamed over it once written: whatever `path` was (a hard
/// link, a symbolic link) is replaced, never written through.
fn write_file(
    path: &Path,
    permissions: Option<fs::Permissions>,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<(), FixError> {
    let (temporary, file) = temporary_beside(path).map_err(|source| write_error(path, source))?;
    let result = (|| -> io::Result<()> {
        let mut writer = BufWriter::with_capacity(1 << 16, file);
        write(&mut writer)?;
        let file = writer
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        // The temporary file is ours and half-written; the error below is what matters.
        drop(fs::remove_file(&temporary));
    }
    result.map_err(|source| write_error(path, source))
}

/// A file created new beside `path`, under a name nothing else has.
fn temporary_beside(path: &Path) -> io::Result<(PathBuf, File)> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for attempt in 0_u32.. {
        let temporary = folder.join(format!(".{name}.sqlca-{attempt}.tmp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("no free temporary name"))
}

/// Fills each removal's line in the original, its original bytes and, with `annotate`, its comment, reading each
/// changed input once.
fn describe_removals(
    removed: &mut [Removal],
    files: &[InputFile],
    annotate: bool,
) -> Result<(), FixError> {
    removed.sort_by(|a, b| (&a.file, a.span.start).cmp(&(&b.file, b.span.start)));
    for file in files {
        let name = check::display(&file.path);
        let mut mine = removed.iter_mut().filter(|r| r.file == name).peekable();
        if mine.peek().is_none() {
            continue;
        }
        let read_error = |source| write_error(&file.path, source);
        let mut reader = BufReader::new(File::open(&file.path).map_err(read_error)?);
        let (mut at, mut line, mut last_byte) = (0_u64, 1_u64, None);
        for removal in mine {
            let mut before = Vec::new();
            (&mut reader)
                .take(removal.span.start.saturating_sub(at))
                .read_to_end(&mut before)
                .map_err(read_error)?;
            line += newlines(&before);
            last_byte = before.last().copied().or(last_byte);
            let mut text = Vec::new();
            (&mut reader)
                .take(removal.span.end - removal.span.start)
                .read_to_end(&mut text)
                .map_err(read_error)?;
            removal.line = line;
            line += newlines(&text);
            at = removal.span.end;
            removal.text = text;
            if !removal.nulled.is_empty() {
                removal.replacement = with_nulls(removal, annotate);
            } else if annotate {
                removal.replacement = annotation(removal, last_byte.is_none_or(|b| b == b'\n'));
            }
            last_byte = removal.text.last().copied().or(last_byte);
        }
    }
    Ok(())
}

/// The comment `annotate` leaves for `removal`: a line of its own, indented as the removed line was, when the removal
/// took whole lines; else a block comment, so the rest of the line stays as it was. It keeps the removal's line end.
fn annotation(removal: &Removal, at_line_start: bool) -> Vec<u8> {
    let what = if removal.clause { "a row" } else { "an INSERT" };
    let mut briefs: Vec<&str> = Vec::new();
    for reason in &removal.reasons {
        if !briefs.contains(&reason.brief.as_str()) {
            briefs.push(&reason.brief);
        }
    }
    let note = safe(&format!(
        "sqlca fix removed {what} here ({}); see rejects.sql",
        briefs.join(", ")
    ));
    let text = &removal.text;
    let eol: &[u8] = if text.ends_with(b"\r\n") {
        b"\r\n"
    } else if text.ends_with(b"\n") {
        b"\n"
    } else {
        b""
    };
    let mut comment = Vec::new();
    if at_line_start && !eol.is_empty() {
        let indent = text.iter().take_while(|b| **b == b' ' || **b == b'\t');
        comment.extend(indent);
        comment.extend_from_slice(b"-- ");
        comment.extend_from_slice(note.as_bytes());
    } else {
        comment.extend_from_slice(b"/* ");
        comment.extend_from_slice(note.as_bytes());
        comment.extend_from_slice(b" */");
    }
    comment.extend_from_slice(eol);
    comment
}

/// Names can hold anything. These characters read the same in every encoding the input can have, and none can end a
/// comment, continue a line or start a SQL*Plus substitution.
fn safe(note: &str) -> String {
    note.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " _.,;()".contains(c) {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// A changed row's text with NULL for each value it sets to NULL, followed with `annotate` by a comment saying why.
fn with_nulls(removal: &Removal, annotate: bool) -> Vec<u8> {
    let mut text = Vec::with_capacity(removal.text.len());
    let mut at = 0;
    for (column, span) in &removal.nulled {
        let from = usize::try_from(span.start - removal.span.start).unwrap_or(usize::MAX);
        let to = usize::try_from(span.end - removal.span.start).unwrap_or(usize::MAX);
        let (Some(before), Some(_)) = (removal.text.get(at..from), removal.text.get(from..to))
        else {
            continue;
        };
        text.extend_from_slice(before);
        text.extend_from_slice(b"NULL");
        if annotate {
            let note = safe(&format!("sqlca fix set {column} to NULL; see rejects.sql"));
            text.extend_from_slice(format!(" /* {note} */").as_bytes());
        }
        at = to;
    }
    text.extend_from_slice(removal.text.get(at..).unwrap_or_default());
    text
}

fn newlines(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.split(|&b| b == b'\n').count() - 1).unwrap_or(0)
}

/// `rejects.sql`: each removed row's original bytes, after a comment saying where it was and why it went.
pub fn write_rejects(removed: &[Removal], out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "-- Rows sqlca fix removed or changed, in their original form. Each can be corrected and loaded by hand."
    )?;
    for removal in removed {
        writeln!(out)?;
        let part = if removal.clause {
            " (one row of a multi-row INSERT)"
        } else {
            ""
        };
        writeln!(
            out,
            "-- {}:{}{part}, round {}{}",
            removal.file,
            removal.line,
            removal.round,
            set_to_null(removal).map_or_else(String::new, |s| format!("; kept with {s}"))
        )?;
        for reason in &removal.reasons {
            writeln!(out, "--   {}", reason.text)?;
        }
        out.write_all(&removal.text)?;
        if !removal.text.ends_with(b"\n") {
            writeln!(out)?;
        }
    }
    Ok(())
}

/// `DEPT_ID set to NULL`, for a changed row.
fn set_to_null(removal: &Removal) -> Option<String> {
    if removal.nulled.is_empty() {
        return None;
    }
    let columns: Vec<&str> = removal.nulled.iter().map(|(c, _)| c.as_str()).collect();
    Some(format!("{} set to NULL", columns.join(", ")))
}

/// `Fixed      12 rows in 2 rounds (missing parent 9, duplicate key 2, NULL in NOT NULL 1); 1 left for review`.
pub fn summary(report: &FixReport) -> String {
    let mut counts: BTreeMap<FindingKind, usize> = BTreeMap::new();
    for reason in report.removed.iter().flat_map(|r| &r.reasons) {
        *counts.entry(reason.kind).or_default() += 1;
    }
    let kinds = crate::report::kind_counts(&counts);
    let changed = report
        .removed
        .iter()
        .filter(|r| !r.nulled.is_empty())
        .count();
    let changed = if changed == 0 {
        String::new()
    } else {
        format!("; {changed} kept with a foreign key set to NULL")
    };
    let committed = match report.committed.len() {
        0 => String::new(),
        1 => "; COMMIT added to 1 file".to_owned(),
        n => format!("; COMMIT added to {n} files"),
    };
    let left = if report.left.is_empty() {
        String::new()
    } else {
        format!("; {} left for review", report.left.len())
    };
    let rounds = if report.rounds == 1 {
        "round"
    } else {
        "rounds"
    };
    let fixed = if report.target.dry_run {
        "Would fix "
    } else {
        "Fixed     "
    };
    if report.removed.is_empty() && !committed.is_empty() {
        let committed = committed.trim_start_matches("; ");
        return format!("{fixed} {committed}{left}");
    }
    format!(
        "{fixed} {} rows in {} {rounds}{kinds}{changed}{committed}{left}",
        report.removed.len(),
        report.rounds
    )
}

/// Where the fix writes, for the run's header and the reports.
pub fn destination_text(target: &Target) -> String {
    match &target.destination {
        Destination::Out(dir) => {
            format!("fixed copies under {}, originals untouched", dir.display())
        }
        Destination::InPlace {
            backup: Some(suffix),
        } => format!("the inputs, in place; each changed one kept first as <file>{suffix}"),
        Destination::InPlace { backup: None } => "the inputs, in place, without backups".to_owned(),
    }
}

/// `fix-report.txt`: what the fix did, for people: where it wrote, each removal with its reasons, the files it
/// changed, what it left, and what the output still has.
pub fn write_report(report: &FixReport, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "sqlca {} fix", env!("CARGO_PKG_VERSION"))?;
    writeln!(out, "Output: {}", destination_text(&report.target))?;
    if report.target.dry_run {
        writeln!(out, "Dry run: nothing was written")?;
    }
    writeln!(out, "{}", summary(report))?;
    writeln!(out)?;
    writeln!(out, "Removed:")?;
    for removal in &report.removed {
        let part = if removal.clause {
            ", one row of a multi-row INSERT"
        } else {
            ""
        };
        let kept = set_to_null(removal).map_or_else(String::new, |s| format!(": kept with {s}"));
        writeln!(
            out,
            "  {}:{} (round {}{part}){kept}",
            removal.file, removal.line, removal.round
        )?;
        for reason in &removal.reasons {
            writeln!(out, "    {}", reason.text)?;
        }
    }
    if !report.committed.is_empty() {
        writeln!(out)?;
        writeln!(out, "Committed:")?;
        for commit in &report.committed {
            writeln!(out, "  {}: COMMIT added at the end", commit.file)?;
            writeln!(out, "    {}", crate::report::describe(&commit.finding))?;
        }
    }
    let changed: Vec<&Output> = report.files.iter().filter(|f| f.changed).collect();
    if !changed.is_empty() {
        writeln!(out)?;
        writeln!(out, "Changed:")?;
        for file in changed {
            let shown = match report.target.destination {
                Destination::Out(_) => check::display(&file.path),
                Destination::InPlace { .. } => file.input.clone(),
            };
            match file.backup.as_deref().and_then(Path::file_name) {
                Some(backup) => writeln!(
                    out,
                    "  {shown}, original kept beside it as {}",
                    backup.to_string_lossy()
                )?,
                None => writeln!(out, "  {shown}")?,
            }
        }
    }
    if !report.left.is_empty() {
        writeln!(out)?;
        writeln!(out, "Left for review:")?;
        for left in &report.left {
            writeln!(
                out,
                "  {}: {}",
                crate::report::describe(&left.finding),
                left.why
            )?;
        }
    }
    writeln!(out)?;
    writeln!(out, "Still in the output:")?;
    for finding in &report.output.findings {
        writeln!(out, "  {}", crate::report::describe(finding))?;
    }
    writeln!(out, "{}", crate::report::result(&report.output))
}

#[derive(serde::Serialize)]
struct JsonReport<'a> {
    sqlca: &'static str,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    out: Option<String>,
    in_place: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_suffix: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report: Option<String>,
    rounds: usize,
    files: Vec<JsonFile<'a>>,
    removed: Vec<JsonRemoval<'a>>,
    committed: Vec<JsonCommit<'a>>,
    left: Vec<JsonLeft<'a>>,
    output: JsonOutput<'a>,
}
#[derive(serde::Serialize)]
struct JsonCommit<'a> {
    file: &'a str,
    finding: &'a Finding,
}
#[derive(serde::Serialize)]
struct JsonFile<'a> {
    input: &'a str,
    output: String,
    changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256_before: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256_after: Option<&'a str>,
}
#[derive(serde::Serialize)]
struct JsonRemoval<'a> {
    file: &'a str,
    line: u64,
    round: usize,
    clause: bool,
    reasons: Vec<JsonReason<'a>>,
    /// For a row kept rather than removed: the columns set to NULL.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    nulled: Vec<&'a str>,
}
#[derive(serde::Serialize)]
struct JsonReason<'a> {
    kind: FindingKind,
    text: &'a str,
}
#[derive(serde::Serialize)]
struct JsonLeft<'a> {
    finding: &'a Finding,
    why: &'a str,
}
#[derive(serde::Serialize)]
struct JsonOutput<'a> {
    findings: &'a [Finding],
    summary: crate::report::Summary<'a>,
}

/// `fix-report.json`: `fix-report.txt` for tools, with each written file's SHA-256 before and after.
pub fn write_json_report(report: &FixReport, out: &mut dyn Write) -> io::Result<()> {
    let (out_dir, backup_suffix) = match &report.target.destination {
        Destination::Out(dir) => (Some(check::display(dir)), None),
        Destination::InPlace { backup } => (None, backup.as_deref()),
    };
    let json = JsonReport {
        sqlca: env!("CARGO_PKG_VERSION"),
        dry_run: report.target.dry_run,
        in_place: out_dir.is_none(),
        out: out_dir,
        backup_suffix,
        report: report.report.as_deref().map(check::display),
        rounds: report.rounds,
        files: report
            .files
            .iter()
            .map(|f| JsonFile {
                input: &f.input,
                output: check::display(&f.path),
                changed: f.changed,
                backup: f.backup.as_deref().map(check::display),
                sha256_before: f.sha256.as_ref().map(|d| d.before.as_str()),
                sha256_after: f.sha256.as_ref().map(|d| d.after.as_str()),
            })
            .collect(),
        removed: report
            .removed
            .iter()
            .map(|r| JsonRemoval {
                file: &r.file,
                line: r.line,
                round: r.round,
                clause: r.clause,
                reasons: r
                    .reasons
                    .iter()
                    .map(|reason| JsonReason {
                        kind: reason.kind,
                        text: &reason.text,
                    })
                    .collect(),
                nulled: r.nulled.iter().map(|(c, _)| c.as_str()).collect(),
            })
            .collect(),
        committed: report
            .committed
            .iter()
            .map(|c| JsonCommit {
                file: &c.file,
                finding: &c.finding,
            })
            .collect(),
        left: report
            .left
            .iter()
            .map(|l| JsonLeft {
                finding: &l.finding,
                why: l.why,
            })
            .collect(),
        output: JsonOutput {
            findings: &report.output.findings,
            summary: crate::report::summary(&report.output, &crate::report::Omitted::default()),
        },
    };
    serde_json::to_writer_pretty(&mut *out, &json).map_err(io::Error::other)?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn utc_stamps_match_the_calendar() {
        // From `date -u -d @SECONDS +%Y%m%dT%H%M%SZ`.
        for (seconds, stamp) in [
            (0, "19700101T000000Z"),
            (951_782_400, "20000229T000000Z"),
            (1_709_251_199, "20240229T235959Z"),
            (1_793_318_400, "20261030T000000Z"),
            (4_102_444_799, "20991231T235959Z"),
        ] {
            assert_eq!(utc_stamp(UNIX_EPOCH + Duration::from_secs(seconds)), stamp);
        }
    }

    #[test]
    fn a_backup_is_the_file_name_with_the_suffix() {
        assert_eq!(
            beside(Path::new("data/emp.sql"), ".orig"),
            Path::new("data/emp.sql.orig")
        );
    }
}
