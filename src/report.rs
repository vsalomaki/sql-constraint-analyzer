//! What a check shows: findings on stdout, as text or JSON Lines, and everything about the run on stderr: what was
//! read, what couldn't be, what the checker couldn't decide, and the result.

use std::collections::BTreeMap;
use std::io::{self, Write};

use serde::Serialize;

use crate::check::{Problem, Report, Warning};
use crate::finding::{Finding, FindingKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Format {
    #[default]
    Text,
    /// JSON Lines: one finding per line, then a summary line.
    Json,
}

/// How much of the run stderr shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    /// The result line only.
    Quiet,
    #[default]
    Normal,
    /// Every statement not followed, one per line.
    Verbose,
}

/// The findings a cap left out, by what they were grouped by.
pub type Omitted = BTreeMap<String, u64>;

/// What a finding is capped by: its constraint, or its kind, table and columns when it has none, so two unnamed
/// constraints of a table are capped apart.
fn group(finding: &Finding) -> String {
    finding.constraint.clone().unwrap_or_else(|| {
        format!(
            "{} in {} ({})",
            kind_name(finding.kind),
            finding.table,
            finding.columns.join(", ")
        )
    })
}

/// Writes the findings, at most `cap` per constraint (0: all), and returns what the cap left out.
pub fn write_findings(
    findings: &[Finding],
    format: Format,
    cap: u64,
    out: &mut dyn Write,
) -> io::Result<Omitted> {
    let mut shown: BTreeMap<String, u64> = BTreeMap::new();
    let mut omitted = Omitted::new();
    for finding in findings {
        let group = group(finding);
        let count = shown.entry(group.clone()).or_default();
        if cap > 0 && *count >= cap {
            *omitted.entry(group).or_default() += 1;
            continue;
        }
        *count += 1;
        match format {
            Format::Text => writeln!(out, "{}", describe(finding))?,
            Format::Json => writeln!(
                out,
                "{}",
                serde_json::to_string(finding).map_err(io::Error::other)?
            )?,
        }
    }
    Ok(omitted)
}

/// The summary line ending a JSON Lines output.
pub fn write_json_summary(
    report: &Report,
    omitted: &Omitted,
    out: &mut dyn Write,
) -> io::Result<()> {
    #[derive(Serialize)]
    struct Summary<'a> {
        findings: usize,
        by_kind: BTreeMap<FindingKind, usize>,
        omitted: u64,
        rows: u64,
        errors: usize,
        warnings: usize,
        exit: u8,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        problems: Vec<&'a Problem>,
    }
    #[derive(Serialize)]
    struct Line<'a> {
        summary: Summary<'a>,
    }
    let line = Line {
        summary: Summary {
            findings: report.findings.len(),
            by_kind: by_kind(&report.findings),
            omitted: omitted.values().sum(),
            rows: report.rows,
            errors: report.errors.len() + report.unreadable.len(),
            warnings: warning_count(report),
            exit: exit_code(report),
            problems: report.unreadable.iter().chain(&report.errors).collect(),
        },
    };
    writeln!(
        out,
        "{}",
        serde_json::to_string(&line).map_err(io::Error::other)?
    )
}

fn by_kind(findings: &[Finding]) -> BTreeMap<FindingKind, usize> {
    let mut counts = BTreeMap::new();
    for finding in findings {
        *counts.entry(finding.kind).or_default() += 1;
    }
    counts
}

/// 2 when a file couldn't be read to the end, so the check is incomplete; 1 when the export has findings or
/// statements that couldn't be read; 0 when it is clean.
pub fn exit_code(report: &Report) -> u8 {
    if report.unreadable.is_empty() {
        u8::from(!report.findings.is_empty() || !report.errors.is_empty())
    } else {
        2
    }
}

fn kind_name(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::FkMissingParent => "missing parent",
        FindingKind::FkLoadOrder => "child before parent",
        FindingKind::DuplicateKey => "duplicate key",
        FindingKind::NotNull => "NULL in NOT NULL",
        FindingKind::UnknownTable => "unknown table",
        FindingKind::AmbiguousTable => "ambiguous table",
        FindingKind::UnknownColumn => "unknown column",
        FindingKind::ValueCountMismatch => "value count",
        FindingKind::ParentTableEmpty => "parent table empty",
    }
}

/// `COL='value', COL2=NULL`: a key's columns with their canonical values.
fn key(finding: &Finding) -> String {
    finding
        .columns
        .iter()
        .zip(&finding.key)
        .map(|(column, value)| match value {
            Some(value) => format!("{column}='{}'", value.replace('\'', "''")),
            None => format!("{column}=NULL"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One finding as a line: where it is, what it is, and the values involved.
pub fn describe(finding: &Finding) -> String {
    let place = match (&finding.file, finding.line) {
        (Some(file), Some(line)) => format!("{file}:{line}: "),
        _ => String::new(),
    };
    let table = &finding.table;
    let columns = finding.columns.join(", ");
    let named = |unnamed: &str| {
        finding
            .constraint
            .clone()
            .unwrap_or_else(|| format!("{unnamed} {table}"))
    };
    let text = match finding.kind {
        FindingKind::FkMissingParent => {
            format!(
                "missing parent for {}: {}",
                named("a foreign key of"),
                key(finding)
            )
        }
        FindingKind::FkLoadOrder => format!(
            "child before parent for {}: {}, whose parent comes later",
            named("a foreign key of"),
            key(finding)
        ),
        FindingKind::DuplicateKey => {
            let first = finding
                .first
                .as_ref()
                .map(|f| format!(", first at {}:{}", f.file, f.line))
                .unwrap_or_default();
            format!("duplicate key {}: {}{first}", named("of"), key(finding))
        }
        FindingKind::NotNull => match &finding.constraint {
            None => format!("NULL in NOT NULL column {table}.{columns}"),
            Some(constraint) => {
                format!("NULL in {table}.{columns}, which {constraint} makes NOT NULL")
            }
        },
        FindingKind::UnknownTable => format!("unknown table {table}"),
        FindingKind::AmbiguousTable => {
            format!("table {table} exists in several schemas; give --default-schema")
        }
        FindingKind::UnknownColumn => format!("unknown column {columns} in {table}"),
        FindingKind::ValueCountMismatch => {
            format!("the value count doesn't match the columns of {table}")
        }
        FindingKind::ParentTableEmpty => format!(
            "{}: the parent table has no rows, so {} rows of {table} have no parent",
            named("a foreign key of"),
            finding.count.unwrap_or(0)
        ),
    };
    let assumed = if finding.assumed {
        " (assumes dates written as text are YYYY-MM-DD)"
    } else {
        ""
    };
    format!("{place}{text}{assumed}")
}

fn warning_count(report: &Report) -> usize {
    report.warnings.len() + report.catalog.len() + usize::from(!report.not_followed.is_empty())
}

fn problem(problem: &Problem) -> String {
    match problem.line {
        Some(line) => format!("{}:{line}: {}", problem.file, problem.message),
        None => format!("{}: {}", problem.file, problem.message),
    }
}

/// Everything about the run that isn't a finding, and the result line, for stderr.
pub fn write_run(
    report: &Report,
    omitted: &Omitted,
    verbosity: Verbosity,
    err: &mut dyn Write,
) -> io::Result<()> {
    if verbosity != Verbosity::Quiet {
        let schema = report.schema;
        writeln!(
            err,
            "Schema     {} tables · {} keys ({} disabled, {} deferred)",
            schema.tables, schema.keys, schema.disabled, schema.deferred
        )?;
        writeln!(err, "Data       {} rows", report.rows)?;
        for error in report.unreadable.iter().chain(&report.errors) {
            writeln!(err, "error: {}", problem(error))?;
        }
        for issue in &report.catalog {
            writeln!(err, "warning: {}", problem(issue))?;
        }
        for warning in &report.warnings {
            writeln!(err, "warning: {}", self::warning(warning))?;
        }
        if !report.not_followed.is_empty() {
            writeln!(
                err,
                "warning: {} statements change data in ways the check doesn't follow, so the end state may differ{}",
                report.not_followed.len(),
                if verbosity == Verbosity::Verbose {
                    ":"
                } else {
                    " (-v lists them)"
                }
            )?;
            if verbosity == Verbosity::Verbose {
                for statement in &report.not_followed {
                    writeln!(err, "  {}", problem(statement))?;
                }
            }
        }
        for (group, count) in omitted {
            writeln!(
                err,
                "note: {count} more findings for {group} not shown (--max-per-constraint 0 shows all)"
            )?;
        }
    }
    writeln!(err, "{}", result(report))
}

fn warning(warning: &Warning) -> String {
    let named = |constraint: &Option<String>, table: &str| {
        constraint
            .clone()
            .unwrap_or_else(|| format!("a constraint of {table}"))
    };
    match warning {
        Warning::Unverifiable {
            table,
            constraint,
            count,
            reason,
        } => format!(
            "{count} keys of {} can't be checked: they involve {reason}",
            named(constraint, table)
        ),
        Warning::ParentNotInInput {
            table,
            constraint,
            parent,
            count,
        } => format!(
            "{count} rows of {} can't be checked: no input defines its parent table {parent} (give it with --reference)",
            named(constraint, table)
        ),
        Warning::DateAssumed {
            table,
            column,
            count,
        } => format!(
            "{count} values of {table}.{column} are dates written as text, read as YYYY-MM-DD; findings that depend \
             on them are marked"
        ),
    }
}

/// `Result     4 findings (missing parent 3, duplicate key 1) · 2 warnings · 0 errors → exit 1`.
pub fn result(report: &Report) -> String {
    let kinds: Vec<String> = by_kind(&report.findings)
        .iter()
        .map(|(kind, n)| format!("{} {n}", kind_name(*kind)))
        .collect();
    let kinds = if kinds.is_empty() {
        String::new()
    } else {
        format!(" ({})", kinds.join(", "))
    };
    format!(
        "Result     {} findings{kinds} · {} warnings · {} errors → exit {}",
        report.findings.len(),
        warning_count(report),
        report.errors.len() + report.unreadable.len(),
        exit_code(report)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem() -> Problem {
        Problem {
            file: "a.sql".into(),
            line: Some(1),
            message: "x".into(),
        }
    }

    #[test]
    fn an_incomplete_read_exits_2_before_findings_or_statement_errors_exit_1() {
        let mut report = Report::default();
        assert_eq!(exit_code(&report), 0);
        report.errors.push(problem());
        assert_eq!(exit_code(&report), 1);
        report.unreadable.push(problem());
        assert_eq!(exit_code(&report), 2);
        assert!(
            result(&report).ends_with("2 errors → exit 2"),
            "{}",
            result(&report)
        );
    }
}
