//! The checker against the generator's manifests: what it finds in an export is exactly what the generator planted,
//! in end-state mode (all but load-order findings) and with `--load-order` (all of them). Files are read in the
//! export's `order.txt` order, `ref/` as reference files, as a user would pass them.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use sqlca::check::{self, Options, Report};
use sqlca::finding::{Finding, FindingKind};
use sqlca::input::{InputFile, OrderSource, Role};
use sqlca::source::Encoding;

use crate::fixtures::{self, inputs};

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
            for earlier in f.first.iter_mut().chain(&mut f.unchecked) {
                earlier.file = relative(&earlier.file);
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

#[test]
fn committed_exports_give_exactly_the_planted_findings_in_both_modes() {
    for root in fixtures::with_manifests() {
        let manifest = fixtures::manifest(&root);
        for load_order in [false, true] {
            let at = format!("{} load_order={load_order}", root.display());
            let (report, found) = check(&root, manifest.options.encoding, load_order);
            assert_clean(&report, &at);
            let expected = expected(&manifest.findings, load_order);
            assert!(found == expected, "{at}\n{}", difference(&found, &expected));
        }
    }
}

/// The findings of `sql` as one data file, as `check_sql` would give them, but without its COMMIT at the end.
#[expect(
    clippy::unwrap_used,
    reason = "a script that can't be written or read fails the test"
)]
fn uncommitted(sql: &str, role: Role) -> Vec<Finding> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sql");
    fs::write(&path, sql).unwrap();
    let files = [InputFile {
        path,
        role,
        bytes: 0,
        order: OrderSource::Given,
        includes: Vec::new(),
    }];
    let report = check::run(&files, &Options::default()).unwrap();
    report
        .findings
        .into_iter()
        .filter(|f| f.kind == FindingKind::Uncommitted)
        .collect()
}

#[test]
fn rows_with_no_commit_after_them_are_one_finding_at_the_first() {
    let table = "CREATE TABLE T (ID NUMBER);\n";
    let found = uncommitted(
        &format!(
            "{table}INSERT INTO T VALUES (1);\nCOMMIT;\nINSERT INTO T VALUES (2);\n\
             INSERT ALL INTO T VALUES (3) INTO T VALUES (4) SELECT 1 FROM DUAL;\n"
        ),
        Role::Data,
    );
    let [finding] = found.as_slice() else {
        panic!("{found:#?}");
    };
    assert_eq!((finding.line, finding.count), (Some(4), Some(3)));
    assert_eq!(finding.table, "T");
    let text = sqlca::report::describe(finding);
    assert!(
        text.ends_with(
            "t.sql:4: 3 rows with no COMMIT after them, from this row of T to the end of the file"
        ),
        "{text}"
    );
    let committed = [
        "INSERT INTO T VALUES (1);\ncommit work;\n",
        "INSERT INTO T VALUES (1);\nALTER TABLE T ADD PRIMARY KEY (ID);\n",
        "INSERT INTO T VALUES (1);\nGRANT SELECT ON T TO U;\n",
        "",
    ];
    for sql in committed {
        assert!(
            uncommitted(&format!("{table}{sql}"), Role::Data).is_empty(),
            "{sql}"
        );
    }
    let open = [
        "INSERT INTO T VALUES (1);\nALTER SESSION SET NLS_DATE_FORMAT = 'YYYY';\n",
        "INSERT INTO T VALUES (1);\nROLLBACK;\n",
    ];
    for sql in open {
        assert_eq!(
            uncommitted(&format!("{table}{sql}"), Role::Data).len(),
            1,
            "{sql}"
        );
    }
    let rows = format!("{table}INSERT INTO T VALUES (1);\n");
    assert!(
        uncommitted(&rows, Role::Reference).is_empty(),
        "a reference file needs none"
    );
    let off = Options {
        commit_check: false,
        ..Options::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sql");
    fs::write(&path, &rows).unwrap();
    let files = [InputFile {
        path,
        role: Role::Data,
        bytes: 0,
        order: OrderSource::Given,
        includes: Vec::new(),
    }];
    assert!(check::run(&files, &off).unwrap().findings.is_empty());
}

/// A deferred foreign key doesn't excuse its table's rows: they need a commit after them like any others.
#[test]
fn rows_of_a_table_with_a_deferred_foreign_key_need_a_commit_too() {
    let sql = "CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
               CREATE TABLE C (ID NUMBER, P_ID NUMBER CONSTRAINT C_P REFERENCES P DEFERRABLE INITIALLY DEFERRED);\n\
               INSERT INTO C VALUES (1, 1);\n";
    assert_eq!(uncommitted(sql, Role::Data).len(), 1);
}

/// SQLcl writes no COMMIT into its data files, so each has one finding for its rows; nothing else is wrong. Reference
/// files need no commit.
#[test]
fn the_sqlcl_export_misses_only_a_commit_in_each_data_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sqlcl");
    let (report, found) = check(&root, Encoding::Utf8, true);
    assert_clean(&report, "sqlcl");
    assert!(
        found.iter().all(|f| f.kind == FindingKind::Uncommitted),
        "{found:#?}"
    );
    let with_rows: BTreeSet<String> = inputs(&root)
        .iter()
        .filter(|f| f.role == Role::Data)
        .filter(|f| fs::read_to_string(&f.path).unwrap().contains("Insert into"))
        .map(|f| {
            f.path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    let files: Vec<String> = found.iter().filter_map(|f| f.file.clone()).collect();
    assert_eq!(files.len(), with_rows.len(), "one finding per file");
    assert_eq!(files.into_iter().collect::<BTreeSet<_>>(), with_rows);
    assert_eq!(report.not_followed.len(), 8);
}

#[cfg(feature = "gen")]
mod generated {
    use sqlca::synth::{self, Layout, Noise, Placement};

    use super::{
        BTreeSet, Finding, FindingKind, InputFile, Options, Path, assert_clean, check, difference,
        expected, inputs,
    };

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

    /// Cascade rows have no finding of their own, wherever their parents are, and with a plant at every row the
    /// extras' keys stay clear of theirs.
    #[test]
    fn cascade_rows_add_no_finding() {
        for shuffle_order in [false, true] {
            for constraints in [Placement::BeforeData, Placement::AfterData] {
                assert_manifest(&synth::Options {
                    cascades: true,
                    manager_chain: 3,
                    violations_ppm: 1_000_000,
                    shuffle_order,
                    constraints,
                    ..base(5)
                });
            }
        }
    }

    /// A finding as the manifest writes it: its file relative to the export.
    fn placed(finding: &Finding, root: &Path) -> Finding {
        let file = finding.file.as_deref().map(|file| {
            Path::new(file).strip_prefix(root).map_or_else(
                |_| file.to_owned(),
                |p| p.to_string_lossy().replace('\\', "/"),
            )
        });
        Finding {
            file,
            ..finding.clone()
        }
    }

    #[test]
    fn data_files_in_dependency_order_leave_only_the_load_order_findings_within_a_file() {
        let runs = (1..=4).flat_map(|seed| {
            [Placement::BeforeData, Placement::AfterData].map(|constraints| (seed, constraints))
        });
        for (seed, constraints) in runs {
            let options = synth::Options {
                shuffle_order: true,
                layout: Layout::Separate,
                constraints,
                ..base(seed)
            };
            let seed = format!("{seed} {constraints:?}");
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("shuffled");
            synth::generate(&options, &root).unwrap();
            let unshuffled = synth::generate(
                &synth::Options {
                    shuffle_order: false,
                    ..options.clone()
                },
                &dir.path().join("unshuffled"),
            )
            .unwrap();
            let check_options = Options {
                encoding: options.encoding,
                load_order: true,
                ..Options::default()
            };
            let given = inputs(&root);
            let ordered =
                check::dependency_order(&given, &check_options, &mut check::Quiet).unwrap();
            assert!(ordered.unmet.is_empty(), "seed {seed}: {:?}", ordered.unmet);
            let findings = |files: &[InputFile]| -> (BTreeSet<Finding>, BTreeSet<Finding>) {
                let report = check::run(files, &check_options).unwrap();
                assert_clean(&report, &format!("seed {seed}"));
                report
                    .findings
                    .iter()
                    .map(|f| placed(f, &root))
                    .partition(|f| f.kind == FindingKind::FkLoadOrder)
            };
            let (before_load_order, before) = findings(&given);
            let (after_load_order, after) = findings(&ordered.files);
            // Rows before a constraint added after the data are judged only in the end state.
            assert_eq!(
                before_load_order.is_empty(),
                constraints == Placement::AfterData,
                "seed {seed}: load-order findings before reordering"
            );
            assert_eq!(
                after, before,
                "seed {seed}: reordering changed other findings"
            );
            let within: BTreeSet<Finding> = unshuffled
                .findings
                .into_iter()
                .filter(|f| f.kind == FindingKind::FkLoadOrder)
                .collect();
            assert!(
                after_load_order == within,
                "seed {seed}\n{}",
                difference(&after_load_order, &within)
            );
        }
    }

    #[test]
    fn a_parent_without_rows_is_one_finding_per_foreign_key() {
        assert_manifest(&synth::Options {
            omit_data: vec!["APP.PROJECTS".to_owned()],
            ..base(5)
        });
    }

    /// Without a table's COMMIT, its rows are a finding unless something after them in the same file commits: the
    /// next table's COMMIT, or DDL added after the data.
    #[test]
    fn rows_without_a_commit_after_them_in_every_layout() {
        let mut planted = 0;
        for layout in [Layout::Single, Layout::Separate, Layout::Type] {
            for constraints in [Placement::BeforeData, Placement::AfterData] {
                for table in ["APP.TIMESHEETS", "APP.AuditLog", "REF.COUNTRIES"] {
                    let options = synth::Options {
                        layout,
                        constraints,
                        omit_commit: vec![table.to_owned()],
                        ..base(6)
                    };
                    assert_manifest(&options);
                    let dir = tempfile::tempdir().unwrap();
                    let manifest = synth::generate(&options, &dir.path().join("export")).unwrap();
                    planted += manifest
                        .findings
                        .iter()
                        .filter(|f| f.kind == sqlca::finding::FindingKind::Uncommitted)
                        .count();
                }
            }
        }
        assert!(
            planted >= 3,
            "some layouts leave rows uncommitted: {planted}"
        );
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

/// The report of `text`, as written, checked as one data file.
#[expect(
    clippy::unwrap_used,
    reason = "a script that can't be written or read fails the test"
)]
fn run_script(text: &str, load_order: bool) -> Report {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sql");
    fs::write(&path, text).unwrap();
    let files = [InputFile {
        path,
        role: Role::Data,
        bytes: 0,
        order: OrderSource::Given,
        includes: Vec::new(),
    }];
    let options = Options {
        load_order,
        ..Options::default()
    };
    check::run(&files, &options).unwrap()
}

/// The findings and report of a script given as text, as one data file.
fn check_sql(sql: &str, load_order: bool) -> (Report, Vec<(FindingKind, u64, bool)>) {
    // A COMMIT at the end, as an export has; it moves no line.
    let report = run_script(&format!("{sql}\nCOMMIT;\n"), load_order);
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

const DEPT_AND_EMP: &str = "CREATE TABLE DEPT (DEPTNO NUMBER(2) PRIMARY KEY);\n\
    CREATE TABLE EMP (EMPNO NUMBER(4) PRIMARY KEY, DEPTNO NUMBER(2) NOT NULL REFERENCES DEPT);\n";

/// The kind and line of each finding, the lines of the errors, and each foreign key's rows that can't be checked.
type Outcome = (Vec<(FindingKind, u64)>, Vec<u64>, Vec<(String, u64)>);

fn outcome(report: &Report) -> Outcome {
    let findings = report
        .findings
        .iter()
        .map(|f| (f.kind, f.line.unwrap_or(0)))
        .collect();
    let errors = report.errors.iter().filter_map(|e| e.line).collect();
    let unverifiable = report
        .warnings
        .iter()
        .filter_map(|w| match w {
            check::Warning::Unverifiable { table, count, .. } => Some((table.clone(), *count)),
            _ => None,
        })
        .collect();
    (findings, errors, unverifiable)
}

#[test]
fn rows_in_an_insert_the_checker_cant_parse_leave_their_tables_keys_incomplete() {
    // Multi-row VALUES stands for any insert the runner runs and the checker can't parse.
    let report = run_script(
        &format!(
            "{DEPT_AND_EMP}INSERT INTO DEPT VALUES\n (10),\n (20);\nINSERT INTO DEPT VALUES (30);\n\
             INSERT INTO EMP VALUES (1, 10);\nINSERT INTO EMP VALUES (2, 30);\nCOMMIT;\n"
        ),
        false,
    );
    assert_eq!(
        outcome(&report),
        (vec![], vec![4], vec![("EMP".to_owned(), 1)])
    );
}

#[test]
fn an_insert_the_checker_cant_parse_in_a_child_table_leaves_its_parent_checked() {
    let report = run_script(
        &format!(
            "{DEPT_AND_EMP}INSERT INTO DEPT VALUES (10);\nINSERT INTO EMP VALUES\n (1, 10),\n (2, 99);\n\
             INSERT INTO EMP VALUES (3, 99);\nCOMMIT;\n"
        ),
        false,
    );
    assert_eq!(
        outcome(&report),
        (vec![(FindingKind::FkMissingParent, 7)], vec![5], vec![])
    );
}

#[test]
fn an_insert_whose_table_cant_be_read_leaves_every_key_incomplete() {
    let report = run_script(
        &format!(
            "{DEPT_AND_EMP}INSERT INTO 9 VALUES (1);\nINSERT INTO DEPT VALUES (30);\n\
             INSERT INTO EMP VALUES (1, 10);\nCOMMIT;\n"
        ),
        false,
    );
    assert_eq!(
        outcome(&report),
        (vec![], vec![3], vec![("EMP".to_owned(), 1)])
    );
}

#[test]
fn a_statement_the_runner_never_runs_holds_no_key() {
    // Cut off at the end of the file, the last insert never runs: its child is missing its parent.
    let report = run_script(
        &format!(
            "{DEPT_AND_EMP}INSERT INTO DEPT VALUES (30);\nINSERT INTO EMP VALUES (1, 10);\n\
             INSERT INTO DEPT VALUES (10)\n"
        ),
        false,
    );
    let (findings, errors, unverifiable) = outcome(&report);
    assert!(
        findings.contains(&(FindingKind::FkMissingParent, 4)),
        "{findings:?}"
    );
    assert_eq!((errors, unverifiable), (vec![5], vec![]));
}

/// The database rejects a whole `INSERT ALL` when one clause names a column its table doesn't have, or gives the
/// wrong number of values: none of its rows holds a key, so a child of one is missing its parent and a later row
/// with the same key is no duplicate.
#[test]
fn a_structure_error_in_one_clause_rejects_the_whole_insert_all() {
    use FindingKind::{DuplicateKey, FkMissingParent, UnknownColumn, ValueCountMismatch};
    for (clause, expected) in [
        ("", vec![(DuplicateKey, 5, false)]),
        (
            " INTO EMP (EMPNO, NOPE) VALUES (2, 0)",
            vec![(UnknownColumn, 3, false), (FkMissingParent, 4, false)],
        ),
        (
            " INTO EMP VALUES (2)",
            vec![(ValueCountMismatch, 3, false), (FkMissingParent, 4, false)],
        ),
    ] {
        let (_, findings) = check_sql(
            &format!(
                "{DEPT_AND_EMP}INSERT ALL INTO DEPT VALUES (10){clause} INTO DEPT VALUES (20) SELECT 1 FROM DUAL;\n\
                 INSERT INTO EMP VALUES (1, 10);\nINSERT INTO DEPT VALUES (20);\n"
            ),
            false,
        );
        assert_eq!(findings, expected, "{clause}");
    }
}

/// A table no input defines, or one in several schemas, may be in the database, which then runs the statement: the
/// other rows of its `INSERT ALL` stay checked, and the child of one finds its parent.
#[test]
fn a_table_the_inputs_cant_resolve_leaves_its_insert_all_checked() {
    for (table, kind) in [
        ("T", FindingKind::AmbiguousTable),
        ("NOPE", FindingKind::UnknownTable),
    ] {
        let (_, findings) = check_sql(
            &format!(
                "CREATE TABLE A.T (ID NUMBER PRIMARY KEY);\nCREATE TABLE B.T (ID NUMBER PRIMARY KEY);\n\
                 CREATE TABLE A.P (ID NUMBER PRIMARY KEY);\n\
                 CREATE TABLE A.C (ID NUMBER PRIMARY KEY, PID NUMBER NOT NULL REFERENCES A.P);\n\
                 INSERT ALL INTO A.P VALUES (7) INTO {table} VALUES (1) SELECT 1 FROM DUAL;\n\
                 INSERT INTO A.C VALUES (1, 7);\n"
            ),
            false,
        );
        assert_eq!(findings, [(kind, 5, false)], "{table}");
    }
}

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
fn a_default_reads_as_the_same_value_written_in_the_row() {
    let (report, findings) = check_sql(
        "CREATE TABLE T (ID NUMBER, D DATE DEFAULT TO_DATE('2024-01-31', 'YYYY-MM-DD'), \
         CONSTRAINT T_UK UNIQUE (ID, D));\n\
         INSERT INTO T (ID) VALUES (1);\n\
         INSERT INTO T (ID, D) VALUES (1, DATE '2024-01-31');\n",
        false,
    );
    assert_eq!(findings, [(FindingKind::DuplicateKey, 3, false)]);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
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

/// Oracle accepts `MODIFY … AS IDENTITY` only on an identity column, so one that restates it, as Oracle's CO sample
/// does at its end, changes no more than the next generated value. One that changes whether an explicit NULL takes
/// a generated value isn't followed.
#[test]
fn an_identity_restated_by_alter_table_is_followed() {
    let table = "CREATE TABLE T (ID NUMBER GENERATED BY DEFAULT ON NULL AS IDENTITY PRIMARY KEY, N NUMBER);\n\
                 INSERT INTO T VALUES (1, 1);\n";
    for (modify, followed) in [
        (
            "MODIFY ID GENERATED BY DEFAULT ON NULL AS IDENTITY (START WITH LIMIT VALUE)",
            true,
        ),
        (
            "MODIFY (\"ID\" GENERATED BY DEFAULT ON NULL AS IDENTITY START WITH 10 NOCACHE NOT NULL)",
            true,
        ),
        ("MODIFY ID GENERATED ALWAYS AS IDENTITY", false),
        ("MODIFY N GENERATED BY DEFAULT ON NULL AS IDENTITY", false),
        ("MODIFY N GENERATED ALWAYS AS (ID * 2) VIRTUAL", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sql");
        fs::write(&path, format!("{table}ALTER TABLE T {modify};\nCOMMIT;\n")).unwrap();
        let files = [InputFile {
            path,
            role: Role::Data,
            bytes: 0,
            order: OrderSource::Given,
            includes: Vec::new(),
        }];
        let report = check::run(&files, &Options::default()).unwrap();
        assert!(
            report.findings.is_empty(),
            "{modify}: {:?}",
            report.findings
        );
        let problems: Vec<(Option<u64>, &str)> = report
            .catalog
            .iter()
            .map(|p| (p.line, p.message.as_str()))
            .collect();
        if followed {
            assert_eq!(problems, [], "{modify}");
        } else {
            let [(Some(3), message)] = problems.as_slice() else {
                panic!("{modify}: {problems:?}");
            };
            assert!(message.ends_with(": not followed"), "{message}");
        }
    }
}

/// A script switches an identity column to `BY DEFAULT` to load rows that bring their own values, then back: the
/// database refuses a value only while the column is `ALWAYS`, also when the `MODIFY` changes more than the check
/// follows.
#[test]
fn an_identity_refuses_values_only_while_it_is_always() {
    use FindingKind::{DuplicateKey, GeneratedColumn};
    let report = run_script(
        "CREATE TABLE T (ID NUMBER GENERATED ALWAYS AS IDENTITY, U NUMBER UNIQUE);\n\
         INSERT INTO T VALUES (1, 7);\n\
         ALTER TABLE T MODIFY (ID GENERATED BY DEFAULT AS IDENTITY, U DEFAULT 5);\n\
         INSERT INTO T VALUES (2, 7);\nINSERT INTO T VALUES (3, 7);\n\
         ALTER TABLE T MODIFY ID GENERATED ALWAYS AS IDENTITY (START WITH LIMIT VALUE);\n\
         INSERT INTO T VALUES (4, 8);\nINSERT INTO T (U) VALUES (8);\nCOMMIT;\n",
        false,
    );
    let (findings, errors, _) = outcome(&report);
    assert_eq!(
        (findings, errors),
        (
            vec![
                (GeneratedColumn, 2),
                (DuplicateKey, 5),
                (GeneratedColumn, 7)
            ],
            vec![]
        )
    );
    let issues: Vec<Option<u64>> = report.catalog.iter().map(|p| p.line).collect();
    assert_eq!(issues, [Some(3)], "{:?}", report.catalog);
}

/// A `MODIFY` that writes neither `ALWAYS` nor `BY DEFAULT` keeps the column's mode, and whether NULL takes a value:
/// the parent row that brings its own key stays, and so does its child.
#[test]
fn an_identity_modify_without_a_mode_keeps_the_columns_own() {
    use FindingKind::GeneratedColumn;
    for (created, rows, expected) in [
        (
            "BY DEFAULT",
            "INSERT INTO P VALUES (10, 7);\nINSERT INTO P VALUES (DEFAULT, 8);\n",
            vec![],
        ),
        (
            "BY DEFAULT ON NULL",
            "INSERT INTO P VALUES (NULL, 7);\n",
            vec![],
        ),
        (
            "ALWAYS",
            "INSERT INTO P VALUES (10, 7);\nINSERT INTO P VALUES (DEFAULT, 7);\n",
            vec![(GeneratedColumn, 4, false)],
        ),
    ] {
        let (_, findings) = check_sql(
            &format!(
                "CREATE TABLE P (ID NUMBER GENERATED {created} AS IDENTITY, K NUMBER PRIMARY KEY);\n\
                 CREATE TABLE C (K NUMBER REFERENCES P);\n\
                 ALTER TABLE P MODIFY ID GENERATED AS IDENTITY (CACHE 100);\n\
                 {rows}INSERT INTO C VALUES (7);\n"
            ),
            false,
        );
        assert_eq!(findings, expected, "{created}");
    }
}

/// After `DROP IDENTITY` the column takes the values rows give it: the parent row that brings its own stays, and so
/// does its child. A value before it is still refused.
#[test]
fn an_identity_takes_values_after_drop_identity() {
    for drop in ["MODIFY ID DROP IDENTITY", "MODIFY (ID DROP IDENTITY)"] {
        let (_, findings) = check_sql(
            &format!(
                "CREATE TABLE P (ID NUMBER GENERATED ALWAYS AS IDENTITY, K NUMBER PRIMARY KEY);\n\
                 CREATE TABLE C (K NUMBER REFERENCES P);\n\
                 INSERT INTO P (K) VALUES (1);\nINSERT INTO P VALUES (9, 3);\nALTER TABLE P {drop};\n\
                 INSERT INTO P (ID, K) VALUES (10, 2);\nINSERT INTO C VALUES (2);\n"
            ),
            false,
        );
        assert_eq!(
            findings,
            [(FindingKind::GeneratedColumn, 4, false)],
            "{drop}"
        );
    }
}

/// Each constraint's rows the check can't verify.
fn unverifiable(report: &Report) -> BTreeSet<(Option<String>, u64)> {
    report
        .warnings
        .iter()
        .filter_map(|w| match w {
            check::Warning::Unverifiable {
                constraint, count, ..
            } => Some((constraint.clone(), *count)),
            _ => None,
        })
        .collect()
}

/// Oracle computes a virtual column, so a key over one is unverifiable, never a duplicate by the rule for partly
/// NULL keys.
#[test]
fn a_unique_key_over_a_virtual_column_is_unverifiable() {
    let (report, findings) = check_sql(
        "CREATE TABLE US (ID NUMBER(9,0) CONSTRAINT US_PK PRIMARY KEY, TENANT_ID NUMBER(9,0) NOT NULL, \
         EMAIL VARCHAR2(50), EMAIL_UC VARCHAR2(50) GENERATED ALWAYS AS (UPPER(EMAIL)) VIRTUAL, \
         CONSTRAINT US_EMAIL_UK UNIQUE (TENANT_ID, EMAIL_UC));\n\
         INSERT INTO US (ID, TENANT_ID, EMAIL) VALUES (1, 7, 'a@x');\n\
         INSERT INTO US (ID, TENANT_ID, EMAIL) VALUES (2, 7, 'b@x');\n\
         INSERT INTO US VALUES (3, 7, 'c@x', DEFAULT);\n",
        false,
    );
    assert_eq!(findings, []);
    assert_eq!(
        unverifiable(&report),
        BTreeSet::from([(Some("US_EMAIL_UK".to_owned()), 3)])
    );
}

/// A virtual column under a primary key, NOT NULL or a foreign key is never NULL and never a missing parent: its
/// keys are unverifiable, and so are the children of a parent key over one.
#[test]
fn a_virtual_column_is_never_null_or_a_missing_parent() {
    let (report, findings) = check_sql(
        "CREATE TABLE P (ID NUMBER, K AS (ID * 10) CONSTRAINT P_PK PRIMARY KEY);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_K AS (ID + 0) NOT NULL CONSTRAINT C_FK REFERENCES P);\n\
         CREATE TABLE D (ID NUMBER PRIMARY KEY, P_K NUMBER CONSTRAINT D_FK REFERENCES P);\n\
         INSERT INTO P (ID) VALUES (1);\nINSERT INTO C (ID) VALUES (10);\nINSERT INTO D VALUES (1, 99);\n",
        false,
    );
    assert_eq!(findings, []);
    let constraints = ["P_PK", "C_FK", "D_FK"].map(|c| (Some(c.to_owned()), 1));
    assert_eq!(unverifiable(&report), BTreeSet::from(constraints));
}

/// Oracle rejects a row that gives a virtual or `GENERATED ALWAYS` identity column a value, even NULL; it takes only
/// `DEFAULT`. The statement holds no key, a whole `INSERT ALL` included, so a later row with the same key is no
/// duplicate.
#[test]
fn a_value_for_a_generated_column_rejects_the_statement() {
    use FindingKind::{DuplicateKey, GeneratedColumn, ValueCountMismatch};
    let (report, findings) = check_sql(
        "CREATE TABLE V (ID NUMBER PRIMARY KEY, E VARCHAR2(9), U AS (UPPER(E)));\n\
         CREATE TABLE I (ID NUMBER GENERATED ALWAYS AS IDENTITY, N NUMBER PRIMARY KEY);\n\
         CREATE TABLE B (ID NUMBER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, N NUMBER);\n\
         INSERT INTO V (ID, E, U) VALUES (1, 'a', NULL);\nINSERT INTO V VALUES (1, 'b', DEFAULT);\n\
         INSERT INTO V VALUES (2, 'c');\nINSERT INTO V (ID, U) VALUES (2, DEFAULT);\n\
         INSERT INTO I VALUES (1, 1);\nINSERT INTO I (ID, N) VALUES (DEFAULT, 1);\nINSERT INTO I (N) VALUES (1);\n\
         INSERT INTO B VALUES (1, 1);\nINSERT INTO B VALUES (1, 2);\n\
         INSERT ALL INTO V (ID, E) VALUES (3, 'x') INTO V (ID, U) VALUES (4, 'Y') SELECT 1 FROM DUAL;\n\
         INSERT INTO V (ID, E) VALUES (3, 'z');\n",
        false,
    );
    assert_eq!(
        findings,
        [
            (GeneratedColumn, 4, false),
            (ValueCountMismatch, 6, false),
            (GeneratedColumn, 8, false),
            (DuplicateKey, 10, false),
            (DuplicateKey, 12, false),
            (GeneratedColumn, 13, false)
        ]
    );
    let columns: Vec<&[String]> = report
        .findings
        .iter()
        .filter(|f| f.kind == GeneratedColumn)
        .map(|f| f.columns.as_slice())
        .collect();
    assert_eq!(
        columns,
        [["U".to_owned()], ["ID".to_owned()], ["U".to_owned()]]
    );
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
