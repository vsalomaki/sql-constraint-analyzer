//! `tests/fixtures/sqlcl/` (`make sqlcl-fixtures`) holds what Oracle kept of a generated export, written back out
//! by SQLcl the way SQL Developer exports: files the generator didn't write, with no violations in them.

use std::fs;
use std::path::{Path, PathBuf};

use sqlca::source::PieceKind;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sqlcl")
}

/// `counts.csv` as (`SCHEMA.TABLE`, rows), with SQLcl's CSV quoting removed.
#[expect(
    clippy::unwrap_used,
    reason = "a malformed fixture fails the test that reads it"
)]
fn counts() -> Vec<(String, usize)> {
    let csv = fs::read_to_string(root().join("counts.csv")).unwrap();
    csv.lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (name, rows) = line.rsplit_once(',').unwrap();
            let name = name
                .strip_prefix('"')
                .and_then(|n| n.strip_suffix('"'))
                .unwrap()
                .replace("\"\"", "\"");
            (name, rows.trim().parse().unwrap())
        })
        .collect()
}

#[test]
fn every_listed_file_exists() {
    let order = fs::read_to_string(root().join("order.txt")).unwrap();
    let paths: Vec<&str> = order.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(paths.len(), 26);
    for path in paths {
        assert!(root().join(path).is_file(), "{path} is listed but missing");
    }
}

#[test]
fn each_data_file_has_one_insert_per_row_oracle_counted() {
    let counts = counts();
    assert_eq!(counts.len(), 8, "{counts:?}");
    for (table, rows) in counts {
        let (schema, name) = table.split_once('.').unwrap();
        let file = root()
            .join(schema.to_ascii_lowercase())
            .join("data")
            .join(format!("{}.sql", name.replace('"', "")));
        let text = fs::read_to_string(&file).unwrap();
        let inserts = text
            .lines()
            .filter(|l| l.starts_with("Insert into "))
            .count();
        assert_eq!(inserts, rows, "{}", file.display());
        assert!(
            text.starts_with(&format!("REM INSERTING into {table}")),
            "{}",
            file.display()
        );
    }
}

#[test]
fn no_fixture_file_holds_an_error_spooled_by_sqlcl() {
    let order = fs::read_to_string(root().join("order.txt")).unwrap();
    for path in order.lines().filter(|l| !l.is_empty()) {
        let text = fs::read_to_string(root().join(path)).unwrap();
        assert!(
            !text
                .lines()
                .any(|l| l.starts_with("ORA-") || l.starts_with("SP2-") || l.starts_with("Error")),
            "{path} has an error in it"
        );
    }
}

#[test]
fn every_fixture_file_splits_cleanly_and_data_files_into_one_statement_per_row() {
    let order = fs::read_to_string(root().join("order.txt")).unwrap();
    for path in order.lines().filter(|l| !l.is_empty()) {
        let pieces = crate::split::pieces(&root().join(path));
        assert!(
            pieces
                .iter()
                .all(|p| !matches!(p.kind, PieceKind::Unterminated | PieceKind::RunAgain)),
            "{path}: {pieces:?}"
        );
    }
    for (table, rows) in counts() {
        let (schema, name) = table.split_once('.').unwrap();
        let path = root()
            .join(schema.to_ascii_lowercase())
            .join("data")
            .join(format!("{}.sql", name.replace('"', "")));
        let pieces = crate::split::pieces(&path);
        let of = |kind| pieces.iter().filter(|p| p.kind == kind).count();
        assert_eq!(
            (of(PieceKind::Command), of(PieceKind::Sql)),
            (2, rows),
            "{}",
            path.display()
        );
    }
}

#[test]
fn sqlcl_ddl_splits_into_its_statements() {
    let shape = |path: &str| -> Vec<(PieceKind, u64)> {
        crate::split::pieces(&root().join(path))
            .iter()
            .map(|p| (p.kind, p.line))
            .collect()
    };
    // CREATE TABLE, the primary key, and DBMS_METADATA's `INSERT … SELECT … FROM …@ KU$`.
    assert_eq!(
        shape("app/tables/EMPLOYEES.sql"),
        [
            (PieceKind::Sql, 2),
            (PieceKind::Sql, 20),
            (PieceKind::Sql, 26)
        ]
    );
    assert_eq!(
        shape("app/triggers/AUDITLOG_BI.sql"),
        [(PieceKind::Block, 2), (PieceKind::Sql, 9)]
    );
}
