//! The checker against the generator's manifests: what it finds in an export is exactly what the generator planted,
//! in end-state mode (all but load-order findings) and with `--load-order` (all of them). Files are read in the
//! export's `order.txt` order, `ref/` as reference files, as a user would pass them.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sqlca::check::{self, Options, Report};
use sqlca::finding::{Finding, FindingKind};
use sqlca::input::{InputFile, OrderSource, Role};
use sqlca::source::Encoding;

#[derive(Deserialize)]
struct Expected {
    options: ExpectedOptions,
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
struct ExpectedOptions {
    encoding: Encoding,
}

/// The export's files in `order.txt` order, `ref/` ones as reference files.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture without order.txt fails the test"
)]
fn inputs(root: &Path) -> Vec<InputFile> {
    fs::read_to_string(root.join("order.txt"))
        .unwrap()
        .lines()
        .map(|path| InputFile {
            path: root.join(path),
            role: if path.starts_with("ref/") {
                Role::Reference
            } else {
                Role::Data
            },
            bytes: 0,
            order: OrderSource::Given,
        })
        .collect()
}

/// Runs the check on an export and returns its findings with paths relative to the export, as the manifest has them.
#[expect(
    clippy::unwrap_used,
    reason = "an export that can't be read fails the test"
)]
fn check(root: &Path, encoding: Encoding, load_order: bool) -> (Report, BTreeSet<Finding>) {
    let options = Options {
        encoding,
        load_order,
        ..Options::default()
    };
    let report = check::run(&inputs(root), &options).unwrap();
    let relative = |file: &str| {
        Path::new(file)
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    let findings = report
        .findings
        .iter()
        .cloned()
        .map(|mut f| {
            f.file = f.file.as_deref().map(relative);
            if let Some(first) = &mut f.first {
                first.file = relative(&first.file);
            }
            f
        })
        .collect();
    (report, findings)
}

/// The manifest's findings a mode reports: load-order ones only with `--load-order`.
fn expected(findings: &[Finding], load_order: bool) -> BTreeSet<Finding> {
    findings
        .iter()
        .filter(|f| load_order || f.kind != FindingKind::FkLoadOrder)
        .cloned()
        .collect()
}

#[track_caller]
fn assert_clean(report: &Report, at: &str) {
    assert!(report.errors.is_empty(), "{at}: {:?}", report.errors);
    assert!(report.catalog.is_empty(), "{at}: {:?}", report.catalog);
}

/// Findings in one set but not the other, for a readable failure.
fn difference(found: &BTreeSet<Finding>, expected: &BTreeSet<Finding>) -> String {
    let missing: Vec<_> = expected.difference(found).collect();
    let extra: Vec<_> = found.difference(expected).collect();
    format!("missing {missing:#?}\nextra {extra:#?}")
}

fn committed() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gen/oracle");
    [
        "separate-sqldev",
        "single-all-crlf-1252",
        "type-after-data-shuffled",
    ]
    .iter()
    .map(|f| root.join(f))
    .collect()
}

#[test]
fn committed_exports_give_exactly_the_planted_findings_in_both_modes() {
    for root in committed() {
        let manifest: Expected =
            serde_json::from_str(&fs::read_to_string(root.join("expected.json")).unwrap()).unwrap();
        for load_order in [false, true] {
            let at = format!("{} load_order={load_order}", root.display());
            let (report, found) = check(&root, manifest.options.encoding, load_order);
            assert_clean(&report, &at);
            let expected = expected(&manifest.findings, load_order);
            assert!(found == expected, "{at}\n{}", difference(&found, &expected));
        }
    }
}

#[test]
fn the_sqlcl_export_has_no_findings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sqlcl");
    let (report, found) = check(&root, Encoding::Utf8, true);
    assert_clean(&report, "sqlcl");
    assert!(found.is_empty(), "{found:#?}");
    assert_eq!(report.not_followed.len(), 8);
}

#[cfg(feature = "gen")]
mod generated {
    use sqlca::synth::{self, Layout, Noise, Placement};

    use super::{assert_clean, check, difference, expected};

    /// Generates an export and checks it in both modes.
    #[expect(clippy::unwrap_used, reason = "a failed generation fails the test")]
    fn assert_manifest(options: &synth::Options) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("export");
        let manifest = synth::generate(options, &root).unwrap();
        for load_order in [false, true] {
            let at = format!("{options:?} load_order={load_order}");
            let (report, found) = check(&root, options.encoding, load_order);
            assert_clean(&report, &at);
            let expected = expected(&manifest.findings, load_order);
            assert!(found == expected, "{at}\n{}", difference(&found, &expected));
        }
    }

    fn base(seed: u64) -> synth::Options {
        synth::Options {
            seed,
            rows: 40,
            ..synth::Options::default()
        }
    }

    #[test]
    fn seeds_layouts_and_placements() {
        for seed in 1..=4 {
            for layout in [Layout::Single, Layout::Separate, Layout::Type] {
                for constraints in [Placement::BeforeData, Placement::AfterData] {
                    assert_manifest(&synth::Options {
                        layout,
                        constraints,
                        ..base(seed)
                    });
                }
            }
        }
    }

    #[test]
    fn every_noise_switch_and_none() {
        for noise in [Noise::all(), Noise::default(), Noise::sqldev()] {
            assert_manifest(&synth::Options { noise, ..base(7) });
        }
    }

    #[test]
    fn child_files_before_their_parents_are_load_order_findings() {
        for constraints in [Placement::BeforeData, Placement::AfterData] {
            assert_manifest(&synth::Options {
                shuffle_order: true,
                layout: Layout::Separate,
                constraints,
                ..base(3)
            });
        }
    }

    #[test]
    fn a_parent_without_rows_is_one_finding_per_foreign_key() {
        assert_manifest(&synth::Options {
            omit_data: vec!["APP.PROJECTS".to_owned()],
            ..base(5)
        });
    }

    #[test]
    fn without_deferred_violations() {
        assert_manifest(&synth::Options {
            deferred_violations: false,
            violations_ppm: 50_000,
            ..base(9)
        });
    }
}

/// The findings and report of a script given as text, as one data file.
#[expect(
    clippy::unwrap_used,
    reason = "a script that can't be written or read fails the test"
)]
fn check_sql(sql: &str, load_order: bool) -> (Report, Vec<(FindingKind, u64, bool)>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sql");
    fs::write(&path, sql).unwrap();
    let files = [InputFile {
        path,
        role: Role::Data,
        bytes: 0,
        order: OrderSource::Given,
    }];
    let options = Options {
        load_order,
        ..Options::default()
    };
    let report = check::run(&files, &options).unwrap();
    assert_clean(&report, sql);
    let findings = report
        .findings
        .iter()
        .map(|f| (f.kind, f.line.unwrap_or(0), f.assumed))
        .collect();
    (report, findings)
}

const PARENT_AND_CHILD: &str =
    "CREATE TABLE P (ID NUMBER PRIMARY KEY);\nCREATE TABLE C (P_ID NUMBER);\n";

#[test]
fn a_parent_key_the_checker_cant_know_makes_a_missing_parent_unverifiable() {
    let (report, findings) = check_sql(
        &format!(
            "{PARENT_AND_CHILD}ALTER TABLE C ADD CONSTRAINT C_FK FOREIGN KEY (P_ID) REFERENCES P;\n\
             INSERT INTO P VALUES (S.NEXTVAL);\nINSERT INTO C VALUES (1);\n"
        ),
        false,
    );
    assert_eq!(findings, []);
    let unverifiable: Vec<_> = report
        .warnings
        .iter()
        .filter_map(|w| match w {
            check::Warning::Unverifiable {
                constraint, count, ..
            } => Some((constraint.clone(), *count)),
            _ => None,
        })
        .collect();
    assert!(
        unverifiable.contains(&(Some("C_FK".to_owned()), 1)),
        "{:?}",
        report.warnings
    );
}

#[test]
fn rows_before_an_enable_novalidate_are_not_judged_by_it() {
    let script = |enable: &str| {
        format!(
            "{PARENT_AND_CHILD}ALTER TABLE C ADD CONSTRAINT C_FK FOREIGN KEY (P_ID) REFERENCES P DISABLE;\n\
             INSERT INTO P VALUES (9);\nINSERT INTO C VALUES (1);\nALTER TABLE C {enable} CONSTRAINT C_FK;\n\
             INSERT INTO C VALUES (2);\n"
        )
    };
    let (_, findings) = check_sql(&script("ENABLE NOVALIDATE"), true);
    assert_eq!(findings, [(FindingKind::FkMissingParent, 7, false)]);
    let (_, findings) = check_sql(&script("ENABLE"), true);
    assert_eq!(
        findings,
        [
            (FindingKind::FkMissingParent, 5, false),
            (FindingKind::FkMissingParent, 7, false)
        ]
    );
}

#[test]
fn an_insert_all_is_checked_as_one_statement() {
    let ddl =
        "CREATE TABLE P (ID NUMBER PRIMARY KEY);\nCREATE TABLE C (P_ID NUMBER REFERENCES P);\n";
    let (_, findings) = check_sql(
        &format!("{ddl}INSERT ALL INTO C VALUES (1) INTO P VALUES (1) SELECT 1 FROM DUAL;\n"),
        true,
    );
    assert_eq!(findings, []);
    let (_, findings) = check_sql(
        &format!("{ddl}INSERT INTO C VALUES (1);\nINSERT INTO P VALUES (1);\n"),
        true,
    );
    assert_eq!(findings, [(FindingKind::FkLoadOrder, 3, false)]);
}

#[test]
fn a_duplicate_of_a_date_read_from_text_is_assumed_whichever_row_came_first() {
    let ddl = "CREATE TABLE T (D DATE PRIMARY KEY);\n";
    for (first, second) in [
        ("'2024-01-02'", "DATE '2024-01-02'"),
        ("DATE '2024-01-02'", "'2024-01-02'"),
    ] {
        let (_, findings) = check_sql(
            &format!("{ddl}INSERT INTO T VALUES ({first});\nINSERT INTO T VALUES ({second});\n"),
            false,
        );
        assert_eq!(
            findings,
            [(FindingKind::DuplicateKey, 3, true)],
            "{first} then {second}"
        );
    }
}

#[test]
fn null_into_an_identity_by_default_on_null_column_takes_a_generated_value() {
    let (_, findings) = check_sql(
        "CREATE TABLE T (ID NUMBER GENERATED BY DEFAULT ON NULL AS IDENTITY NOT NULL, X NUMBER);\n\
         INSERT INTO T VALUES (NULL, 1);\nINSERT INTO T (X) VALUES (2);\n",
        false,
    );
    assert_eq!(findings, []);
}

#[test]
fn a_unique_index_keeps_judging_rows_a_later_novalidate_constraint_does_not() {
    let (_, findings) = check_sql(
        "CREATE TABLE T (ID NUMBER);\nCREATE UNIQUE INDEX T_UIX ON T (ID);\n\
         INSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\n\
         ALTER TABLE T ADD CONSTRAINT T_UK UNIQUE (ID) USING INDEX T_UIX ENABLE NOVALIDATE;\n",
        false,
    );
    assert_eq!(findings, [(FindingKind::DuplicateKey, 4, false)]);
}

#[test]
fn a_missing_parent_among_dates_read_from_text_is_assumed() {
    let (_, findings) = check_sql(
        "CREATE TABLE P (D DATE PRIMARY KEY);\nCREATE TABLE C (D DATE REFERENCES P);\n\
         INSERT INTO P VALUES ('2024-01-02');\nINSERT INTO C VALUES (DATE '2024-02-01');\n",
        false,
    );
    assert_eq!(findings, [(FindingKind::FkMissingParent, 4, true)]);
}

#[test]
fn a_parent_found_later_is_no_load_order_finding_when_an_earlier_parent_key_was_unknown() {
    let (_, findings) = check_sql(
        &format!(
            "{PARENT_AND_CHILD}ALTER TABLE C ADD CONSTRAINT C_FK FOREIGN KEY (P_ID) REFERENCES P;\n\
             INSERT INTO P VALUES (S.NEXTVAL);\nINSERT INTO C VALUES (1);\nINSERT INTO P VALUES (1);\n"
        ),
        true,
    );
    assert_eq!(findings, []);
}

#[test]
fn keys_compare_as_a_decimal_column_stores_them() {
    let (_, findings) = check_sql(
        "CREATE TABLE T (ID NUMBER(5,0) PRIMARY KEY);\nINSERT INTO T VALUES (1.1);\nINSERT INTO T VALUES (1.2);\n",
        false,
    );
    assert_eq!(findings, [(FindingKind::DuplicateKey, 3, false)]);
    // The child column stores 1.4 as 1 in NUMBER(5,0), which matches; NUMBER(5,1) keeps 1.4, which doesn't.
    let (_, findings) = check_sql(
        "CREATE TABLE P (ID NUMBER(5,0) PRIMARY KEY);\nCREATE TABLE A (P_ID NUMBER(5,0) REFERENCES P);\n\
         CREATE TABLE B (P_ID NUMBER(5,1) REFERENCES P);\nINSERT INTO P VALUES (1);\n\
         INSERT INTO A VALUES (1.4);\nINSERT INTO B VALUES (1.4);\n",
        false,
    );
    assert_eq!(findings, [(FindingKind::FkMissingParent, 6, false)]);
}

#[test]
fn a_wildcard_or_integer_number_has_no_precision_limit() {
    for column in ["NUMBER(*,0)", "INTEGER"] {
        let (_, findings) = check_sql(
            &format!(
                "CREATE TABLE T (ID {column} PRIMARY KEY);\nINSERT INTO T VALUES (1E38);\nINSERT INTO T VALUES (1E38);\n"
            ),
            false,
        );
        assert_eq!(
            findings,
            [(FindingKind::DuplicateKey, 3, false)],
            "{column}"
        );
    }
}
