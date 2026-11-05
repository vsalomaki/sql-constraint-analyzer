//! The neutral path from files to rows: open, split, parse, build the catalog and match rows to tables through
//! `DialectId` only, as the checker will. Every committed export reads without an error, and its structure findings
//! are exactly the ones its manifest plants.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sqlca::catalog::{Catalog, CatalogBuilder, Origin};
use sqlca::check::{self, StructureError};
use sqlca::dialect::{DialectId, Runner};
use sqlca::finding::{Finding, FindingKind};
use sqlca::input;
use sqlca::source::Encoding;
use sqlca::statement::{Expr, InsertRow, Skip, Statement};

#[derive(Deserialize)]
struct Expected {
    options: Options,
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
struct Options {
    encoding: Encoding,
}

struct Export {
    root: PathBuf,
    encoding: Encoding,
    /// The structure findings the manifest plants.
    structure: BTreeSet<Finding>,
}

#[expect(clippy::unwrap_used, reason = "a malformed fixture fails the test")]
fn exports() -> Vec<Export> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut found = Vec::new();
    for fixture in [
        "separate-sqldev",
        "single-all-crlf-1252",
        "type-after-data-shuffled",
    ] {
        let root = fixtures.join("gen/oracle").join(fixture);
        let expected: Expected =
            serde_json::from_str(&fs::read_to_string(root.join("expected.json")).unwrap()).unwrap();
        let structure = expected
            .findings
            .into_iter()
            .filter(|f| {
                matches!(
                    f.kind,
                    FindingKind::UnknownTable
                        | FindingKind::UnknownColumn
                        | FindingKind::ValueCountMismatch
                )
            })
            .collect();
        found.push(Export {
            root,
            encoding: expected.options.encoding,
            structure,
        });
    }
    found.push(Export {
        root: fixtures.join("sqlcl"),
        encoding: Encoding::Utf8,
        structure: BTreeSet::new(),
    });
    found
}

/// Every statement of the export in `order.txt` order, with its file and origin.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that can't be read fails the test"
)]
#[expect(
    clippy::panic,
    reason = "a statement that doesn't parse fails the test"
)]
fn statements(export: &Export) -> Vec<(String, Statement, Origin)> {
    let dialect = DialectId::Oracle;
    let order = fs::read_to_string(export.root.join("order.txt")).unwrap();
    let mut out = Vec::new();
    for (file, path) in order.lines().enumerate() {
        let opened = input::open(&export.root.join(path)).unwrap();
        for piece in dialect.pieces(opened.reader, Runner::SqlPlus, opened.base_offset) {
            let piece = piece.unwrap();
            let statement = dialect
                .parse(&piece, export.encoding)
                .unwrap_or_else(|e| panic!("{path}:{}: {}", e.line, e.message));
            let origin = Origin {
                file,
                offset: piece.span.start,
                line: piece.line,
            };
            out.push((path.to_owned(), statement, origin));
        }
    }
    out
}

fn catalog(statements: &[(String, Statement, Origin)]) -> Catalog {
    let mut builder = CatalogBuilder::new(DialectId::Oracle.rules(), None);
    for (_, statement, origin) in statements {
        builder.add(statement.clone(), *origin);
    }
    let (catalog, issues) = builder.finish();
    assert!(issues.is_empty(), "{issues:?}");
    catalog
}

#[test]
fn every_export_reads_into_a_catalog_with_exactly_the_planted_structure_findings() {
    for export in exports() {
        let at = export.root.display().to_string();
        let statements = statements(&export);
        let catalog = catalog(&statements);
        let mut rows = 0;
        let mut found = BTreeSet::new();
        let mut not_followed = Vec::new();
        for (file, statement, _) in &statements {
            match statement {
                Statement::Insert(inserted) => {
                    for row in inserted {
                        rows += 1;
                        if let Err(error) = check::resolve_row(&catalog, row) {
                            found.insert(error.finding(&catalog, row, file));
                        }
                    }
                }
                Statement::Skipped(Skip::NotFollowed(what)) => not_followed.push(what.clone()),
                _ => {}
            }
        }
        assert!(rows > 0, "{at}");
        assert_eq!(found, export.structure, "{at}");
        // DBMS_METADATA's `INSERT … SELECT … FROM …@ KU$` copies, one per table; nothing else is skipped as
        // changing data.
        let copies = if at.ends_with("sqlcl") { 8 } else { 0 };
        assert_eq!(
            not_followed,
            vec!["INSERT … SELECT".to_owned(); copies],
            "{at}"
        );
    }
}

/// The statements of a script given as text.
#[expect(
    clippy::unwrap_used,
    reason = "test SQL that doesn't parse fails the test"
)]
fn parse(sql: &str) -> Vec<(String, Statement, Origin)> {
    DialectId::Oracle
        .pieces(sql.as_bytes(), Runner::SqlPlus, 0)
        .map(|piece| {
            let piece = piece.unwrap();
            let origin = Origin {
                file: 0,
                offset: piece.span.start,
                line: piece.line,
            };
            (
                "t.sql".to_owned(),
                DialectId::Oracle.parse(&piece, Encoding::Utf8).unwrap(),
                origin,
            )
        })
        .collect()
}

fn rows(statements: &[(String, Statement, Origin)]) -> Vec<&InsertRow> {
    statements
        .iter()
        .filter_map(|(_, s, _)| match s {
            Statement::Insert(rows) => Some(rows),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Each row's values by table column (`-` where left out), or its structure error.
fn matched(sql: &str) -> Vec<Result<Vec<String>, StructureError>> {
    let statements = parse(sql);
    let catalog = catalog(&statements);
    rows(&statements)
        .into_iter()
        .map(|row| {
            check::resolve_row(&catalog, row).map(|r| {
                r.values
                    .iter()
                    .map(|v| match v.map(|v| &v.expr) {
                        Some(Expr::Number(n)) => n.clone(),
                        Some(other) => format!("{other:?}"),
                        None => "-".to_owned(),
                    })
                    .collect()
            })
        })
        .collect()
}

#[test]
fn values_go_to_their_columns_by_list_or_by_table_order() {
    let ddl =
        "CREATE TABLE T (A NUMBER, H NUMBER INVISIBLE, B NUMBER);\nALTER TABLE T ADD (C NUMBER);\n";
    let cases = [
        (
            "INSERT INTO T (B, A) VALUES (2, 1);",
            Ok(vec!["1", "-", "2", "-"]),
        ),
        (
            "INSERT INTO t (\"C\") VALUES (3);",
            Ok(vec!["-", "-", "-", "3"]),
        ),
        (
            "INSERT INTO T (H) VALUES (9);",
            Ok(vec!["-", "9", "-", "-"]),
        ),
        (
            "INSERT INTO T VALUES (1, 2, 3);",
            Ok(vec!["1", "-", "2", "3"]),
        ),
        (
            "INSERT INTO T VALUES (1, 2);",
            Err(StructureError::ValueCount {
                columns: 3,
                values: 2,
            }),
        ),
        (
            "INSERT INTO T (A, B) VALUES (1, 2, 3);",
            Err(StructureError::ValueCount {
                columns: 2,
                values: 3,
            }),
        ),
        (
            "INSERT INTO T (A, X, \"b\") VALUES (1, 2, 3);",
            Err(StructureError::UnknownColumns(vec!["X".into(), "b".into()])),
        ),
        (
            "INSERT INTO U VALUES (1);",
            Err(StructureError::UnknownTable),
        ),
    ];
    for (insert, expected) in cases {
        let expected = expected.map(|v| v.into_iter().map(str::to_owned).collect::<Vec<_>>());
        assert_eq!(matched(&format!("{ddl}{insert}\n")), [expected], "{insert}");
    }
}

#[test]
fn an_unqualified_name_in_several_schemas_is_ambiguous_and_named_as_written() {
    let statements = parse(
        "CREATE TABLE A.T (X NUMBER);\nCREATE TABLE B.T (X NUMBER);\nINSERT INTO t VALUES (1);\nINSERT INTO C.\"t\" VALUES (1);\n",
    );
    let catalog = catalog(&statements);
    let findings: Vec<(FindingKind, String, Option<u64>)> = rows(&statements)
        .into_iter()
        .map(|row| {
            let f = check::resolve_row(&catalog, row)
                .unwrap_err()
                .finding(&catalog, row, "t.sql");
            (f.kind, f.table, f.line)
        })
        .collect();
    assert_eq!(
        findings,
        [
            (FindingKind::AmbiguousTable, "T".to_owned(), Some(3)),
            (FindingKind::UnknownTable, "C.\"t\"".to_owned(), Some(4)),
        ]
    );
}
