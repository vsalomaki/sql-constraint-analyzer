//! The INSERT parser and value canonicalization against the committed exports: every row the generator or SQLcl
//! wrote parses, at the line the manifest names, and the manifest's keys come out of the parsed values.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sqlca::dialect::oracle::{RULES, eval, insert};
use sqlca::finding::{Finding, FindingKind};
use sqlca::source::Encoding;
use sqlca::statement::{DataType, InsertRow, Skip, Statement, TypeFamily};
use sqlca::value::{self, Canonical};

/// The parts of a generator manifest (`expected.json`) these tests read.
#[derive(Deserialize)]
struct Expected {
    options: Options,
    files: Vec<OutputFile>,
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
struct Options {
    encoding: Encoding,
}

#[derive(Deserialize)]
struct OutputFile {
    path: String,
}

const GENERATED: [&str; 3] = [
    "separate-sqldev",
    "single-all-crlf-1252",
    "type-after-data-shuffled",
];

fn generated(fixture: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gen/oracle")
        .join(fixture)
}

#[expect(
    clippy::unwrap_used,
    reason = "a malformed fixture fails the test that reads it"
)]
fn expected(root: &Path) -> Expected {
    serde_json::from_str(&fs::read_to_string(root.join("expected.json")).unwrap()).unwrap()
}

/// Every statement of `path` the INSERT parser reads, in file order; a parse error fails the test.
#[expect(
    clippy::panic,
    reason = "a statement that doesn't parse fails the test that read it"
)]
fn inserts(path: &Path, encoding: Encoding) -> Vec<Statement<'static>> {
    let mut statements = Vec::new();
    for piece in crate::split::pieces(path) {
        let starts_insert = piece
            .text
            .get(..6)
            .is_some_and(|head| head.eq_ignore_ascii_case(b"INSERT"));
        match insert::parse_insert(&piece, encoding) {
            Ok(Some(statement)) => statements.push(statement.into_owned()),
            Ok(None) => assert!(
                !starts_insert,
                "{}:{} not read as an INSERT",
                path.display(),
                piece.line
            ),
            Err(error) => panic!("{}: {error:?}", path.display()),
        }
    }
    statements
}

fn rows<'a>(statements: &'a [Statement<'static>]) -> Vec<&'a InsertRow<'static>> {
    statements
        .iter()
        .flat_map(|s| match s {
            Statement::Insert(rows) => rows.as_slice(),
            _ => &[],
        })
        .collect()
}

/// Rows as the generator writes them: a line starting `insert into` or (in an `INSERT ALL`) `into`.
#[expect(
    clippy::unwrap_used,
    reason = "an unreadable fixture fails the test that reads it"
)]
fn row_lines(path: &Path) -> usize {
    let bytes = fs::read(path).unwrap();
    String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| l.trim_start().to_ascii_lowercase())
        .filter(|l| l.starts_with("insert into ") || l.starts_with("into "))
        .count()
}

/// The generator's schema (`src/synth/schema.rs`): each table's columns in order, with their type families.
fn columns(table: &str) -> Option<&'static [(&'static str, TypeFamily)]> {
    use TypeFamily::{Date, FixedText, Number, Text, Timestamp};
    Some(match table {
        "REF.COUNTRIES" => &[("COUNTRY_CODE", FixedText), ("NAME", Text)],
        "APP.DEPARTMENTS" => &[
            ("DEPT_ID", Number),
            ("NAME", Text),
            ("MANAGER_ID", Number),
            ("COUNTRY_CODE", FixedText),
        ],
        "APP.EMPLOYEES" => &[
            ("EMP_ID", Number),
            ("FIRST_NAME", Text),
            ("LAST_NAME", Text),
            ("EMAIL", Text),
            ("DEPT_ID", Number),
            ("MANAGER_ID", Number),
            ("HIRE_DATE", Date),
            ("STATUS", Text),
            ("CREATED_AT", Timestamp),
            ("PHONE", Text),
        ],
        "APP.PROJECTS" => &[
            ("PROJECT_CODE", Text),
            ("NAME", Text),
            ("DEPT_NAME", Text),
            ("EXTERNAL_REF", Text),
            ("START_DATE", Date),
        ],
        "APP.ASSIGNMENTS" => &[
            ("EMP_ID", Number),
            ("PROJECT_CODE", Text),
            ("ROLE", Text),
            ("ASSIGNED_ON", Date),
        ],
        "APP.TIMESHEETS" => &[
            ("TS_ID", Number),
            ("EMP_ID", Number),
            ("PROJECT_CODE", Text),
            ("WORK_DATE", Date),
            ("HOURS", Number),
        ],
        "APP.LEGACY_NOTES" => &[("NOTE_ID", Number), ("EMP_ID", Number), ("NOTE_TEXT", Text)],
        "APP.\"AuditLog\"" => &[
            ("LOG_ID", Number),
            ("EMP_ID", Number),
            ("LOGGED_AT", Timestamp),
            ("DETAIL", Text),
        ],
        _ => return None,
    })
}

fn qualified(row: &InsertRow) -> String {
    let schema = row.table.schema.as_ref().map_or("", |s| &*s.value);
    RULES.qualified(schema, &row.table.name.value)
}

/// The canonical values of `names` in `row`, as a manifest key; `None` when a value can't be known.
fn key(row: &InsertRow, names: &[String], encoding: Encoding) -> Option<Vec<Option<String>>> {
    let table = columns(&qualified(row))?;
    names
        .iter()
        .map(|name| {
            let index = match &row.columns {
                Some(listed) => listed.iter().position(|c| c.value == *name),
                None => table.iter().position(|(c, _)| c == name),
            }?;
            let (_, family) = table.iter().find(|(c, _)| c == name)?;
            let literal = eval::evaluate(&row.values.get(index)?.expr, encoding);
            match value::canonical(&literal, &DataType::of(*family), &RULES) {
                Canonical::Null => Some(None),
                Canonical::Value(v) => Some(Some(v.into_owned())),
                Canonical::Unknown => None,
            }
        })
        .collect()
}

#[test]
fn every_generated_insert_parses_into_the_rows_and_lines_the_manifest_names() {
    for fixture in GENERATED {
        let root = generated(fixture);
        let expected = expected(&root);
        let encoding = expected.options.encoding;
        let mut by_line: BTreeMap<(String, u64), InsertRow> = BTreeMap::new();
        for file in &expected.files {
            let path = root.join(&file.path);
            let statements = inserts(&path, encoding);
            let rows = rows(&statements);
            assert_eq!(rows.len(), row_lines(&path), "{fixture}/{}", file.path);
            for row in rows {
                let previous = by_line.insert((file.path.clone(), row.line), row.clone());
                assert!(previous.is_none(), "{}:{} twice", file.path, row.line);
            }
        }
        let mut keys = 0;
        for finding in &expected.findings {
            let Some(at) = finding.file.clone().zip(finding.line) else {
                continue;
            };
            let first = finding.first.as_ref().map(|l| (l.file.clone(), l.line));
            for location in std::iter::once(&at).chain(first.as_ref()) {
                assert!(
                    by_line.contains_key(location),
                    "{fixture}: no row at {location:?} for {finding:?}"
                );
            }
            if matches!(
                finding.kind,
                FindingKind::DuplicateKey | FindingKind::FkMissingParent | FindingKind::FkLoadOrder
            ) {
                let row = &by_line[&at];
                assert_eq!(qualified(row), finding.table, "{fixture}: {finding:?}");
                assert_eq!(
                    key(row, &finding.columns, encoding).as_ref(),
                    Some(&finding.key),
                    "{fixture}: {finding:?}\n{row:?}"
                );
                keys += 1;
            }
        }
        assert!(keys > 5, "{fixture}: only {keys} keys checked");
    }
}

#[test]
fn every_sqlcl_data_file_parses_into_the_rows_oracle_counted() {
    for (table, count) in crate::sqlcl::counts() {
        let path = crate::sqlcl::data_file(&table);
        let statements = inserts(&path, Encoding::Utf8);
        let rows = rows(&statements);
        assert_eq!(
            (statements.len(), rows.len()),
            (count, count),
            "{}",
            path.display()
        );
        for row in rows {
            assert_eq!(qualified(row), table);
            let names: Vec<String> = columns(&table)
                .unwrap()
                .iter()
                .map(|(c, _)| (*c).to_owned())
                .collect();
            let key = key(row, &names, Encoding::Utf8);
            assert!(
                key.is_some(),
                "{}:{} has an unknown value",
                path.display(),
                row.line
            );
        }
    }
}

#[test]
fn every_sqlcl_file_parses_and_the_metadata_copy_is_not_followed() {
    let root = crate::sqlcl::root();
    let order = fs::read_to_string(root.join("order.txt")).unwrap();
    let mut copies = 0;
    for path in order.lines().filter(|l| l.contains("/tables/")) {
        for statement in inserts(&root.join(path), Encoding::Utf8) {
            assert!(
                matches!(&statement, Statement::Skipped(Skip::NotFollowed(what)) if what == "INSERT … SELECT"),
                "{path}: {statement:?}"
            );
            copies += 1;
        }
    }
    assert_eq!(copies, 8);
    for path in order.lines().filter(|l| !l.is_empty()) {
        inserts(&root.join(path), Encoding::Utf8);
    }
}
