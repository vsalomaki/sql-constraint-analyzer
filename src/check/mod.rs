//! The check: a schema pass builds the catalog from every file's DDL, then a rows pass matches each inserted row to
//! its table and runs the constraint checks on it. What a row can't settle on its own (a foreign key whose parent may
//! come later) is resolved once every row is in.

mod order;
mod rows;
mod structure;

use std::collections::BTreeSet;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

pub use order::{Ordered, dependency_order};
pub use structure::{Row, StructureError, resolve_row};

use crate::catalog::{Catalog, CatalogBuilder, CreatedTwice, Issue, Kind, Lookup, Origin, Parent};
use crate::dialect::{DialectId, ParseMemo, Pass, Runner};
use crate::finding::{Finding, FindingKind};
use crate::input::{self, InputError, InputFile, Role, RunPlan, Segment, Unit};
use crate::source::{Encoding, Piece, RunAt, Span};
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
    /// Which folder an unqualified table name's schema is named after, unless there is a default schema.
    pub folder_schema: FolderSchema,
}

/// Which folder, below the one all inputs share, names the schema of an unqualified table name in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum FolderSchema {
    /// The file's own folder: `a/x.sql` is in schema `A`.
    #[default]
    Immediate,
    /// The first folder below the shared one: `a/tables/x.sql` is in schema `A`.
    Top,
    /// None: an unqualified name matches a table of that name in any schema.
    Off,
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
            folder_schema: FolderSchema::default(),
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
    /// The child and parent tables of the enabled foreign keys, as findings name tables.
    pub parents: BTreeSet<(String, String)>,
    /// Rows with a finding, or a duplicated key's first row, that hold a key some child row found, by file and line:
    /// cutting one can leave a child without its parent.
    pub referenced_rows: BTreeSet<(String, u64)>,
    /// Tables with a key value the check couldn't know or assumed: cutting a row there can uncover a missing parent
    /// that no child found.
    pub uncertain_tables: BTreeSet<String>,
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
    /// The current pass starts reading `part` of the file at `index` in the run, which ends the part before. A part
    /// is the whole file, or the stretch between two of its include commands, which read their files in between;
    /// the file's last part ends at `bytes`.
    fn file(&mut self, _index: usize, _name: &str, _bytes: u64, _part: Span) {}
    /// The current part has been read up to `offset` in its file.
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
    let schema = schema(files, options, observer)?;
    run_without(files, options, &schema, &[], &[], observer)
}

/// The catalog the schema pass built from the files' DDL, with what the pass reported.
pub struct Schema {
    catalog: Catalog,
    report: Report,
    /// By file: whether the pass applied table DDL from it.
    ddl: Vec<bool>,
    plan: RunPlan,
}

impl Schema {
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

/// Whether `span` lies inside one of the sorted, separate `cuts`.
pub fn is_removed(cuts: &[Span], span: Span) -> bool {
    let after = cuts.partition_point(|c| c.start <= span.start);
    after
        .checked_sub(1)
        .and_then(|i| cuts.get(i))
        .is_some_and(|c| span.end <= c.end)
}

/// By file position, the schema named after the file's folder as `mode` picks it, below the folder all files share.
fn homes(
    files: &[InputFile],
    mode: FolderSchema,
    schema: impl Fn(&str) -> String,
) -> Vec<Option<String>> {
    let folders: Vec<PathBuf> = files
        .iter()
        // The real folder, as `fix` writes outputs after it: `a/../b` is `b`, and a link counts where it points.
        .map(|f| {
            let path = std::fs::canonicalize(&f.path)
                .or_else(|_| std::path::absolute(&f.path))
                .unwrap_or_else(|_| f.path.clone());
            path.parent().map(Path::to_path_buf).unwrap_or_default()
        })
        .collect();
    let shared = shared_folder(&folders);
    folders
        .iter()
        .map(|folder| {
            let mut below = folder.strip_prefix(&shared).ok()?.components();
            let part = match mode {
                FolderSchema::Immediate => below.next_back(),
                FolderSchema::Top => below.next(),
                FolderSchema::Off => None,
            };
            match part {
                Some(Component::Normal(name)) => name.to_str().map(&schema),
                _ => None,
            }
        })
        .collect()
}

/// The deepest folder that holds all of `folders`.
pub(crate) fn shared_folder(folders: &[PathBuf]) -> PathBuf {
    let mut folders = folders.iter();
    let Some(first) = folders.next() else {
        return PathBuf::new();
    };
    let mut shared: Vec<Component> = first.components().collect();
    for folder in folders {
        let same = shared
            .iter()
            .zip(folder.components())
            .take_while(|(a, b)| *a == b)
            .count();
        shared.truncate(same);
    }
    shared.iter().collect()
}

/// The files a pass reads, and their bytes in all.
fn total(files: &[InputFile], pass: impl Fn(&InputFile) -> bool) -> (usize, u64) {
    let read: Vec<&InputFile> = files.iter().filter(|f| pass(f)).collect();
    (read.len(), read.iter().map(|f| f.bytes).sum::<u64>())
}

/// The schema pass of `run`, for `run_without` to check rows against as often as it is asked.
pub fn schema(
    files: &[InputFile],
    options: &Options,
    observer: &mut dyn Observer,
) -> Result<Schema, InputError> {
    let names: Vec<String> = files.iter().map(|f| display(&f.path)).collect();
    let reading = Reading {
        files,
        names: &names,
        options,
    };
    let mut report = Report::default();
    let schema_given = files.iter().any(|f| f.role == Role::Schema);
    let in_schema_pass = |f: &InputFile| !(schema_given && f.role == Role::Data);
    let rules = options.dialect.rules();
    let homes = homes(files, options.folder_schema, |folder| {
        rules.folder_schema(folder)
    });
    let mut builder = CatalogBuilder::new(rules, options.default_schema.clone(), homes);
    let mut ddl = vec![false; files.len()];
    let plan = RunPlan::new(files);
    let (count, bytes) = total(files, in_schema_pass);
    observer.pass(Pass::Schema, count, bytes);
    for unit in plan.units() {
        if !files.get(unit.file).is_some_and(in_schema_pass) {
            continue;
        }
        read(
            &reading,
            &plan,
            unit,
            Pass::Schema,
            &mut report,
            observer,
            |statement, origin| {
                if let Some(applied) = ddl.get_mut(origin.file) {
                    *applied = true;
                }
                builder.add(statement.into_owned(), origin);
            },
        )?;
    }
    let (catalog, issues) = builder.finish().map_err(
        |CreatedTwice {
             table,
             first,
             again,
         }| {
            let place = |o: Origin| {
                format!(
                    "{}:{}",
                    names.get(o.file).map_or("", String::as_str),
                    o.line
                )
            };
            InputError::TableCreatedTwice {
                table,
                first: place(first),
                again: place(again),
            }
        },
    )?;
    report.schema = schema_counts(&catalog);
    report.referenced = referenced(&catalog);
    report.parents = parents(&catalog);
    observer.schema(&catalog);
    report.catalog = issues
        .into_iter()
        .map(|Issue { origin, message }| problem(&names, origin.file, Some(origin.line), message))
        .collect();
    Ok(Schema {
        catalog,
        report,
        ddl,
        plan,
    })
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
    let names: Vec<String> = files.iter().map(|f| display(&f.path)).collect();
    let reading = Reading {
        files,
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
    for unit in schema.plan.units() {
        let Some(top) = files.get(unit.file).filter(|f| in_rows_pass(f)) else {
            continue;
        };
        // A transaction runs through the unit: a commit in a script ends what its includes left open.
        let mut open = Open::default();
        read(
            &reading,
            &schema.plan,
            unit,
            Pass::Rows,
            &mut report,
            observer,
            |statement, origin| match statement {
                Statement::Insert(mut inserted) => {
                    let index = origin.file;
                    let reference = files.get(index).is_some_and(|f| f.role == Role::Reference);
                    let cuts = removed.get(index).map_or(&[][..], Vec::as_slice);
                    let nulls = nulled.get(index).map_or(&[][..], Vec::as_slice);
                    if !nulls.is_empty() {
                        let values = inserted.iter_mut().flat_map(|r| &mut r.values);
                        for value in values.filter(|v| nulls.binary_search(&v.span).is_ok()) {
                            value.expr = Expr::Null;
                        }
                    }
                    let file = names.get(index).map_or("", String::as_str);
                    let mut matched = Vec::with_capacity(inserted.len());
                    for row in &inserted {
                        if is_removed(cuts, row.span) {
                            if let Lookup::Found(table) = catalog.resolve(&row.table, index) {
                                checker.removed(table);
                            }
                            continue;
                        }
                        count += 1;
                        let at = rows::At {
                            file: index,
                            run: RunAt {
                                offset: row.span.start,
                                ..origin.run
                            },
                            line: row.line,
                        };
                        open.add(catalog, row, index, file);
                        match resolve_row(catalog, row, index) {
                            Ok(resolved) => matched.push((resolved, row, at)),
                            Err(error) => {
                                let finding = error.finding(catalog, row, file, index);
                                checker.structure(finding, reference);
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
        if options.commit_check && top.role == Role::Data {
            uncommitted.extend(open.finding());
        }
    }
    observer.resolve();
    let (mut findings, warnings, holders) = checker.finish();
    report.referenced_rows = holders.rows;
    report.uncertain_tables = holders.uncertain;
    findings.extend(uncommitted);
    sort_by_place(&mut findings, &names);
    report.findings = findings;
    report.warnings = warnings;
    report.not_followed = not_followed;
    report.unused = unused;
    report.rows = count;
    Ok(report)
}

/// The rows of a unit since its last commit: how many, and the first one's file, line and table.
#[derive(Default)]
struct Open {
    rows: u64,
    first: Option<(String, u64, String)>,
}

impl Open {
    fn add(
        &mut self,
        catalog: &Catalog,
        row: &crate::statement::InsertRow,
        index: usize,
        file: &str,
    ) {
        self.rows += 1;
        if self.first.is_none() {
            let table = structure::table_name(catalog, row, index);
            self.first = Some((file.to_owned(), row.line, table));
        }
    }

    fn finding(self) -> Option<Finding> {
        let (file, line, table) = self.first?;
        Some(Finding {
            kind: FindingKind::Uncommitted,
            constraint: None,
            table,
            columns: Vec::new(),
            file: Some(file),
            line: Some(line),
            key: Vec::new(),
            first: None,
            unchecked: Vec::new(),
            unchecked_more: false,
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
            parent: Parent::Key { table, columns, .. },
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
        keys.insert((catalog.display(&parent.name), parent.column_names(columns)));
    }
    keys
}

fn parents(catalog: &Catalog) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for child in catalog.tables() {
        for constraint in &child.constraints {
            let Kind::ForeignKey {
                parent: Parent::Key { table, .. },
                ..
            } = &constraint.kind
            else {
                continue;
            };
            if let Some(parent) = catalog.table(*table).filter(|_| constraint.state.enabled) {
                pairs.insert((catalog.display(&child.name), catalog.display(&parent.name)));
            }
        }
    }
    pairs
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
    files: &'a [InputFile],
    names: &'a [String],
    options: &'a Options,
}

/// Reads a unit's statements for `pass`, segment by segment in the order they run, handing each to `take` with where
/// it is. Each segment gets a splitter of its own: an include command is a piece of its own, so nothing is in progress
/// where one ends.
fn read(
    reading: &Reading,
    plan: &RunPlan,
    unit: &Unit,
    pass: Pass,
    report: &mut Report,
    observer: &mut dyn Observer,
    mut take: impl FnMut(Statement, Origin),
) -> Result<(), InputError> {
    let Reading {
        files,
        names,
        options,
    } = *reading;
    for position in unit.segments.clone() {
        let Some(&Segment {
            file: index,
            start,
            line,
            end,
        }) = plan.segment(position)
        else {
            continue;
        };
        let Some(file) = files.get(index) else {
            continue;
        };
        let opened = input::open(&file.path)?;
        let from = start.max(opened.base_offset);
        let mut reader = opened.reader;
        if from > opened.base_offset {
            reader
                .seek(SeekFrom::Start(from))
                .map_err(|source| InputError::Io {
                    path: file.path.clone(),
                    source,
                })?;
        }
        let part = Span {
            start,
            end: end.unwrap_or(file.bytes),
        };
        let name = names.get(index).map_or("", String::as_str);
        observer.file(index, name, file.bytes, part);
        let reader = reader.take(end.map_or(u64::MAX, |end| end.saturating_sub(from)));
        let mut memo = ParseMemo::new(options.dialect);
        for piece in options.dialect.pieces(reader, options.runner, from, line) {
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
            match options
                .dialect
                .parse_with(&piece, options.encoding, pass, &mut memo)
            {
                Ok(Some(statement)) => {
                    let origin = Origin {
                        file: index,
                        run: RunAt {
                            segment: position,
                            offset: piece.span.start,
                        },
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
    }
    Ok(())
}
