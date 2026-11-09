//! The neutral path from files to rows, through `DialectId` only, as the checker takes it: every piece of a script
//! belongs to exactly one pass, and a row matches its table and columns by list or by table order.

use std::fs;
use std::path::Path;

use sqlca::catalog::{Catalog, CatalogBuilder, Origin};
use sqlca::check::{self, StructureError};
use sqlca::dialect::{DialectId, Pass, Runner};
use sqlca::finding::FindingKind;
use sqlca::input;
use sqlca::source::{Encoding, Piece, RunAt};
use sqlca::statement::{Expr, InsertRow, ParseError, Statement};

use crate::fixtures;

/// What the schema pass makes of a piece, or else the rows pass: each piece belongs to exactly one. The rows pass
/// sees the schema pass's DDL only as a commit.
#[expect(clippy::panic, reason = "a piece both passes take fails the test")]
fn both_passes(piece: &Piece, encoding: Encoding) -> Result<Statement<'static>, ParseError> {
    let schema = DialectId::Oracle.parse(piece, encoding, Pass::Schema)?;
    let rows = DialectId::Oracle.parse(piece, encoding, Pass::Rows)?;
    match (schema, rows) {
        (Some(statement), None | Some(Statement::Commit)) | (None, Some(statement)) => {
            Ok(statement.into_owned())
        }
        other => panic!("{}: {other:?}", piece.line),
    }
}

#[test]
fn every_piece_of_every_export_belongs_to_exactly_one_pass() {
    let mut exports: Vec<(std::path::PathBuf, Encoding)> = fixtures::generated()
        .into_iter()
        .map(|root| {
            let encoding = fixtures::manifest(&root).options.encoding;
            (root, encoding)
        })
        .collect();
    exports.push((
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sqlcl"),
        Encoding::Utf8,
    ));
    for (root, encoding) in exports {
        let order = fs::read_to_string(root.join("order.txt")).unwrap();
        let mut pieces = 0;
        for path in order.lines() {
            let opened = input::open(&root.join(path)).unwrap();
            for piece in
                DialectId::Oracle.pieces(opened.reader, Runner::SqlPlus, opened.base_offset, 1)
            {
                both_passes(&piece.unwrap(), encoding).unwrap();
                pieces += 1;
            }
        }
        assert!(pieces > 0, "{}", root.display());
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "a table created twice in a fixture fails the test"
)]
fn catalog(statements: &[(String, Statement<'static>, Origin)]) -> Catalog {
    let mut builder = CatalogBuilder::new(DialectId::Oracle.rules(), None, Vec::new());
    for (_, statement, origin) in statements {
        builder.add(statement.clone(), *origin);
    }
    let (catalog, issues) = builder.finish().unwrap();
    assert!(issues.is_empty(), "{issues:?}");
    catalog
}

/// The statements of a script given as text.
#[expect(
    clippy::unwrap_used,
    reason = "test SQL that doesn't parse fails the test"
)]
fn parse(sql: &str) -> Vec<(String, Statement<'static>, Origin)> {
    DialectId::Oracle
        .pieces(sql.as_bytes(), Runner::SqlPlus, 0, 1)
        .map(|piece| {
            let piece = piece.unwrap();
            let origin = Origin {
                file: 0,
                run: RunAt {
                    segment: 0,
                    offset: piece.span.start,
                },
                line: piece.line,
            };
            (
                "t.sql".to_owned(),
                both_passes(&piece, Encoding::Utf8).unwrap(),
                origin,
            )
        })
        .collect()
}

fn rows<'a>(statements: &'a [(String, Statement<'static>, Origin)]) -> Vec<&'a InsertRow<'static>> {
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
            check::resolve_row(&catalog, row, 0).map(|r| {
                r.values
                    .iter()
                    .map(|v| match v.map(|v| &v.expr) {
                        Some(Expr::Number(n)) => n.to_string(),
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
            let f = check::resolve_row(&catalog, row, 0)
                .unwrap_err()
                .finding(&catalog, row, "t.sql", 0);
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
