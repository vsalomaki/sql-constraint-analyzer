//! The check: a schema pass builds the catalog from every file's DDL, then a rows pass matches each inserted row to
//! its table and runs the constraint checks on it. What a row can't settle on its own (a foreign key whose parent may
//! come later) is resolved once every row is in.

mod rows;
mod structure;

use std::path::Path;

pub use structure::{Row, StructureError, resolve_row};

use crate::catalog::{CatalogBuilder, Issue, Origin};
use crate::dialect::{DialectId, Pass, Runner};
use crate::finding::Finding;
use crate::input::{self, InputError, InputFile, Role};
use crate::source::{Encoding, Piece};
use crate::statement::{Skip, Statement};

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub dialect: DialectId,
    pub runner: Runner,
    pub encoding: Encoding,
    /// The schema of unqualified names, as the dialect stores it.
    pub default_schema: Option<String>,
    /// Judge rows in the order the files run, reporting children inserted before their parents.
    pub load_order: bool,
}

/// Something about the run that isn't a finding, with where it is when that is known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    pub file: String,
    pub line: Option<u64>,
    pub message: String,
}

/// A count of rows the checker couldn't fully judge, by what kept it from it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Warning {
    /// Keys with a value the checker can't know, such as a sequence or `SYSDATE`: neither satisfied nor violated.
    Unverifiable {
        table: String,
        constraint: Option<String>,
        count: u64,
        /// One of the reasons, for the message.
        reason: String,
    },
    /// A foreign key whose parent table no file defines, so none of its rows can be checked.
    ParentNotInInput {
        table: String,
        constraint: Option<String>,
        parent: String,
        count: u64,
    },
    /// Dates written as plain text, read as ISO; the database reads them with its session's date format.
    DateAssumed {
        table: String,
        column: String,
        count: u64,
    },
}

#[derive(Debug, Default)]
pub struct Report {
    /// By file, then line; summary findings last.
    pub findings: Vec<Finding>,
    pub warnings: Vec<Warning>,
    /// Statements that couldn't be read, and files that couldn't be read to the end.
    pub errors: Vec<Problem>,
    /// DDL the catalog couldn't apply as written.
    pub catalog: Vec<Problem>,
    /// Statements that change data in ways the checker doesn't follow.
    pub not_followed: Vec<Problem>,
    /// Statements the checker has no use for (commits, grants, sequences, SQL*Plus commands).
    pub unused: u64,
    pub rows: u64,
}

/// Checks `files`, in the order given. Only a file that can't be opened stops the run; everything else ends up in the
/// report.
pub fn run(files: &[InputFile], options: &Options) -> Result<Report, InputError> {
    let names: Vec<String> = files.iter().map(|f| display(&f.path)).collect();
    let mut report = Report::default();
    let schema_given = files.iter().any(|f| f.role == Role::Schema);

    let mut builder = CatalogBuilder::new(options.dialect.rules(), options.default_schema.clone());
    for (index, file) in files.iter().enumerate() {
        if schema_given && file.role == Role::Data {
            continue;
        }
        read(
            file,
            index,
            &names,
            Pass::Schema,
            options,
            &mut report,
            |statement, origin| {
                builder.add(statement, origin);
            },
        )?;
    }
    let (catalog, issues) = builder.finish();
    report.catalog = issues
        .into_iter()
        .map(|Issue { origin, message }| problem(&names, origin.file, Some(origin.line), message))
        .collect();

    let mut checker = rows::Checker::new(&catalog, options.dialect, options.load_order, &names);
    let mut not_followed = Vec::new();
    let mut unused = 0;
    let mut count = 0;
    for (index, file) in files.iter().enumerate() {
        if file.role == Role::Schema {
            continue;
        }
        let reference = file.role == Role::Reference;
        read(
            file,
            index,
            &names,
            Pass::Rows,
            options,
            &mut report,
            |statement, origin| match statement {
                Statement::Insert(inserted) => {
                    let file = names.get(index).cloned().unwrap_or_default();
                    let mut matched = Vec::with_capacity(inserted.len());
                    for row in &inserted {
                        count += 1;
                        let at = rows::At {
                            file: index,
                            offset: row.span.start,
                            line: row.line,
                        };
                        match resolve_row(&catalog, row) {
                            Ok(resolved) => matched.push((resolved, row, at)),
                            Err(error) => {
                                checker.structure(error.finding(&catalog, row, &file), reference);
                            }
                        }
                    }
                    checker.statement(&matched, options.encoding, reference);
                }
                Statement::Skipped(Skip::NotFollowed(what)) => {
                    not_followed.push(problem(&names, origin.file, Some(origin.line), what));
                }
                Statement::Skipped(Skip::Unused(_)) => unused += 1,
                _ => {}
            },
        )?;
    }
    let (mut findings, warnings) = checker.finish();
    let position = |file: &Option<String>| {
        file.as_ref()
            .and_then(|f| names.iter().position(|n| n == f))
    };
    findings.sort_by(|a, b| {
        (position(&a.file).unwrap_or(usize::MAX), a.line, a).cmp(&(
            position(&b.file).unwrap_or(usize::MAX),
            b.line,
            b,
        ))
    });
    report.findings = findings;
    report.warnings = warnings;
    report.not_followed = not_followed;
    report.unused = unused;
    report.rows = count;
    Ok(report)
}

/// A path as the user gave it, with `/` between its parts on Windows too, so findings read the same everywhere.
fn display(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

fn problem(names: &[String], file: usize, line: Option<u64>, message: String) -> Problem {
    Problem {
        file: names.get(file).cloned().unwrap_or_default(),
        line,
        message,
    }
}

/// Reads one file's statements for `pass`, handing each to `take` with where it starts.
fn read(
    file: &InputFile,
    index: usize,
    names: &[String],
    pass: Pass,
    options: &Options,
    report: &mut Report,
    mut take: impl FnMut(Statement, Origin),
) -> Result<(), InputError> {
    let opened = input::open(&file.path)?;
    for piece in options
        .dialect
        .pieces(opened.reader, options.runner, opened.base_offset)
    {
        let piece: Piece = match piece {
            Ok(piece) => piece,
            Err(error) => {
                report
                    .errors
                    .push(problem(names, index, None, error.to_string()));
                break;
            }
        };
        match options.dialect.parse(&piece, options.encoding, pass) {
            Ok(Some(statement)) => {
                let origin = Origin {
                    file: index,
                    offset: piece.span.start,
                    line: piece.line,
                };
                take(statement, origin);
            }
            Ok(None) => {}
            Err(error) => {
                report
                    .errors
                    .push(problem(names, index, Some(error.line), error.message));
            }
        }
    }
    Ok(())
}
