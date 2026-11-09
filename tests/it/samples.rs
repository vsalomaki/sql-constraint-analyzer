//! `tests/fixtures/samples/oracle/` (`make sample-fixtures`) holds Oracle's HR and CO sample schemas: DDL and
//! INSERTs Oracle wrote, with no violations in them. Oracle Free loads the same files without an error and holds
//! the row counts below. `sqlcl/` (`make sample-sqlcl-fixtures`) holds what it loaded, exported again by SQLcl in
//! SQL Developer's separate-files layout. The rows `hr_plants.sql` and `co_plants.sql` plant after the data are
//! checked with the generator's exports, against `expected.json`.

use std::fs;
use std::path::{Path, PathBuf};

use sqlca::check::{self, Options, Report, Warning};
use sqlca::finding::FindingKind;
use sqlca::input::{InputFile, OrderSource, Role};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/samples/oracle")
}

/// Checks a schema's create and populate scripts, in that order.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that can't be read fails the test"
)]
fn check(schema: &str) -> Report {
    let root = root().join(schema);
    let files: Vec<InputFile> = ["create", "populate"]
        .iter()
        .map(|part| {
            let path = root.join(format!("{schema}_{part}.sql"));
            InputFile {
                bytes: fs::metadata(&path).unwrap().len(),
                path,
                role: Role::Data,
                order: OrderSource::Given,
                includes: Vec::new(),
            }
        })
        .collect();
    check::run(&files, &Options::default()).unwrap()
}

#[track_caller]
fn assert_read_whole(report: &Report) {
    assert!(report.findings.is_empty(), "{:#?}", report.findings);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.unreadable.is_empty(), "{:?}", report.unreadable);
    assert!(report.not_followed.is_empty(), "{:?}", report.not_followed);
}

#[test]
fn hr_checks_clean() {
    let report = check("hr");
    assert_read_whole(&report);
    assert!(report.catalog.is_empty(), "{:?}", report.catalog);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.rows, 216);
    assert_eq!(
        (
            report.schema.tables,
            report.schema.keys,
            report.schema.disabled
        ),
        (7, 18, 0)
    );
}

/// CO leaves INVENTORY's identity column to Oracle, so its key can't be checked. Its identity resets at the end
/// change only the next generated values.
#[test]
fn co_checks_clean_but_for_the_keys_oracle_generates() {
    let report = check("co");
    assert_read_whole(&report);
    assert!(report.catalog.is_empty(), "{:?}", report.catalog);
    assert_eq!(report.rows, 8_783);
    assert_eq!(
        (
            report.schema.tables,
            report.schema.keys,
            report.schema.disabled
        ),
        (7, 20, 0)
    );
    let [
        Warning::Unverifiable {
            table,
            constraint,
            count,
            ..
        },
    ] = report.warnings.as_slice()
    else {
        panic!("{:?}", report.warnings);
    };
    assert_eq!(
        (table.as_str(), constraint.as_deref(), *count),
        ("INVENTORY", Some("INVENTORY_PK"), 566)
    );
}

/// `counts.csv` as (`SCHEMA.TABLE`, rows).
#[expect(
    clippy::unwrap_used,
    reason = "a malformed fixture fails the test that reads it"
)]
fn counts(root: &Path) -> Vec<(String, u64)> {
    fs::read_to_string(root.join("counts.csv"))
        .unwrap()
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (name, rows) = line.rsplit_once(',').unwrap();
            (
                name.trim_matches('"').to_owned(),
                rows.trim().parse().unwrap(),
            )
        })
        .collect()
}

/// SQLcl writes no COMMIT into its data files, and `DBMS_METADATA` ends each table's DDL with an
/// `INSERT … SELECT … FROM …@ KU$`; nothing else is wrong, and every row Oracle held is read.
#[test]
fn the_sqlcl_exports_hold_every_row_and_miss_only_a_commit_in_each_data_file() {
    for (schema, rows) in [("hr", 216), ("co", 8_783)] {
        let root = root().join("sqlcl").join(schema);
        let counts = counts(&root);
        assert_eq!(counts.len(), 7, "{schema}: {counts:?}");
        assert_eq!(counts.iter().map(|(_, n)| n).sum::<u64>(), rows, "{schema}");
        for (table, n) in &counts {
            let name = table.split_once('.').unwrap().1;
            let text = fs::read_to_string(root.join("data").join(format!("{name}.sql"))).unwrap();
            let inserts = text
                .lines()
                .filter(|l| l.starts_with("Insert into "))
                .count();
            assert_eq!(inserts as u64, *n, "{schema} {table}");
        }

        let report = check::run(&crate::fixtures::inputs(&root), &Options::default()).unwrap();
        assert!(report.errors.is_empty(), "{schema}: {:?}", report.errors);
        assert!(
            report.unreadable.is_empty(),
            "{schema}: {:?}",
            report.unreadable
        );
        assert!(report.catalog.is_empty(), "{schema}: {:?}", report.catalog);
        assert!(
            report.warnings.is_empty(),
            "{schema}: {:?}",
            report.warnings
        );
        assert_eq!(report.rows, rows, "{schema}");
        assert_eq!(
            (report.schema.tables, report.schema.disabled),
            (7, 0),
            "{schema}"
        );
        assert!(
            report.not_followed.len() == 7
                && report.not_followed.iter().all(|p| {
                    p.message == "INSERT … SELECT"
                        && Path::new(&p.file).parent().unwrap().ends_with("tables")
                }),
            "{schema}: {:?}",
            report.not_followed
        );
        let mut files: Vec<String> = report
            .findings
            .iter()
            .map(|f| {
                assert_eq!(f.kind, FindingKind::Uncommitted, "{schema}: {f:?}");
                Path::new(f.file.as_deref().unwrap())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        files.sort();
        let mut tables: Vec<String> = counts
            .iter()
            .map(|(t, _)| format!("{}.sql", t.split_once('.').unwrap().1))
            .collect();
        tables.sort();
        assert_eq!(files, tables, "{schema}: one finding per data file");
    }
}
