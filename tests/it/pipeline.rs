//! The neutral path from files to statements: open, split, parse and build the catalog through `DialectId` only,
//! as the checker will. Every committed export reads without an error, and every row names a table the catalog
//! knows, except the rows the manifest plants an unknown table in.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sqlca::catalog::{CatalogBuilder, Lookup, Origin};
use sqlca::dialect::{DialectId, Runner};
use sqlca::finding::{Finding, FindingKind};
use sqlca::input;
use sqlca::source::Encoding;
use sqlca::statement::{Skip, Statement};

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
    /// `(file, line)` of the rows planted with a table the schema doesn't have.
    unknown_tables: BTreeSet<(String, u64)>,
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
        let unknown_tables = expected
            .findings
            .iter()
            .filter(|f| f.kind == FindingKind::UnknownTable)
            .map(|f| (f.file.clone().unwrap(), f.line.unwrap()))
            .collect();
        found.push(Export {
            root,
            encoding: expected.options.encoding,
            unknown_tables,
        });
    }
    found.push(Export {
        root: fixtures.join("sqlcl"),
        encoding: Encoding::Utf8,
        unknown_tables: BTreeSet::new(),
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

#[test]
fn every_export_reads_into_a_catalog_and_rows_that_resolve() {
    for export in exports() {
        let at = export.root.display().to_string();
        let statements = statements(&export);
        let mut builder = CatalogBuilder::new(DialectId::Oracle.rules(), None);
        for (_, statement, origin) in &statements {
            builder.add(statement.clone(), *origin);
        }
        let (catalog, issues) = builder.finish();
        assert!(issues.is_empty(), "{at}: {issues:?}");

        let mut rows = 0;
        let mut unknown = BTreeSet::new();
        let mut not_followed = Vec::new();
        for (file, statement, _) in &statements {
            match statement {
                Statement::Insert(inserted) => {
                    for row in inserted {
                        rows += 1;
                        match catalog.resolve(&row.table) {
                            Lookup::Found(_) => {}
                            Lookup::Missing => {
                                unknown.insert((file.clone(), row.line));
                            }
                            Lookup::Ambiguous(_) => panic!("{at}: {file}:{} ambiguous", row.line),
                        }
                    }
                }
                Statement::Skipped(Skip::NotFollowed(what)) => not_followed.push(what.clone()),
                _ => {}
            }
        }
        assert!(rows > 0, "{at}");
        assert_eq!(unknown, export.unknown_tables, "{at}");
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
