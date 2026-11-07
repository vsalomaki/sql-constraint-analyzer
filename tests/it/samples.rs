//! `tests/fixtures/samples/oracle/` (`make sample-fixtures`) holds Oracle's HR and CO sample schemas: DDL and
//! INSERTs Oracle wrote, with no violations in them. Oracle Free loads the same files without an error and holds
//! the row counts below.

use std::fs;
use std::path::Path;

use sqlca::check::{self, Options, Report, Warning};
use sqlca::input::{InputFile, OrderSource, Role};

/// Checks a schema's create and populate scripts, in that order.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture that can't be read fails the test"
)]
fn check(schema: &str) -> Report {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/samples/oracle")
        .join(schema);
    let files: Vec<InputFile> = ["create", "populate"]
        .iter()
        .map(|part| {
            let path = root.join(format!("{schema}_{part}.sql"));
            InputFile {
                bytes: fs::metadata(&path).unwrap().len(),
                path,
                role: Role::Data,
                order: OrderSource::Given,
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

/// CO resets its identity columns at the end, which the catalog doesn't follow, and leaves INVENTORY's identity
/// column to Oracle, so those keys can't be checked.
#[test]
fn co_checks_clean_but_for_its_identity_columns() {
    let report = check("co");
    assert_read_whole(&report);
    assert_eq!(report.rows, 8_783);
    assert_eq!(
        (
            report.schema.tables,
            report.schema.keys,
            report.schema.disabled
        ),
        (7, 20, 0)
    );
    let reset: Vec<(Option<u64>, &str)> = report
        .catalog
        .iter()
        .map(|p| {
            (
                p.line,
                p.message.split(" MODIFY").next().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        reset,
        [
            (Some(8920), "ALTER TABLE PRODUCTS"),
            (Some(8926), "ALTER TABLE STORES"),
            (Some(8932), "ALTER TABLE CUSTOMERS"),
            (Some(8938), "ALTER TABLE ORDERS"),
            (Some(8944), "ALTER TABLE SHIPMENTS"),
            (Some(8950), "ALTER TABLE INVENTORY"),
        ]
    );
    assert!(
        report
            .catalog
            .iter()
            .all(|p| p.message.contains("AS IDENTITY") && p.message.ends_with(": not followed")),
        "{:?}",
        report.catalog
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
