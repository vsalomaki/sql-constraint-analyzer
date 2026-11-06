//! `fix`: the check, then the input written again without the rows that break a constraint.
//!
//! The output is always the original bytes minus a growing set of removed ranges, so everything not removed stays
//! byte-identical and the removed bytes, kept in `rejects.sql`, restore the input exactly. Rounds repeat the check on
//! the output: a removed parent can orphan its children, which the next round removes. A round that removes nothing
//! ends the run, and its check is the verified result.
//!
//! Rows are found again by file and line: the fix re-reads each file it changes anyway, so findings carry no spans.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::check::{self, Report};
use crate::dialect::Pass;
use crate::finding::{Finding, FindingKind};
use crate::input::{self, InputError, InputFile};
use crate::source::Span;
use crate::statement::Statement;

/// What happens to a row with a missing parent, or with NULL in a NOT NULL column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnRow {
    #[default]
    Drop,
    Keep,
}

/// What happens to the later rows of a duplicate key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OnDuplicate {
    /// The first row stays; the later ones go.
    #[default]
    KeepFirst,
    Keep,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Policy {
    pub on_fk: OnRow,
    pub on_duplicate: OnDuplicate,
    pub on_not_null: OnRow,
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
    #[error("{} would be written where the report goes; give --out a folder of its own", .0.display())]
    ReportCollides(PathBuf),
}

/// A removed row: where it was in the input, what it was, and why it went.
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
}

/// Why a row went: the finding's kind, and the finding as a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub kind: FindingKind,
    pub text: String,
}

/// A finding the fix would act on but doesn't, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    pub finding: Finding,
    pub why: &'static str,
}

#[derive(Debug)]
pub struct FixReport {
    /// The check of the first round: what the input had.
    pub input: Report,
    /// The check of the last round: what the output still has.
    pub output: Report,
    pub rounds: usize,
    pub removed: Vec<Removal>,
    /// Fixable findings the fix left in place: assumed values, and rows it couldn't tell apart.
    pub left: Vec<Left>,
    /// Where each input went, by input position.
    pub written: Vec<PathBuf>,
}

/// Fixes `files` into `out`, keeping their paths relative to the folder they share.
pub fn fix(
    files: &[InputFile],
    options: &check::Options,
    policy: Policy,
    out: &Path,
) -> Result<FixReport, FixError> {
    let originals: Vec<PathBuf> = files
        .iter()
        .map(|f| fs::canonicalize(&f.path).map_err(|source| write_error(&f.path, source)))
        .collect::<Result<_, _>>()?;
    let written = destinations(&originals, out)?;
    let names: Vec<String> = files.iter().map(|f| check::display(&f.path)).collect();
    let outputs: Vec<InputFile> = files
        .iter()
        .zip(&written)
        .map(|(f, path)| InputFile {
            path: path.clone(),
            ..f.clone()
        })
        .collect();
    let output_names: Vec<String> = outputs.iter().map(|f| check::display(&f.path)).collect();

    let mut cuts: Vec<Vec<Span>> = vec![Vec::new(); files.len()];
    for (original, path) in originals.iter().zip(&written) {
        splice(original, &[], path)?;
    }
    let mut removed = Vec::new();
    let mut left = Vec::new();
    let mut first_report = None;
    let mut rounds = 0;
    let output = loop {
        let report = check::run(&outputs, options)?;
        let (targets, skipped) = targets(&report.findings, &output_names, policy);
        if rounds == 0 {
            left.extend(skipped);
        }
        let mut changed = BTreeSet::new();
        for (index, lines) in &targets {
            let (Some(output), Some(file)) = (outputs.get(*index), files.get(*index)) else {
                continue;
            };
            let located = locate(output, options, lines)?;
            let Some(file_cuts) = cuts.get_mut(*index) else {
                continue;
            };
            for unlocated in located.ambiguous {
                if rounds == 0 {
                    left.extend(lines.get(&unlocated).into_iter().flatten().map(|f| Left {
                        finding: (*f).clone(),
                        why: "the line holds several rows, so the fix can't tell which",
                    }));
                }
            }
            // This round's cuts are found in the output as the last round left it, so they map back through the
            // cuts as they were then.
            let before = file_cuts.clone();
            for cut in located.cuts {
                let span = to_original(cut.span, &before);
                let span = extend_to_line_end(&file.path, span)?;
                insert_cut(file_cuts, span);
                let name = names.get(*index).cloned().unwrap_or_default();
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
                });
                changed.insert(*index);
            }
        }
        if first_report.is_none() {
            first_report = Some(report.clone());
        }
        if changed.is_empty() {
            break report;
        }
        rounds += 1;
        for index in changed {
            if let (Some(original), Some(path), Some(file_cuts)) =
                (originals.get(index), written.get(index), cuts.get(index))
            {
                splice(original, file_cuts, path)?;
            }
        }
    };
    describe_removals(&mut removed, files)?;
    Ok(FixReport {
        input: first_report.unwrap_or_default(),
        output,
        rounds,
        removed,
        left,
        written,
    })
}

fn write_error(path: &Path, source: io::Error) -> FixError {
    FixError::Write {
        path: path.to_owned(),
        source,
    }
}

/// The folder in the output that holds `rejects.sql` and the fix reports.
pub const REPORT_FOLDER: &str = "sqlca-report";

/// Each input's path under `out`, relative to the folder all inputs share. `out` must be new or empty, so every
/// folder in it is one the fix made: nothing in it can lead back to an input.
fn destinations(originals: &[PathBuf], out: &Path) -> Result<Vec<PathBuf>, FixError> {
    let populated = fs::read_dir(out).is_ok_and(|mut entries| entries.next().is_some());
    if populated {
        return Err(FixError::OutNotEmpty(out.to_owned()));
    }
    fs::create_dir_all(out).map_err(|source| write_error(out, source))?;
    let out = fs::canonicalize(out).map_err(|source| write_error(out, source))?;
    let mut common = originals
        .first()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    while !originals.iter().all(|p| p.starts_with(&common)) {
        if !common.pop() {
            break;
        }
    }
    let written: Vec<PathBuf> = originals
        .iter()
        .map(|p| out.join(p.strip_prefix(&common).unwrap_or(p)))
        .collect();
    let report = out.join(REPORT_FOLDER);
    if let Some(clash) = written.iter().find(|p| p.starts_with(&report)) {
        return Err(FixError::ReportCollides(clash.clone()));
    }
    Ok(written)
}

/// The findings a round acts on, by file position and line, and the fixable ones it leaves for review.
type Targets<'a> = BTreeMap<usize, BTreeMap<u64, Vec<&'a Finding>>>;

fn targets<'a>(
    findings: &'a [Finding],
    names: &[String],
    policy: Policy,
) -> (Targets<'a>, Vec<Left>) {
    let fixed = |kind: FindingKind| match kind {
        FindingKind::FkMissingParent => policy.on_fk == OnRow::Drop,
        FindingKind::DuplicateKey => policy.on_duplicate == OnDuplicate::KeepFirst,
        FindingKind::NotNull => policy.on_not_null == OnRow::Drop,
        _ => false,
    };
    let position = |file: &str| names.iter().position(|n| n == file);
    let mut targets: Targets = BTreeMap::new();
    let mut left = Vec::new();
    for finding in findings.iter().filter(|f| fixed(f.kind)) {
        if finding.assumed {
            left.push(Left {
                finding: finding.clone(),
                why: "its key rests on a date written as text",
            });
            continue;
        }
        let (Some(index), Some(line)) = (finding.file.as_deref().and_then(position), finding.line)
        else {
            continue;
        };
        targets
            .entry(index)
            .or_default()
            .entry(line)
            .or_default()
            .push(finding);
    }
    // A duplicate whose first row goes this round waits: once the first row is gone, it may be the one to keep.
    let doomed: BTreeSet<(usize, u64)> = targets
        .iter()
        .flat_map(|(file, lines)| lines.keys().map(move |line| (*file, *line)))
        .collect();
    for lines in targets.values_mut() {
        for findings in lines.values_mut() {
            findings.retain(|f| {
                let first = f
                    .first
                    .as_ref()
                    .and_then(|l| Some((position(&l.file)?, l.line)));
                f.kind != FindingKind::DuplicateKey
                    || first.is_none_or(|first| !doomed.contains(&first))
            });
        }
        lines.retain(|_, findings| !findings.is_empty());
    }
    targets.retain(|_, lines| !lines.is_empty());
    (targets, left)
}

/// A cut in a file's current output, with why.
struct Cut {
    span: Span,
    clause: bool,
    reasons: Vec<Reason>,
}

struct Located {
    cuts: Vec<Cut>,
    /// Target lines holding more than one row.
    ambiguous: Vec<u64>,
}

/// The cuts that remove the target rows from `output`: a row's clause, or its whole statement when every row of
/// the statement goes.
fn locate(
    output: &InputFile,
    options: &check::Options,
    lines: &BTreeMap<u64, Vec<&Finding>>,
) -> Result<Located, FixError> {
    let opened = input::open(&output.path)?;
    let mut rows_per_line: BTreeMap<u64, usize> = BTreeMap::new();
    let mut statements = Vec::new();
    for piece in options
        .dialect
        .pieces(opened.reader, options.runner, opened.base_offset)
    {
        let Ok(piece) = piece else {
            break;
        };
        let Ok(Some(Statement::Insert(rows))) =
            options.dialect.parse(&piece, options.encoding, Pass::Rows)
        else {
            continue;
        };
        for row in &rows {
            *rows_per_line.entry(row.line).or_default() += 1;
        }
        let spans: Vec<(u64, Span)> = rows.iter().map(|r| (r.line, r.span)).collect();
        if spans.iter().any(|(line, _)| lines.contains_key(line)) {
            statements.push((piece.span, spans));
        }
    }
    let ambiguous: Vec<u64> = lines
        .keys()
        .copied()
        .filter(|line| rows_per_line.get(line).copied().unwrap_or(0) > 1)
        .collect();
    let mut cuts = Vec::new();
    for (statement, rows) in statements {
        let reasons = |line: &u64| -> Vec<Reason> {
            lines
                .get(line)
                .into_iter()
                .flatten()
                .map(|f| Reason {
                    kind: f.kind,
                    // Without places: they are the output's of this round, while removals are told in the input's.
                    text: crate::report::describe(&Finding {
                        file: None,
                        line: None,
                        first: None,
                        ..(*f).clone()
                    }),
                })
                .collect()
        };
        let going: Vec<&(u64, Span)> = rows
            .iter()
            .filter(|(line, _)| lines.contains_key(line) && !ambiguous.contains(line))
            .collect();
        if going.is_empty() {
            continue;
        }
        if going.len() == rows.len() {
            cuts.push(Cut {
                span: statement,
                clause: false,
                reasons: going.iter().flat_map(|(line, _)| reasons(line)).collect(),
            });
        } else {
            cuts.extend(going.iter().map(|(line, span)| Cut {
                span: *span,
                clause: true,
                reasons: reasons(line),
            }));
        }
    }
    Ok(Located { cuts, ambiguous })
}

/// A span in the current output as a span in the original: each earlier cut before an end shifts it by its length,
/// so a span that encloses an earlier cut grows by it.
fn to_original(span: Span, cuts: &[Span]) -> Span {
    Span {
        start: original_position(span.start, cuts, true),
        end: original_position(span.end, cuts, false),
    }
}

/// A cut exactly at the position lies before a span's start (the span begins after it) and after its end.
fn original_position(at: u64, cuts: &[Span], start: bool) -> u64 {
    let mut shift = 0;
    for cut in cuts {
        let before = if start {
            cut.start <= at + shift
        } else {
            cut.start < at + shift
        };
        if !before {
            break;
        }
        shift += cut.end - cut.start;
    }
    at + shift
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

/// Writes `original` without the `cuts` to `to`. An unchanged file is a hard link where it can be; anything else goes
/// through a new temporary file renamed into place, since `to` may be a hard link to the original.
fn splice(original: &Path, cuts: &[Span], to: &Path) -> Result<(), FixError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|source| write_error(parent, source))?;
    }
    if cuts.is_empty() && fs::hard_link(original, to).is_ok() {
        return Ok(());
    }
    write_file(to, |writer| {
        let mut reader = BufReader::with_capacity(1 << 16, File::open(original)?);
        let mut at = 0;
        for cut in cuts {
            io::copy(&mut (&mut reader).take(cut.start - at), writer)?;
            io::copy(
                &mut (&mut reader).take(cut.end - cut.start),
                &mut io::sink(),
            )?;
            at = cut.end;
        }
        io::copy(&mut reader, writer)?;
        Ok(())
    })
}

/// Writes `path` through a new temporary file beside it, renamed over it once written: whatever `path` was (a hard
/// link, a symbolic link) is replaced, never written through.
pub fn write_file(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<(), FixError> {
    let (temporary, file) = temporary_beside(path).map_err(|source| write_error(path, source))?;
    let result = (|| -> io::Result<()> {
        let mut writer = BufWriter::with_capacity(1 << 16, file);
        write(&mut writer)?;
        writer
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()?;
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

/// Fills each removal's line in the original and its original bytes, reading each changed input once.
fn describe_removals(removed: &mut [Removal], files: &[InputFile]) -> Result<(), FixError> {
    removed.sort_by(|a, b| (&a.file, a.span.start).cmp(&(&b.file, b.span.start)));
    for file in files {
        let name = check::display(&file.path);
        let mut mine = removed.iter_mut().filter(|r| r.file == name).peekable();
        if mine.peek().is_none() {
            continue;
        }
        let read_error = |source| write_error(&file.path, source);
        let mut reader = BufReader::new(File::open(&file.path).map_err(read_error)?);
        let (mut at, mut line) = (0_u64, 1_u64);
        for removal in mine {
            let mut before = Vec::new();
            (&mut reader)
                .take(removal.span.start.saturating_sub(at))
                .read_to_end(&mut before)
                .map_err(read_error)?;
            line += newlines(&before);
            let mut text = Vec::new();
            (&mut reader)
                .take(removal.span.end - removal.span.start)
                .read_to_end(&mut text)
                .map_err(read_error)?;
            removal.line = line;
            line += newlines(&text);
            at = removal.span.end;
            removal.text = text;
        }
    }
    Ok(())
}

fn newlines(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.split(|&b| b == b'\n').count() - 1).unwrap_or(0)
}

/// `rejects.sql`: each removed row's original bytes, after a comment saying where it was and why it went.
pub fn write_rejects(removed: &[Removal], out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "-- Rows sqlca fix removed, in their original form. Each can be corrected and loaded by hand."
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
            "-- {}:{}{part}, round {}",
            removal.file, removal.line, removal.round
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

/// `Fixed      12 rows in 2 rounds (missing parent 9, duplicate key 2, NULL in NOT NULL 1); 1 left for review`.
pub fn summary(report: &FixReport) -> String {
    let mut counts: BTreeMap<FindingKind, usize> = BTreeMap::new();
    for reason in report.removed.iter().flat_map(|r| &r.reasons) {
        *counts.entry(reason.kind).or_default() += 1;
    }
    let kinds: Vec<String> = counts
        .iter()
        .map(|(kind, n)| format!("{} {n}", crate::report::kind_name(*kind)))
        .collect();
    let kinds = if kinds.is_empty() {
        String::new()
    } else {
        format!(" ({})", kinds.join(", "))
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
    format!(
        "Fixed      {} rows in {} {rounds}{kinds}{left}",
        report.removed.len(),
        report.rounds
    )
}

/// `fix-report.txt`: what the fix did, for people: where it wrote, each removal with its reasons, what it left, and
/// what the output still has.
pub fn write_report(report: &FixReport, out_dir: &Path, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "sqlca {} fix", env!("CARGO_PKG_VERSION"))?;
    writeln!(
        out,
        "Output: fixed copies under {}, originals untouched",
        out_dir.display()
    )?;
    writeln!(out, "{}", summary(report))?;
    writeln!(out)?;
    writeln!(out, "Removed:")?;
    for removal in &report.removed {
        let part = if removal.clause {
            ", one row of a multi-row INSERT"
        } else {
            ""
        };
        writeln!(
            out,
            "  {}:{} (round {}{part})",
            removal.file, removal.line, removal.round
        )?;
        for reason in &removal.reasons {
            writeln!(out, "    {}", reason.text)?;
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
