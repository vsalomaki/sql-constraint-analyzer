//! The check: a schema pass builds the catalog from every file's DDL, then a rows pass matches each inserted row to
//! its table and runs the constraint checks on it. What a row can't settle on its own (a foreign key whose parent may
//! come later) is resolved once every row is in.

mod rows;
mod structure;

use std::collections::BTreeSet;
use std::path::Path;

pub use structure::{Row, StructureError, resolve_row};

use crate::catalog::{Catalog, CatalogBuilder, Issue, Kind, Origin, Parent};
use crate::dialect::{DialectId, Pass, Runner};
use crate::finding::{Finding, FindingKind};
use crate::input::{self, InputError, InputFile, Role};
use crate::source::{Encoding, Piece, Span};
use crate::statement::{Expr, Skip, Statement};

#[derive(Debug, Clone)]
pub struct Options {
    pub dialect: DialectId,
    pub runner: Runner,
    pub encoding: Encoding,
    /// The schema of unqualified names, as the dialect stores it.
    pub default_schema: Option<String>,
    /// Judge rows in the order the files run, reporting children inserted before their parents.
    pub load_order: bool,
    /// Report the rows at the end of a data file that no commit follows.
    pub commit_check: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dialect: DialectId::default(),
            runner: Runner::default(),
            encoding: Encoding::default(),
            default_schema: None,
            load_order: false,
            commit_check: true,
        }
    }
}

/// Something about the run that isn't a finding, with where it is when that is known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
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

#[derive(Debug, Clone, Default)]
pub struct Report {
    /// By file, then line; summary findings last.
    pub findings: Vec<Finding>,
    pub warnings: Vec<Warning>,
    /// Statements that couldn't be read.
    pub errors: Vec<Problem>,
    /// Files that couldn't be read to the end, so the check is incomplete.
    pub unreadable: Vec<Problem>,
    /// DDL the catalog couldn't apply as written.
    pub catalog: Vec<Problem>,
    /// Statements that change data in ways the checker doesn't follow.
    pub not_followed: Vec<Problem>,
    /// Statements the checker has no use for (sequences, SQL*Plus commands).
    pub unused: u64,
    pub rows: u64,
    pub schema: SchemaCounts,
    /// The parent keys enabled foreign keys point at, by table and columns as findings name them.
    pub referenced: BTreeSet<(String, Vec<String>)>,
}

/// What the schema pass found, for the run's summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchemaCounts {
    pub tables: usize,
    /// Primary keys, unique constraints and indexes, and foreign keys; NOT NULL and CHECK aren't counted.
    pub keys: usize,
    pub disabled: usize,
    pub deferred: usize,
}

/// What a run tells its observer as it goes, for a progress display. Every method does nothing by default.
pub trait Observer {
    /// A pass starts over `files` files of `bytes` bytes in all.
    fn pass(&mut self, _pass: Pass, _files: usize, _bytes: u64) {}
    /// The current pass starts reading the file at `index` in the run.
    fn file(&mut self, _index: usize, _name: &str, _bytes: u64) {}
    /// The current file has been read up to `offset`.
    fn advance(&mut self, _offset: u64) {}
    /// The schema pass built the catalog.
    fn schema(&mut self, _catalog: &Catalog) {}
    /// Every row is in; the foreign keys left pending are being resolved.
    fn resolve(&mut self) {}
    /// A run made of several checks, such as a fix, starts its next stage: `check 2`, `write`, `verify`.
    fn stage(&mut self, _stage: &str) {}
}

/// An observer that ignores everything.
pub struct Quiet;

impl Observer for Quiet {}

/// Checks `files`, in the order given. Only a file that can't be opened stops the run; everything else ends up in the
/// report.
pub fn run(files: &[InputFile], options: &Options) -> Result<Report, InputError> {
    run_with(files, options, &mut Quiet)
}

/// `run`, telling `observer` how it goes.
pub fn run_with(
    files: &[InputFile],
    options: &Options,
    observer: &mut dyn Observer,
) -> Result<Report, InputError> {
    run_inner(files, options, &[], observer)
}

/// The catalog the schema pass built from the files' DDL, with what the pass reported.
pub struct Schema {
    catalog: Catalog,
    report: Report,
}

impl Schema {
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

/// The schema pass of `run`, for `run_without` to check rows against as often as it is asked.
pub fn schema(
    files: &[InputFile],
    options: &Options,
    observer: &mut dyn Observer,
) -> Result<Schema, InputError> {
    read_schema(files, options, observer)
}

/// `run` with the schema already read, reading each file as if the rows inside its `removed` spans weren't there and
/// the values at its `nulled` spans were NULL. Both are by file position, each file's spans sorted and apart.
pub fn run_without(
    files: &[InputFile],
    options: &Options,
    schema: &Schema,
    removed: &[Vec<Span>],
    nulled: &[Vec<Span>],
    observer: &mut dyn Observer,
) -> Result<Report, InputError> {
    check_rows(files, options, schema, removed, nulled, observer)
}

/// Whether `span` lies inside one of the sorted, separate `cuts`.
pub fn is_removed(cuts: &[Span], span: Span) -> bool {
    let after = cuts.partition_point(|c| c.start <= span.start);
    after
        .checked_sub(1)
        .and_then(|i| cuts.get(i))
        .is_some_and(|c| span.end <= c.end)
}

fn run_inner(
    files: &[InputFile],
    options: &Options,
    removed: &[Vec<Span>],
    observer: &mut dyn Observer,
) -> Result<Report, InputError> {
    let schema = read_schema(files, options, observer)?;
    check_rows(files, options, &schema, removed, &[], observer)
}

/// The files a pass reads, and their bytes in all.
fn total(files: &[InputFile], pass: impl Fn(&InputFile) -> bool) -> (usize, u64) {
    let read: Vec<&InputFile> = files.iter().filter(|f| pass(f)).collect();
    (read.len(), read.iter().map(|f| f.bytes).sum::<u64>())
}

fn read_schema(
    files: &[InputFile],
    options: &Options,
    observer: &mut dyn Observer,
) -> Result<Schema, InputError> {
    let names: Vec<String> = files.iter().map(|f| display(&f.path)).collect();
    let reading = Reading {
        names: &names,
        options,
    };
    let mut report = Report::default();
    let schema_given = files.iter().any(|f| f.role == Role::Schema);
    let in_schema_pass = |f: &InputFile| !(schema_given && f.role == Role::Data);
    let mut builder = CatalogBuilder::new(options.dialect.rules(), options.default_schema.clone());
    let (count, bytes) = total(files, in_schema_pass);
    observer.pass(Pass::Schema, count, bytes);
    for (index, file) in files.iter().enumerate() {
        if !in_schema_pass(file) {
            continue;
        }
        read(
            &reading,
            file,
            index,
            Pass::Schema,
            &mut report,
            observer,
            |statement, origin| {
                builder.add(statement.into_owned(), origin);
            },
        )?;
    }
    let (catalog, issues) = builder.finish();
    report.schema = schema_counts(&catalog);
    report.referenced = referenced(&catalog);
    observer.schema(&catalog);
    report.catalog = issues
        .into_iter()
        .map(|Issue { origin, message }| problem(&names, origin.file, Some(origin.line), message))
        .collect();
    Ok(Schema { catalog, report })
}

fn check_rows(
    files: &[InputFile],
    options: &Options,
    schema: &Schema,
    removed: &[Vec<Span>],
    nulled: &[Vec<Span>],
    observer: &mut dyn Observer,
) -> Result<Report, InputError> {
    let names: Vec<String> = files.iter().map(|f| display(&f.path)).collect();
    let reading = Reading {
        names: &names,
        options,
    };
    let catalog = &schema.catalog;
    let mut report = schema.report.clone();
    let in_rows_pass = |f: &InputFile| f.role != Role::Schema;
    let mut checker = rows::Checker::new(catalog, options.dialect, options.load_order, &names);
    let mut not_followed = Vec::new();
    let mut unused = 0;
    let mut count = 0;
    let mut uncommitted = Vec::new();
    let (files_read, bytes) = total(files, in_rows_pass);
    observer.pass(Pass::Rows, files_read, bytes);
    for (index, file) in files.iter().enumerate() {
        if !in_rows_pass(file) {
            continue;
        }
        let reference = file.role == Role::Reference;
        let cuts = removed.get(index).map_or(&[][..], Vec::as_slice);
        let nulls = nulled.get(index).map_or(&[][..], Vec::as_slice);
        let mut open = Open::default();
        read(
            &reading,
            file,
            index,
            Pass::Rows,
            &mut report,
            observer,
            |statement, origin| match statement {
                Statement::Insert(mut inserted) => {
                    if !nulls.is_empty() {
                        let values = inserted.iter_mut().flat_map(|r| &mut r.values);
                        for value in values.filter(|v| nulls.binary_search(&v.span).is_ok()) {
                            value.expr = Expr::Null;
                        }
                    }
                    let file = names.get(index).map_or("", String::as_str);
                    let mut matched = Vec::with_capacity(inserted.len());
                    for row in inserted.iter().filter(|r| !is_removed(cuts, r.span)) {
                        count += 1;
                        let at = rows::At {
                            file: index,
                            offset: row.span.start,
                            line: row.line,
                        };
                        match resolve_row(catalog, row) {
                            Ok(resolved) => {
                                open.add(catalog, row);
                                matched.push((resolved, row, at));
                            }
                            Err(error) => {
                                open.add(catalog, row);
                                checker.structure(error.finding(catalog, row, file), reference);
                            }
                        }
                    }
                    checker.statement(&matched, options.encoding, reference);
                }
                Statement::Skipped(Skip::NotFollowed(what)) => {
                    not_followed.push(problem(&names, origin.file, Some(origin.line), what));
                }
                Statement::Commit => open = Open::default(),
                Statement::Skipped(Skip::Unused(_)) => unused += 1,
                _ => {}
            },
        )?;
        if options.commit_check && file.role == Role::Data {
            uncommitted.extend(open.finding(names.get(index).map_or("", String::as_str)));
        }
    }
    observer.resolve();
    let (mut findings, warnings) = checker.finish();
    findings.extend(uncommitted);
    sort_by_place(&mut findings, &names);
    report.findings = findings;
    report.warnings = warnings;
    report.not_followed = not_followed;
    report.unused = unused;
    report.rows = count;
    Ok(report)
}

/// The rows of a file since its last commit: how many, and the first one's line and table.
#[derive(Default)]
struct Open {
    rows: u64,
    first: Option<(u64, String)>,
}

impl Open {
    fn add(&mut self, catalog: &Catalog, row: &crate::statement::InsertRow) {
        self.rows += 1;
        if self.first.is_none() {
            self.first = Some((row.line, structure::table_name(catalog, row)));
        }
    }

    fn finding(self, file: &str) -> Option<Finding> {
        let (line, table) = self.first?;
        Some(Finding {
            kind: FindingKind::Uncommitted,
            constraint: None,
            table,
            columns: Vec::new(),
            file: Some(file.to_owned()),
            line: Some(line),
            key: Vec::new(),
            first: None,
            count: Some(self.rows),
            assumed: false,
        })
    }
}

/// By the file's position in the run, then line; findings without a place last.
fn sort_by_place(findings: &mut [Finding], names: &[String]) {
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
}

fn schema_counts(catalog: &Catalog) -> SchemaCounts {
    let keys = catalog
        .tables()
        .iter()
        .flat_map(|t| &t.constraints)
        .filter(|c| {
            matches!(
                c.kind,
                Kind::PrimaryKey { .. } | Kind::Unique { .. } | Kind::ForeignKey { .. }
            )
        });
    let mut counts = SchemaCounts {
        tables: catalog.tables().len(),
        ..SchemaCounts::default()
    };
    for key in keys {
        counts.keys += 1;
        counts.disabled += usize::from(!key.state.enabled);
        counts.deferred += usize::from(key.state.initially_deferred);
    }
    counts
}

fn referenced(catalog: &Catalog) -> BTreeSet<(String, Vec<String>)> {
    let mut keys = BTreeSet::new();
    for constraint in catalog.tables().iter().flat_map(|t| &t.constraints) {
        let Kind::ForeignKey {
            parent:
                Parent::Key {
                    table,
                    constraint: key,
                },
            ..
        } = &constraint.kind
        else {
            continue;
        };
        if !constraint.state.enabled {
            continue;
        }
        let Some(parent) = catalog.table(*table) else {
            continue;
        };
        let Some(Kind::PrimaryKey { columns } | Kind::Unique { columns, .. }) =
            parent.constraints.get(*key).map(|c| &c.kind)
        else {
            continue;
        };
        let names = columns
            .iter()
            .filter_map(|&c| parent.columns.get(c))
            .map(|c| c.name.clone())
            .collect();
        keys.insert((catalog.display(&parent.name), names));
    }
    keys
}

/// A path as the user gave it, with `/` between its parts on Windows too, so findings read the same everywhere.
pub fn display(path: &Path) -> String {
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

/// What every file read shares.
#[derive(Clone, Copy)]
struct Reading<'a> {
    names: &'a [String],
    options: &'a Options,
}

/// Reads one file's statements for `pass`, handing each to `take` with where it starts.
fn read(
    reading: &Reading,
    file: &InputFile,
    index: usize,
    pass: Pass,
    report: &mut Report,
    observer: &mut dyn Observer,
    mut take: impl FnMut(Statement, Origin),
) -> Result<(), InputError> {
    let Reading { names, options } = *reading;
    let opened = input::open(&file.path)?;
    observer.file(
        index,
        names.get(index).map_or("", String::as_str),
        file.bytes,
    );
    for piece in options
        .dialect
        .pieces(opened.reader, options.runner, opened.base_offset)
    {
        let piece: Piece = match piece {
            Ok(piece) => {
                observer.advance(piece.span.end);
                piece
            }
            Err(error) => {
                // Both passes read the file; the last one to read it reports it.
                if pass == Pass::Rows || file.role == Role::Schema {
                    report
                        .unreadable
                        .push(problem(names, index, None, error.to_string()));
                }
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
