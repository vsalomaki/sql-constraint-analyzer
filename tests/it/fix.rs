//! `fix` against the generator's exports and hand-written cases: the output has none of the violations it fixes, its
//! bytes plus the removed ones restore the input exactly, and what it must not touch survives.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sqlca::check::{self, Options};
use sqlca::finding::{FindingKind, Location};
use sqlca::fix::{self, FixReport, Policy};
use sqlca::input::{InputFile, OrderSource, Role};

use crate::fixtures::{self, Nulled, inputs};

const FIXED: [FindingKind; 3] = [
    FindingKind::FkMissingParent,
    FindingKind::DuplicateKey,
    FindingKind::NotNull,
];

fn count(report: &check::Report, kind: FindingKind) -> usize {
    report.findings.iter().filter(|f| f.kind == kind).count()
}

/// Each input's bytes equal its output's with the removed bytes put back where they were, in place of what the output
/// has there instead.
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "byte bookkeeping over files the test wrote; a mismatch fails the test"
)]
fn assert_restores(files: &[InputFile], report: &FixReport) {
    for (file, written) in files.iter().zip(report.files.iter().map(|f| &f.path)) {
        let original = fs::read(&file.path).unwrap();
        let output = fs::read(written).unwrap();
        let name = check::display(&file.path);
        // Each change as (span, what the output has, what the input had); an added commit replaces nothing.
        let removals = report.removed.iter().filter(|r| r.file == name);
        let commits = report.committed.iter().filter(|c| c.file == name);
        let mut changes: Vec<(u64, u64, &[u8], &[u8])> = removals
            .map(|r| (r.span.start, r.span.end, &r.replacement[..], &r.text[..]))
            .chain(commits.map(|c| (c.at, c.at, &c.text[..], &b""[..])))
            .collect();
        changes.sort_by_key(|c| c.0);
        let (mut rebuilt, mut at_original, mut at_output) = (Vec::new(), 0_usize, 0_usize);
        for (start, end, replacement, text) in changes {
            let start = usize::try_from(start).unwrap();
            let kept = start - at_original;
            rebuilt.extend_from_slice(&output[at_output..at_output + kept]);
            at_output += kept;
            let replaced = &output[at_output..at_output + replacement.len()];
            assert!(replaced == replacement, "{name}: no replacement at {start}");
            rebuilt.extend_from_slice(text);
            at_output += replacement.len();
            at_original = usize::try_from(end).unwrap();
        }
        rebuilt.extend_from_slice(&output[at_output..]);
        assert!(rebuilt == original, "{name} isn't restored by its rejects");
    }
}

fn run(files: &[InputFile], options: &Options) -> (tempfile::TempDir, FixReport) {
    run_with(files, options, Policy::default(), false)
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture or folder that fails fails the test"
)]
fn run_with(
    files: &[InputFile],
    options: &Options,
    policy: Policy,
    annotate: bool,
) -> (tempfile::TempDir, FixReport) {
    let out = tempfile::tempdir().unwrap();
    let report = fix::fix(
        files,
        options,
        policy,
        &fix::Target {
            annotate,
            ..fix::Target::out(out.path().join("fixed"))
        },
    )
    .unwrap();
    (out, report)
}

#[test]
fn committed_exports_are_fixed_and_restored_by_their_rejects() {
    for root in fixtures::with_manifests() {
        let manifest = fixtures::manifest(&root);
        for load_order in [false, true] {
            let options = Options {
                encoding: manifest.options.encoding,
                load_order,
                ..Options::default()
            };
            let files = inputs(&root);
            let (_dir, report) = run(&files, &options);
            let at = format!("{} load_order={load_order}", root.display());
            for kind in FIXED {
                assert!(count(&report.input, kind) > 0, "{at}: no {kind:?} planted");
                assert_eq!(
                    count(&report.output, kind),
                    0,
                    "{at}: {kind:?} left: {:#?}",
                    report.output.findings
                );
            }
            for kind in [
                FindingKind::UnknownTable,
                FindingKind::UnknownColumn,
                FindingKind::ValueCountMismatch,
            ] {
                assert_eq!(
                    count(&report.output, kind),
                    count(&report.input, kind),
                    "{at}: {kind:?}"
                );
            }
            // Load-order findings are valid rows in the end state: removed only as a cascade row, with its root.
            let removed: BTreeSet<&Location> = manifest.fixes.removed.iter().collect();
            let cascading = manifest
                .findings
                .iter()
                .filter(|f| f.kind == FindingKind::FkLoadOrder && load_order)
                .filter_map(|f| f.file.clone().zip(f.line))
                .filter(|(file, line)| {
                    removed.contains(&Location {
                        file: file.clone(),
                        line: *line,
                    })
                })
                .count();
            assert_eq!(
                count(&report.output, FindingKind::FkLoadOrder),
                count(&report.input, FindingKind::FkLoadOrder) - cascading,
                "{at}"
            );
            assert!(
                report.output.errors.is_empty(),
                "{at}: {:?}",
                report.output.errors
            );
            assert_restores(&files, &report);
        }
    }
}

/// Under the default policy and `--on-fk null`, `fix` removes exactly the rows the export's manifest lists, sets to
/// NULL exactly the columns it lists, and leaves nothing it acts on, cascade rows included.
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "an export the test reads or fixes; a mismatch fails the test"
)]
fn assert_fixes_as_listed(root: &Path) {
    let manifest = fixtures::manifest(root);
    let files = inputs(root);
    let order = fs::read_to_string(root.join("order.txt")).unwrap();
    let relative: BTreeMap<String, &str> = files
        .iter()
        .zip(order.lines())
        .map(|(file, path)| (check::display(&file.path), path))
        .collect();
    let listed = &manifest.fixes;
    let policies = [
        (Policy::default(), &listed.removed, &[][..]),
        (
            null_fk(),
            &listed.removed_on_fk_null,
            &listed.nulled_on_fk_null[..],
        ),
    ];
    for load_order in [false, true] {
        for (policy, removed, nulled) in policies {
            let options = Options {
                encoding: manifest.options.encoding,
                load_order,
                ..Options::default()
            };
            let (_dir, report) = run_with(&files, &options, policy, false);
            let at = format!("{} {policy:?} load_order={load_order}", root.display());
            let place = |file: &str| relative[file].to_owned();
            let got: BTreeSet<Location> = report
                .removed
                .iter()
                .filter(|r| r.nulled.is_empty())
                .map(|r| Location {
                    file: place(&r.file),
                    line: r.line,
                })
                .collect();
            assert_eq!(got, removed.iter().cloned().collect(), "{at}: removed");
            let got: BTreeSet<Nulled> = report
                .removed
                .iter()
                .filter(|r| !r.nulled.is_empty())
                .map(|r| {
                    let mut columns: Vec<String> =
                        r.nulled.iter().map(|(c, _)| c.clone()).collect();
                    columns.sort();
                    Nulled {
                        file: place(&r.file),
                        line: r.line,
                        columns,
                    }
                })
                .collect();
            assert_eq!(got, nulled.iter().cloned().collect(), "{at}: nulled");
            for kind in FIXED {
                assert_eq!(count(&report.output, kind), 0, "{at}: {kind:?} left");
            }
        }
    }
}

#[test]
fn committed_exports_lose_exactly_the_rows_their_manifests_list() {
    for root in fixtures::with_manifests() {
        assert_fixes_as_listed(&root);
    }
}

/// A script as one data file, fixed into a fresh folder.
fn fix_sql(sql: &str) -> (tempfile::TempDir, Vec<InputFile>, FixReport) {
    fix_scripts(
        &[("t.sql", sql)],
        &Options::default(),
        Policy::default(),
        false,
    )
}

/// The scripts, by name, in one folder, fixed into a fresh folder: a name under `ref/` is a reference file.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture or folder that fails fails the test"
)]
fn fix_scripts(
    scripts: &[(&str, &str)],
    options: &Options,
    policy: Policy,
    annotate: bool,
) -> (tempfile::TempDir, Vec<InputFile>, FixReport) {
    let (dir, files) = write_scripts(scripts);
    let report = fix::fix(
        &files,
        options,
        policy,
        &fix::Target {
            annotate,
            ..fix::Target::out(dir.path().join("out"))
        },
    )
    .unwrap();
    assert_restores(&files, &report);
    (dir, files, report)
}

/// Each script that ends its last line also ends with a COMMIT, as an export does; `outputs` leaves it out.
#[expect(clippy::unwrap_used, reason = "a folder that fails fails the test")]
fn write_scripts(scripts: &[(&str, &str)]) -> (tempfile::TempDir, Vec<InputFile>) {
    let dir = tempfile::tempdir().unwrap();
    let files = scripts
        .iter()
        .map(|(name, sql)| {
            let path = dir.path().join("in").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let sql = if sql.ends_with('\n') {
                format!("{sql}{COMMIT}")
            } else {
                (*sql).to_owned()
            };
            fs::write(&path, &sql).unwrap();
            InputFile {
                path,
                role: if name.starts_with("ref/") {
                    Role::Reference
                } else {
                    Role::Data
                },
                bytes: u64::try_from(sql.len()).unwrap(),
                order: OrderSource::Given,
                includes: Vec::new(),
            }
        })
        .collect();
    (dir, files)
}

/// What `write_scripts` ends each script with.
const COMMIT: &str = "COMMIT;\n";

/// The written outputs' text, by input position, without the COMMIT `write_scripts` ended each with.
#[expect(clippy::unwrap_used, reason = "an output that fails fails the test")]
fn outputs(report: &FixReport) -> Vec<String> {
    report
        .files
        .iter()
        .map(|f| {
            let text = fs::read_to_string(&f.path).unwrap();
            text.strip_suffix(COMMIT).unwrap_or(&text).to_owned()
        })
        .collect()
}

fn keep_last() -> Policy {
    Policy {
        on_duplicate: fix::OnDuplicate::KeepLast,
        ..Policy::default()
    }
}

fn null_fk() -> Policy {
    Policy {
        on_fk: fix::OnFk::Null,
        ..Policy::default()
    }
}

/// A script as one data file, fixed with `--on-fk null`.
fn fix_null(sql: &str) -> (tempfile::TempDir, Vec<InputFile>, FixReport) {
    fix_scripts(&[("t.sql", sql)], &Options::default(), null_fk(), false)
}

const PARENT: &str = "CREATE TABLE P (ID NUMBER PRIMARY KEY);\nINSERT INTO P VALUES (1);\n";

#[test]
fn a_missing_parent_is_set_to_null_and_the_original_kept_in_the_rejects() {
    let (dir, _, report) = fix_null(&format!(
        "{PARENT}CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
         INSERT INTO C VALUES (1, 1);\nINSERT INTO C VALUES (2, 9);\n"
    ));
    assert!(
        output(&report)
            .ends_with("INSERT INTO C VALUES (1, 1);\nINSERT INTO C VALUES (2, NULL);\n"),
        "{}",
        output(&report)
    );
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
    let [change] = report.removed.as_slice() else {
        panic!("{:#?}", report.removed);
    };
    assert_eq!(change.line, 5);
    let nulled: Vec<&str> = change.nulled.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(nulled, ["P_ID"]);
    assert!(fix::summary(&report).contains("1 kept with a foreign key set to NULL"));
    let reports = dir.path().join("out/sqlca-report");
    let rejects = fs::read_to_string(reports.join("rejects.sql")).unwrap();
    assert!(rejects.contains("kept with P_ID set to NULL"), "{rejects}");
    assert!(
        rejects.contains("INSERT INTO C VALUES (2, 9);"),
        "{rejects}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(reports.join("fix-report.json")).unwrap())
            .unwrap();
    assert_eq!(json["removed"][0]["nulled"], serde_json::json!(["P_ID"]));
}

#[test]
fn a_foreign_key_that_cant_hold_null_drops_the_row_and_says_why() {
    for (column, insert, why) in [
        (
            "P_ID NUMBER NOT NULL REFERENCES P",
            "INSERT INTO C VALUES (2, 9);",
            "P_ID is NOT NULL",
        ),
        (
            "P_ID NUMBER DEFAULT ON NULL 9 REFERENCES P",
            "INSERT INTO C VALUES (2, 9);",
            "P_ID takes its default in place of NULL",
        ),
        (
            "P_ID NUMBER DEFAULT 9 REFERENCES P",
            "INSERT INTO C (ID) VALUES (2);",
            "the row leaves P_ID out, so its default applies",
        ),
    ] {
        let (_dir, _, report) = fix_null(&format!(
            "{PARENT}CREATE TABLE C (ID NUMBER PRIMARY KEY, {column});\nINSERT INTO C VALUES (1, 1);\n{insert}\n"
        ));
        let [removal] = report.removed.as_slice() else {
            panic!("{column}: {:#?}", report.removed);
        };
        assert!(removal.nulled.is_empty(), "{column}");
        assert!(
            removal.reasons[0]
                .text
                .ends_with(&format!("; the row went, since {why}")),
            "{column}: {}",
            removal.reasons[0].text
        );
        assert!(!output(&report).contains(insert), "{column}");
        assert!(
            report.output.findings.is_empty(),
            "{column}: {:?}",
            report.output.findings
        );
    }
    let (_dir, _, report) = fix_null(&format!(
        "{PARENT}CREATE TABLE C (P_ID NUMBER PRIMARY KEY REFERENCES P);\n\
         INSERT INTO C VALUES (1);\nINSERT INTO C VALUES (9);\n"
    ));
    assert!(
        report.removed[0].reasons[0]
            .text
            .ends_with("; the row went, since P_ID is part of the primary key")
    );
}

#[test]
fn a_disabled_primary_key_lets_its_column_be_set_to_null() {
    let script = |state: &str| {
        format!(
            "{PARENT}CREATE TABLE C (X NUMBER, PRIMARY KEY (X){state}, FOREIGN KEY (X) REFERENCES P);\n\
             INSERT INTO C VALUES (1);\nINSERT INTO C VALUES (9);\n"
        )
    };
    let (_dir, _, report) = fix_null(&script(" DISABLE NOVALIDATE"));
    assert!(
        output(&report).ends_with("INSERT INTO C VALUES (NULL);\n"),
        "{}",
        output(&report)
    );
    let (_dir, _, report) = fix_null(&script(""));
    assert!(report.removed[0].nulled.is_empty(), "{:#?}", report.removed);
}

#[test]
fn a_constraint_enabled_after_the_row_without_validating_lets_it_be_set_to_null() {
    for (declared, enable, nulled) in [
        (
            "CONSTRAINT C_NN NOT NULL DISABLE",
            "ENABLE NOVALIDATE CONSTRAINT C_NN",
            true,
        ),
        (
            "CONSTRAINT C_NN NOT NULL DISABLE",
            "ENABLE VALIDATE CONSTRAINT C_NN",
            false,
        ),
        (
            "CONSTRAINT C_PK PRIMARY KEY DISABLE",
            "ENABLE NOVALIDATE CONSTRAINT C_PK",
            true,
        ),
        (
            "CONSTRAINT C_PK PRIMARY KEY DISABLE",
            "ENABLE VALIDATE CONSTRAINT C_PK",
            false,
        ),
    ] {
        let (_dir, _, report) = fix_null(&format!(
            "{PARENT}CREATE TABLE C (X NUMBER {declared}, CONSTRAINT C_FK FOREIGN KEY (X) REFERENCES P);\n\
             INSERT INTO C VALUES (9);\nALTER TABLE C {enable};\nINSERT INTO C VALUES (1);\n"
        ));
        let [action] = report.removed.as_slice() else {
            panic!("{declared} / {enable}: {:#?}", report.removed);
        };
        assert_eq!(!action.nulled.is_empty(), nulled, "{declared} / {enable}");
        assert!(
            report.output.findings.is_empty(),
            "{declared} / {enable}: {:?}",
            report.output.findings
        );
    }
}

#[test]
fn every_column_of_a_composite_foreign_key_is_set_to_null() {
    let (_dir, _, report) = fix_null(
        "CREATE TABLE P (A NUMBER, B NUMBER, PRIMARY KEY (A, B));\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, A NUMBER, B NUMBER, FOREIGN KEY (A, B) REFERENCES P);\n\
         INSERT INTO P VALUES (1, 2);\nINSERT INTO C VALUES (1, 7, 8);\n",
    );
    assert!(output(&report).ends_with("INSERT INTO C VALUES (1, NULL, NULL);\n"));
    assert!(report.output.findings.is_empty());
}

#[test]
fn a_row_set_to_null_then_removed_is_one_removal_with_its_original() {
    let (dir, _, report) = fix_null(&format!(
        "{PARENT}CREATE TABLE Q (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, Q_ID NUMBER NOT NULL REFERENCES Q);\n\
         INSERT INTO Q VALUES (1, NULL);\nINSERT INTO Q VALUES (2, 5);\n\
         INSERT INTO C VALUES (1, 9, 1);\nINSERT INTO C VALUES (2, 1, 2);\n"
    ));
    let removed: Vec<(u64, usize, usize)> = report
        .removed
        .iter()
        .map(|r| (r.line, r.round, r.nulled.len()))
        .collect();
    assert_eq!(removed, [(5, 1, 0), (7, 2, 0)], "{:#?}", report.removed);
    let kinds: Vec<FindingKind> = report.removed[1].reasons.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        [FindingKind::FkMissingParent, FindingKind::FkMissingParent]
    );
    assert!(!output(&report).contains("(1, 9, 1)") && !output(&report).contains("NULL, 1)"));
    let rejects = fs::read_to_string(dir.path().join("out/sqlca-report/rejects.sql")).unwrap();
    assert_eq!(
        rejects.matches("INSERT INTO C VALUES (1, 9, 1);").count(),
        1,
        "{rejects}"
    );
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

#[test]
fn two_foreign_keys_set_to_null_in_different_rounds_are_one_change() {
    let (_dir, _, report) = fix_null(&format!(
        "{PARENT}CREATE TABLE Q (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, Q_ID NUMBER REFERENCES Q);\n\
         INSERT INTO Q VALUES (1, NULL);\nINSERT INTO Q VALUES (2, 5);\n\
         INSERT INTO C VALUES (1, 9, 1);\n"
    ));
    let changes: Vec<(u64, usize, Vec<&str>)> = report
        .removed
        .iter()
        .filter(|r| !r.nulled.is_empty())
        .map(|r| {
            (
                r.line,
                r.round,
                r.nulled.iter().map(|(c, _)| c.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [(7, 1, vec!["P_ID", "Q_ID"])],
        "{:#?}",
        report.removed
    );
    assert!(output(&report).ends_with("INSERT INTO C VALUES (1, NULL, NULL);\n"));
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

#[test]
fn a_null_that_makes_a_unique_key_a_duplicate_is_caught_in_a_later_round() {
    let (_dir, _, report) = fix_null(&format!(
        "{PARENT}CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, K NUMBER, UNIQUE (P_ID, K));\n\
         INSERT INTO C VALUES (1, NULL, 5);\nINSERT INTO C VALUES (2, 9, 5);\n"
    ));
    let removed: Vec<(u64, usize, usize)> = report
        .removed
        .iter()
        .map(|r| (r.line, r.round, r.nulled.len()))
        .collect();
    assert_eq!(removed, [(5, 2, 0)], "{:#?}", report.removed);
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

#[test]
fn a_row_of_an_insert_all_is_set_to_null_in_place() {
    let (_dir, _, report) = fix_null(&format!(
        "{PARENT}CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
         INSERT ALL\n  INTO C VALUES (1, 1)\n  INTO C VALUES (2, 9)\nSELECT * FROM dual;\n"
    ));
    assert!(
        output(&report)
            .ends_with("  INTO C VALUES (1, 1)\n  INTO C VALUES (2, NULL)\nSELECT * FROM dual;\n"),
        "{}",
        output(&report)
    );
    assert!(report.removed[0].clause);
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

#[test]
fn an_annotated_null_says_why_inside_a_comment() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            &format!(
                "{PARENT}CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\nINSERT INTO C VALUES (2, 9);\n"
            ),
        )],
        &Options::default(),
        null_fk(),
        true,
    );
    assert!(
        output(&report).ends_with(
            "INSERT INTO C VALUES (2, NULL /* sqlca fix set P_ID to NULL; see rejects.sql */);\n"
        ),
        "{}",
        output(&report)
    );
    assert!(report.output.findings.is_empty() && report.output.errors.is_empty());
}

#[test]
fn keep_last_keeps_the_last_row_of_each_key_in_run_order() {
    let ddl = "CREATE TABLE T (ID NUMBER PRIMARY KEY, V VARCHAR2(9));\n";
    let a = format!("{ddl}INSERT INTO T VALUES (1, 'a');\nINSERT INTO T VALUES (2, 'x');\n");
    let b = "INSERT INTO T VALUES (1, 'b');\nINSERT INTO T VALUES (1, 'c');\nINSERT INTO T VALUES (2, 'y');\n";
    let (_dir, _, report) = fix_scripts(
        &[("a.sql", &a), ("b.sql", b)],
        &Options::default(),
        keep_last(),
        false,
    );
    assert_eq!(
        outputs(&report),
        [
            ddl.to_owned(),
            "INSERT INTO T VALUES (1, 'c');\nINSERT INTO T VALUES (2, 'y');\n".to_owned()
        ]
    );
    assert_eq!(report.rounds, 1, "{:#?}", report.removed);
    assert!(
        report
            .removed
            .iter()
            .all(|r| r.reasons.len() == 1 && r.reasons[0].kind == FindingKind::DuplicateKey)
    );
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
    assert!(report.left.is_empty(), "{:?}", report.left);
}

#[test]
fn duplicates_wait_until_the_row_to_keep_is_known() {
    // The row each policy would keep loses its parent two rounds later; the other row is the one that stays.
    let ddl = "CREATE TABLE G (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
               CREATE TABLE P (ID NUMBER PRIMARY KEY, G_ID NUMBER REFERENCES G);\n\
               CREATE TABLE T (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
               INSERT INTO G VALUES (1, 1);\nINSERT INTO G VALUES (2, NULL);\n\
               INSERT INTO P VALUES (1, 1);\nINSERT INTO P VALUES (2, 2);\n";
    let (valid, doomed) = (
        "INSERT INTO T VALUES (1, 1);\n",
        "INSERT INTO T VALUES (1, 2);\n",
    );
    for (on_duplicate, rows) in [
        (fix::OnDuplicate::KeepFirst, format!("{valid}{doomed}")),
        (fix::OnDuplicate::KeepFirst, format!("{doomed}{valid}")),
        (fix::OnDuplicate::KeepLast, format!("{valid}{doomed}")),
        (fix::OnDuplicate::KeepLast, format!("{doomed}{valid}")),
    ] {
        let policy = Policy {
            on_duplicate,
            ..Policy::default()
        };
        let (_dir, _, report) = fix_scripts(
            &[("t.sql", &format!("{ddl}{rows}"))],
            &Options::default(),
            policy,
            false,
        );
        let at = format!("{on_duplicate:?} {rows:?}");
        assert!(
            output(&report).ends_with(&format!("(1, 1);\n{valid}")),
            "{at}"
        );
        assert!(report.output.findings.is_empty(), "{at}");
    }
}

#[test]
fn keep_last_tells_unnamed_keys_apart() {
    let ddl = "CREATE TABLE T (A NUMBER UNIQUE, B NUMBER UNIQUE);\n";
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            &format!(
                "{ddl}INSERT INTO T VALUES (1, 10);\nINSERT INTO T VALUES (1, 20);\n\
                 INSERT INTO T VALUES (30, 1);\nINSERT INTO T VALUES (40, 1);\n"
            ),
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    assert_eq!(
        output(&report),
        format!("{ddl}INSERT INTO T VALUES (1, 20);\nINSERT INTO T VALUES (40, 1);\n")
    );
}

#[test]
fn keep_last_waits_while_the_last_row_goes_for_another_reason() {
    // The last row of key 1 has a NULL; removing it with the others would lose the key.
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
             INSERT INTO T VALUES (1, 5);\nINSERT INTO T VALUES (1, 6);\nINSERT INTO T VALUES (1, NULL);\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(2, 2), (4, 1)], "{:#?}", report.removed);
    assert!(output(&report).ends_with(");\nINSERT INTO T VALUES (1, 6);\n"));
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );

    // Row 3 is the last of key 1 and goes as an earlier row of 'b': key 1 waits for it.
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (ID NUMBER PRIMARY KEY, C VARCHAR2(1) UNIQUE);\n\
             INSERT INTO T VALUES (1, 'a');\nINSERT INTO T VALUES (1, 'b');\nINSERT INTO T VALUES (2, 'b');\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(3, 1)], "{:#?}", report.removed);
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

#[test]
fn keep_last_takes_the_rows_a_removed_first_row_hid() {
    // The unique key is enabled after two rows of key 1, so only the third row's duplicate is a finding, naming the
    // first row. Once that row goes, the second comes first, and the third is its duplicate.
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (ID NUMBER, V NUMBER);\n\
             CREATE INDEX T_I ON T (ID);\n\
             INSERT INTO T VALUES (1, 1);\nINSERT INTO T VALUES (1, 10);\n\
             ALTER TABLE T ADD CONSTRAINT T_U UNIQUE (ID) USING INDEX T_I ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (1, 20);\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(3, 1), (4, 2)], "{:#?}", report.removed);
    assert!(output(&report).ends_with("ENABLE NOVALIDATE;\nINSERT INTO T VALUES (1, 20);\n"));
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

#[test]
fn keep_last_leaves_a_key_whose_rows_it_cant_all_reach() {
    let ddl = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\n";
    let (_dir, _, report) = fix_scripts(
        &[
            ("ref/r.sql", "INSERT INTO T VALUES (1);\n"),
            ("t.sql", &format!("{ddl}INSERT INTO T VALUES (1);\n")),
        ],
        &Options::default(),
        keep_last(),
        false,
    );
    assert!(report.removed.is_empty(), "{:#?}", report.removed);
    assert_eq!(report.left.len(), 1);
    assert!(
        report.left[0].why.contains("reference file"),
        "{:?}",
        report.left
    );

    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            &format!(
                "{ddl}INSERT ALL INTO T VALUES (1) INTO T VALUES (2) SELECT 1 FROM DUAL;\n\
                 INSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\n"
            ),
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    assert!(report.removed.is_empty(), "{:#?}", report.removed);
    assert_eq!(report.left.len(), 2, "{:?}", report.left);
    assert!(
        report
            .left
            .iter()
            .all(|l| l.why.contains("shares its line"))
    );
}

#[test]
fn findings_on_dates_read_from_text_are_fixed_when_told() {
    let sql = "CREATE TABLE T (D DATE PRIMARY KEY);\nINSERT INTO T VALUES ('2024-01-02');\nINSERT INTO T VALUES (DATE '2024-01-02');\n";
    for (on_duplicate, line) in [
        (fix::OnDuplicate::KeepFirst, 3),
        (fix::OnDuplicate::KeepLast, 2),
    ] {
        let policy = Policy {
            on_duplicate,
            fix_assumed: true,
            ..Policy::default()
        };
        let (_dir, _, report) = fix_scripts(&[("t.sql", sql)], &Options::default(), policy, false);
        let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
        assert_eq!(removed, [line], "{on_duplicate:?}");
        assert!(
            report.left.is_empty(),
            "{on_duplicate:?}: {:?}",
            report.left
        );
        assert!(report.output.findings.is_empty(), "{on_duplicate:?}");

        let (_dir, _, report) = fix_scripts(
            &[("t.sql", sql)],
            &Options::default(),
            Policy {
                fix_assumed: false,
                ..policy
            },
            false,
        );
        assert!(report.removed.is_empty(), "{on_duplicate:?}");
        assert_eq!(report.left.len(), 1, "{on_duplicate:?}");
    }
}

#[test]
fn annotations_mark_each_removal_and_parse_as_comments_under_both_runners() {
    use sqlca::dialect::Runner;
    let ddl =
        "CREATE TABLE T (ID NUMBER CONSTRAINT T_PK PRIMARY KEY);\nINSERT INTO T VALUES (1);\n";
    let note = |what: &str| {
        format!("sqlca fix removed {what} here (duplicate key of T_PK); see rejects.sql")
    };
    let row = note("a row");
    let insert = note("an INSERT");
    let both = [Runner::SqlPlus, Runner::Sqlcl];
    let cases: [(&[Runner], String, String); 7] = [
        (
            &both,
            "INSERT ALL\nINTO T VALUES (1) -- note\nINTO T VALUES (2)\nSELECT 1 FROM DUAL;\n"
                .to_owned(),
            format!("INSERT ALL\n/* {row} */ -- note\nINTO T VALUES (2)\nSELECT 1 FROM DUAL;\n"),
        ),
        (
            &both,
            "INSERT INTO T VALUES (2);\nINSERT INTO T VALUES (1);".to_owned(),
            format!("INSERT INTO T VALUES (2);\n/* {insert} */"),
        ),
        (
            &both,
            "INSERT ALL\n  INTO T VALUES (1)\n  INTO T VALUES (2)\nSELECT 1 FROM DUAL;\n"
                .to_owned(),
            format!("INSERT ALL\n  -- {row}\n  INTO T VALUES (2)\nSELECT 1 FROM DUAL;\n"),
        ),
        (
            &both,
            "INSERT ALL\r\n  INTO T VALUES (2)\r\n  INTO T VALUES (1)\r\nSELECT 1 FROM DUAL;\r\n"
                .to_owned(),
            format!("INSERT ALL\r\n  INTO T VALUES (2)\r\n  -- {row}\r\nSELECT 1 FROM DUAL;\r\n"),
        ),
        (
            &both,
            "INSERT ALL INTO T VALUES (1)\n  INTO T VALUES (2) SELECT 1 FROM DUAL;\n".to_owned(),
            format!("INSERT ALL /* {row} */\n  INTO T VALUES (2) SELECT 1 FROM DUAL;\n"),
        ),
        (
            &both,
            "  INSERT INTO T VALUES (1);\nINSERT INTO T VALUES (2);\n".to_owned(),
            format!("  -- {insert}\nINSERT INTO T VALUES (2);\n"),
        ),
        (
            &[Runner::Sqlcl],
            "INSERT INTO T VALUES (1); COMMIT;\nINSERT INTO T VALUES (2);".to_owned(),
            format!("/* {insert} */ COMMIT;\nINSERT INTO T VALUES (2);"),
        ),
    ];
    for (runners, sql, expected) in cases {
        for runner in runners {
            // Some cases end without a line end, so they have no COMMIT.
            let options = Options {
                runner: *runner,
                commit_check: false,
                ..Options::default()
            };
            let input = format!("{ddl}{sql}");
            let (_dir, _, report) =
                fix_scripts(&[("t.sql", &input)], &options, Policy::default(), true);
            let at = format!("{runner:?} {sql:?}");
            assert_eq!(output(&report), format!("{ddl}{expected}"), "{at}");
            assert!(
                report.output.findings.is_empty(),
                "{at}: {:?}",
                report.output.findings
            );
            assert!(
                report.output.errors.is_empty(),
                "{at}: {:?}",
                report.output.errors
            );
            assert_eq!(report.output.rows, 2, "{at}");
        }
    }
}

#[test]
fn an_annotation_holds_no_newline_substitution_or_early_comment_end() {
    let sql = "SET DEFINE OFF\nCREATE TABLE T (ID NUMBER CONSTRAINT \"A*/B\nC&D\" PRIMARY KEY);\nSET DEFINE ON\n\
               INSERT INTO T VALUES (1);\nINSERT ALL INTO T VALUES (1)\n  INTO T VALUES (2) SELECT 1 FROM DUAL;\n";
    let (_dir, _, report) = fix_scripts(
        &[("t.sql", sql)],
        &Options::default(),
        Policy::default(),
        true,
    );
    let replacement = String::from_utf8(report.removed[0].replacement.clone()).unwrap();
    let inner = replacement
        .strip_prefix("/* ")
        .and_then(|r| r.strip_suffix(" */\n"))
        .unwrap();
    assert!(
        !inner.contains("*/") && !inner.contains('\n') && !inner.contains('&'),
        "{replacement}"
    );
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
    assert_eq!(report.output.rows, 2);
}

fn output(report: &FixReport) -> String {
    outputs(report).swap_remove(0)
}

const FAMILY: &str = "CREATE TABLE G (ID NUMBER PRIMARY KEY);\n\
                      CREATE TABLE P (ID NUMBER PRIMARY KEY, G_ID NUMBER REFERENCES G);\n\
                      CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
                      CREATE TABLE D (ID NUMBER PRIMARY KEY, C_ID NUMBER REFERENCES C);\n";

/// A grandchild, child and parent, the parent's own parent missing: each round orphans the next. Each table keeps a
/// valid row, so no parent table ends up empty, which would be a summary finding instead.
const CASCADE: &str = "INSERT INTO D VALUES (1, 1);\nINSERT INTO C VALUES (1, 10);\nINSERT INTO C VALUES (2, 20);\n\
                       INSERT INTO P VALUES (10, 99);\nINSERT INTO P VALUES (20, 1);\nINSERT INTO G VALUES (1);\n";

#[test]
fn a_removed_parent_takes_its_children_in_later_rounds() {
    let (_dir, _, report) = fix_sql(&format!("{FAMILY}{CASCADE}"));
    assert_eq!(report.rounds, 3, "{:#?}", report.removed);
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(5, 3), (6, 2), (8, 1)]);
    assert_eq!(
        output(&report),
        format!(
            "{FAMILY}INSERT INTO C VALUES (2, 20);\nINSERT INTO P VALUES (20, 1);\nINSERT INTO G VALUES (1);\n"
        )
    );
    assert!(report.output.findings.is_empty());
}

#[test]
fn rows_past_the_round_limit_are_left_for_review() {
    let fix = |max_rounds| {
        fix_scripts(
            &[("t.sql", &format!("{FAMILY}{CASCADE}"))],
            &Options::default(),
            Policy {
                max_rounds,
                ..Policy::default()
            },
            false,
        )
    };
    let (_dir, _, report) = fix(2);
    assert_eq!(report.rounds, 2);
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(6, 2), (8, 1)]);
    let left: Vec<(Option<u64>, &str)> = report
        .left
        .iter()
        .map(|l| (l.finding.line, l.why))
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left.first().unwrap().0, Some(5));
    assert!(left.first().unwrap().1.contains("--max-rounds"));
    assert_eq!(count(&report.output, FindingKind::FkMissingParent), 1);

    let (_dir, _, report) = fix(3);
    assert_eq!(report.rounds, 3);
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert!(report.output.findings.is_empty());
}

#[test]
fn a_duplicate_of_a_removed_row_is_kept() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE T (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
         INSERT INTO T VALUES (1, NULL);\nINSERT INTO T VALUES (1, 5);\n",
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [2]);
    assert!(output(&report).ends_with("INSERT INTO T VALUES (1, 5);\n"));
    assert!(report.output.findings.is_empty());
}

/// Nothing going can change which row of the key stays: the NULL row is no key's first and no row's parent. So the
/// duplicate goes in the same round, and the fix needs no second check.
#[test]
fn a_duplicate_goes_with_the_other_rows_when_no_row_going_can_change_its_key() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE T (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
         INSERT INTO T VALUES (1, 5);\nINSERT INTO T VALUES (1, 6);\nINSERT INTO T VALUES (2, NULL);\n",
    );
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(3, 1), (4, 1)]);
    assert_eq!(report.rounds, 1);
    assert!(report.output.findings.is_empty());
}

/// The parent going for its NULL takes its child, the first row of `U`, a round later; the duplicate waits for that,
/// and is then the row of `U` to keep.
#[test]
fn a_duplicate_waits_while_a_row_going_can_take_its_first_row_with_it() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, U VARCHAR2(9) UNIQUE);\n\
         INSERT INTO P VALUES (1, NULL);\nINSERT INTO P VALUES (2, 5);\n\
         INSERT INTO C VALUES (1, 1, 'x');\nINSERT INTO C VALUES (2, 2, 'x');\n",
    );
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(3, 1), (5, 2)]);
    assert!(output(&report).ends_with("INSERT INTO C VALUES (2, 2, 'x');\n"));
    assert!(report.output.findings.is_empty());
}

/// Under keep-last, both rows of parent 1 go for their NULL, and the first is also the duplicate keep-last would
/// remove; that must not hide that no row of key 1 stays. Child 2 then loses its parent, so the duplicates of `U`
/// wait, and child 1 is the one to keep.
#[test]
fn keep_last_duplicates_wait_while_every_row_of_a_parent_key_goes() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE P (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
             CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, U NUMBER UNIQUE);\n\
             INSERT INTO P VALUES (1, NULL);\nINSERT INTO P VALUES (1, NULL);\nINSERT INTO P VALUES (2, 5);\n\
             INSERT INTO C VALUES (1, 2, 9);\nINSERT INTO C VALUES (2, 1, 9);\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [3, 4, 7]);
    assert!(output(&report).contains("INSERT INTO C VALUES (1, 2, 9);"));
    assert!(report.output.findings.is_empty());
}

/// Under keep-last, the row with the NULL is the first of `A`, whose key is enabled after the row of `A` 1 and `B` 9
/// that only shows once the first goes. That row then goes as `A`'s duplicate, so the duplicates of `B` wait for it,
/// and the other row of `B` 9 stays.
#[test]
fn keep_last_duplicates_wait_while_a_first_row_goes_for_another_reason() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (A NUMBER, B NUMBER UNIQUE, V NUMBER NOT NULL);\nCREATE INDEX T_I ON T (A);\n\
             INSERT INTO T VALUES (2, 9, 1);\nINSERT INTO T VALUES (1, 7, NULL);\nINSERT INTO T VALUES (1, 9, 1);\n\
             ALTER TABLE T ADD CONSTRAINT T_A UNIQUE (A) USING INDEX T_I ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (1, 8, 1);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [4, 5]);
    assert!(output(&report).contains("INSERT INTO T VALUES (2, 9, 1);"));
    assert!(report.output.findings.is_empty());
}

/// Under keep-last, removing a key's first row as a duplicate can bring up a row the key between rows hid, the
/// first of another key, while a child of a removed row still waits to go: the duplicates wait while any other row
/// goes, so the row of `A` 0 and `B` 0 stays.
#[test]
fn keep_last_duplicates_wait_while_any_other_row_goes() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (A NUMBER, B NUMBER UNIQUE, P NUMBER, V NUMBER NOT NULL);\nCREATE INDEX T_I ON T (A);\n\
             INSERT INTO T VALUES (1, 1, NULL, 3);\nINSERT INTO T VALUES (0, 3, NULL, 2);\n\
             INSERT INTO T VALUES (0, 0, 3, 2);\nINSERT INTO T VALUES (2, 1, 3, 0);\n\
             ALTER TABLE T ADD CONSTRAINT T_A UNIQUE (A) USING INDEX T_I ENABLE NOVALIDATE;\n\
             ALTER TABLE T ADD CONSTRAINT T_F FOREIGN KEY (P) REFERENCES T (A);\n\
             INSERT INTO T VALUES (0, NULL, 1, 3);\nINSERT INTO T VALUES (1, 1, 0, NULL);\n\
             INSERT INTO T VALUES (3, 3, NULL, 0);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    assert!(output(&report).contains("INSERT INTO T VALUES (0, 0, 3, 2);"));
    assert!(report.output.findings.is_empty());
}

/// The row with the NULL is also a duplicate whose key rests on a date written as text, left for review; once the
/// row goes, nothing of it is left for review, whether the fix ends early or checks once more on a dry run.
#[test]
fn a_removed_row_leaves_nothing_for_review() {
    let sql = "CREATE TABLE T (ID NUMBER PRIMARY KEY, D DATE UNIQUE, V NUMBER NOT NULL);\n\
               INSERT INTO T VALUES (1, DATE '2024-01-02', 1);\nINSERT INTO T VALUES (2, '2024-01-02', NULL);\n\
               INSERT INTO T VALUES (1, DATE '2024-01-03', 1);\nCOMMIT;\n";
    let (_dir, _, report) = fix_sql(sql);
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [3, 4]);
    assert!(report.left.is_empty(), "{:?}", report.left);
    let (dir, files) = write_scripts(&[("t.sql", sql)]);
    let dry = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &fix::Target {
            dry_run: true,
            ..fix::Target::out(dir.path().join("out"))
        },
    )
    .unwrap();
    assert!(dry.left.is_empty(), "{:?}", dry.left);
}

/// Only duplicates go: parent 2 for its name, and child 2 for `U`, which would keep child 1. But child 1's parent is
/// parent 2, so the duplicates of `U` wait; child 1 goes with its parent, and child 2 is the one to keep.
#[test]
fn duplicates_wait_while_a_duplicate_going_takes_the_parent_of_the_row_they_keep() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, NAME VARCHAR2(9) UNIQUE);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, U VARCHAR2(9) UNIQUE);\n\
         INSERT INTO P VALUES (1, 'a');\nINSERT INTO P VALUES (2, 'a');\n\
         INSERT INTO C VALUES (1, 2, 'x');\nINSERT INTO C VALUES (2, 1, 'x');\nCOMMIT;\n",
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [4, 5]);
    assert!(output(&report).contains("INSERT INTO C VALUES (2, 1, 'x');"));
    assert!(report.output.findings.is_empty());
}

/// The report says when a key kept another row than its policy's, because that row went for another reason: here
/// `U`'s first row, child 1, lost its parent, so child 2 stays. In the text report, the JSON and the summary.
#[test]
fn the_report_tells_a_key_that_kept_another_row_than_its_first() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, NAME VARCHAR2(9) UNIQUE);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, U VARCHAR2(9) UNIQUE);\n\
         INSERT INTO P VALUES (1, 'a');\nINSERT INTO P VALUES (2, 'a');\n\
         INSERT INTO C VALUES (1, 2, 'x');\nINSERT INTO C VALUES (2, 1, 'x');\nCOMMIT;\n",
    );
    let kept: Vec<(u64, Option<u64>, bool)> = report
        .kept
        .iter()
        .map(|k| (k.policy.line, k.kept.as_ref().map(|l| l.line), k.first))
        .collect();
    assert_eq!(kept, [(5, Some(6), true)]);
    assert_eq!(
        report.kept[0].why,
        ["missing parent for a foreign key of C"]
    );
    let mut text = Vec::new();
    fix::write_report(&report, &mut text).unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(
        text.contains("Kept another row (duplicates are decided among the rows that stay):\n"),
        "{text}"
    );
    assert!(
        text.contains(":6; its first row, ")
            && text.contains(":5, went (missing parent for a foreign key of C)"),
        "{text}"
    );
    assert!(fix::summary(&report).contains("; 1 key kept another row"));
    let mut json = Vec::new();
    fix::write_json_report(&report, &mut json).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(json["kept_another_row"][0]["policy"], "first");
    assert_eq!(json["kept_another_row"][0]["kept"]["line"], 6);
}

/// Under keep-last the last row of a key goes for its NULL, so the one before it stays; when every row of a key goes,
/// the report says that none stays.
#[test]
fn the_report_tells_a_key_that_kept_another_row_than_its_last_or_none() {
    let ddl = "CREATE TABLE T (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n";
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            &format!(
                "{ddl}INSERT INTO T VALUES (1, 5);\nINSERT INTO T VALUES (1, NULL);\nCOMMIT;\n"
            ),
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let kept: Vec<(u64, Option<u64>, bool)> = report
        .kept
        .iter()
        .map(|k| (k.policy.line, k.kept.as_ref().map(|l| l.line), k.first))
        .collect();
    assert_eq!(kept, [(3, Some(2), false)]);
    let (_dir, _, report) = fix_sql(&format!(
        "{ddl}INSERT INTO T VALUES (1, NULL);\nINSERT INTO T VALUES (1, NULL);\nCOMMIT;\n"
    ));
    assert_eq!(report.kept.len(), 1);
    assert_eq!(report.kept[0].kept, None);
    let mut text = Vec::new();
    fix::write_report(&report, &mut text).unwrap();
    assert!(
        String::from_utf8(text)
            .unwrap()
            .contains(": no row of it that a check found stays; its first row, ")
    );
}

/// Both rows of a key in one `INSERT ALL` go, so the statement goes whole: the report says no row of the key stays,
/// though the removal names only the statement's first line.
#[test]
fn the_report_tells_a_key_whose_rows_all_went_with_their_insert_all() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE T (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\n\
         INSERT ALL\n  INTO T VALUES (1, NULL)\n  INTO T VALUES (1, NULL)\nSELECT * FROM dual;\nCOMMIT;\n",
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [2]);
    let kept: Vec<(u64, Option<u64>)> = report
        .kept
        .iter()
        .map(|k| (k.policy.line, k.kept.as_ref().map(|l| l.line)))
        .collect();
    assert_eq!(kept, [(3, None)]);
}

/// The key on `ID` is enabled after two rows, so the input's duplicate names only the first, which goes for its NULL,
/// and the row after the key. A later round finds the second row, which the key never checked: that row stays, and
/// the report names it.
#[test]
fn the_report_names_a_kept_row_a_later_round_found() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE T (ID NUMBER, V NUMBER NOT NULL);\nCREATE INDEX T_I ON T (ID);\n\
         INSERT INTO T VALUES (1, NULL);\nINSERT INTO T VALUES (1, 5);\n\
         ALTER TABLE T ADD CONSTRAINT T_U UNIQUE (ID) USING INDEX T_I ENABLE NOVALIDATE;\n\
         INSERT INTO T VALUES (1, 6);\nCOMMIT;\n",
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [3, 6]);
    let kept: Vec<(u64, Option<u64>)> = report
        .kept
        .iter()
        .map(|k| (k.policy.line, k.kept.as_ref().map(|l| l.line)))
        .collect();
    assert_eq!(kept, [(3, Some(4))]);
}

/// As above, with the row after the key also NULL: the row before it, which the key never checked, stays, and the
/// report names it, from the duplicate's rows the key never checked.
#[test]
fn the_report_names_a_kept_row_the_key_never_checked() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE T (ID NUMBER, V NUMBER NOT NULL);\nCREATE INDEX T_I ON T (ID);\n\
         INSERT INTO T VALUES (1, NULL);\nINSERT INTO T VALUES (1, 5);\n\
         ALTER TABLE T ADD CONSTRAINT T_U UNIQUE (ID) USING INDEX T_I ENABLE NOVALIDATE;\n\
         INSERT INTO T VALUES (1, NULL);\nCOMMIT;\n",
    );
    assert!(output(&report).contains("INSERT INTO T VALUES (1, 5);"));
    let kept: Vec<(u64, Option<u64>)> = report
        .kept
        .iter()
        .map(|k| (k.policy.line, k.kept.as_ref().map(|l| l.line)))
        .collect();
    assert_eq!(kept, [(3, Some(4))]);
}

/// Under SQLcl a statement can end on the line where the next begins. The removed row of `ID` 9 ends on the line of
/// the first row of `ID` 1, which stays: the report names no key as having kept another row.
#[test]
fn a_removal_ending_on_a_kept_rows_line_does_not_take_it() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (ID NUMBER PRIMARY KEY, V NUMBER NOT NULL);\nINSERT INTO T VALUES (9,\n\
             NULL); INSERT INTO T VALUES (1, 5);\nINSERT INTO T VALUES (1, 6);\nCOMMIT;\n",
        )],
        &Options {
            runner: sqlca::dialect::Runner::Sqlcl,
            ..Options::default()
        },
        Policy::default(),
        false,
    );
    assert_eq!(report.removed.len(), 2, "{:#?}", report.removed);
    assert!(report.kept.is_empty(), "{:?}", report.kept);
}

/// As above, with a grandchild under child 2, whose row also goes as `C`'s duplicate on `U`, and could orphan the
/// grandchild: child 2 must not go before child 1, whose parent goes first. Then child 2 is `U`'s row, and the
/// grandchild keeps its parent.
#[test]
fn a_duplicate_that_can_orphan_waits_while_its_kept_row_can_lose_its_parent() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, NAME VARCHAR2(9) UNIQUE);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P, U VARCHAR2(9) UNIQUE);\n\
         CREATE TABLE G (ID NUMBER PRIMARY KEY, C_ID NUMBER REFERENCES C);\n\
         INSERT INTO P VALUES (1, 'a');\nINSERT INTO P VALUES (2, 'a');\n\
         INSERT INTO C VALUES (1, 2, 'x');\nINSERT INTO C VALUES (2, 1, 'x');\nINSERT INTO G VALUES (1, 2);\nCOMMIT;\n",
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [5, 6]);
    let out = output(&report);
    assert!(out.contains("INSERT INTO C VALUES (2, 1, 'x');"), "{out}");
    assert!(out.contains("INSERT INTO G VALUES (1, 2);"), "{out}");
    assert!(report.output.findings.is_empty());
}

/// Parent 2 goes for its name, which takes child 1, and then grandchild 1, a round later each. Grandchild 1 is also
/// the first row of `U` in `G`, so the duplicate of `U` waits, though `G` points at `C`, not at `P`: then it is the
/// row to keep.
#[test]
fn a_duplicate_waits_while_its_kept_row_can_lose_a_grandparent() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, NAME VARCHAR2(9) UNIQUE);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
         CREATE TABLE G (ID NUMBER PRIMARY KEY, C_ID NUMBER REFERENCES C, U VARCHAR2(9) UNIQUE);\n\
         INSERT INTO P VALUES (1, 'a');\nINSERT INTO P VALUES (2, 'a');\n\
         INSERT INTO C VALUES (1, 2);\nINSERT INTO C VALUES (2, 1);\n\
         INSERT INTO G VALUES (1, 1, 'x');\nINSERT INTO G VALUES (2, 2, 'x');\nCOMMIT;\n",
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [5, 6, 8]);
    assert!(output(&report).contains("INSERT INTO G VALUES (2, 2, 'x');"));
    assert!(report.output.findings.is_empty());
}

/// Under keep-last, the key on `A` is enabled after two rows of `A` 1, so its duplicate names the first and, as a row
/// the key never checked, the second. That one goes once it comes first, a round later; it is also `B` 9's last row,
/// so `B` waits for it, and its other row stays.
#[test]
fn keep_last_waits_while_its_last_row_is_a_row_another_key_never_checked() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (A NUMBER, B NUMBER UNIQUE, V NUMBER NOT NULL);\nCREATE INDEX T_I ON T (A);\n\
             INSERT INTO T VALUES (2, 9, 1);\nINSERT INTO T VALUES (1, 7, 1);\nINSERT INTO T VALUES (1, 9, 1);\n\
             ALTER TABLE T ADD CONSTRAINT T_A UNIQUE (A) USING INDEX T_I ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (1, 8, 1);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(4, 1), (5, 2)]);
    let out = output(&report);
    assert!(out.contains("INSERT INTO T VALUES (2, 9, 1);"), "{out}");
    assert!(out.contains("INSERT INTO T VALUES (1, 8, 1);"), "{out}");
    assert!(report.output.findings.is_empty());
}

/// Under keep-last, `P`'s key on `U` names its first row and an unchecked second, and keeps the third. But the third's
/// parent `C` 10 has the first row as its parent: the first goes alone, then `C` 10, then the third, and the second
/// is the row of `U` 1 left.
#[test]
fn keep_last_keeps_an_unchecked_row_while_its_kept_row_can_lose_a_parent_further_up() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE P (ID NUMBER PRIMARY KEY, U NUMBER, C_ID NUMBER);\n\
             CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
             ALTER TABLE P ADD CONSTRAINT P_C FOREIGN KEY (C_ID) REFERENCES C;\nCREATE INDEX P_I ON P (U);\n\
             INSERT INTO P VALUES (1, 1, NULL);\nINSERT INTO P VALUES (2, 1, NULL);\n\
             ALTER TABLE P ADD CONSTRAINT P_U UNIQUE (U) USING INDEX P_I ENABLE NOVALIDATE;\n\
             INSERT INTO C VALUES (10, 1);\nINSERT INTO P VALUES (3, 1, 10);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let out = output(&report);
    assert!(out.contains("INSERT INTO P VALUES (2, 1, NULL);"), "{out}");
    assert!(report.output.findings.is_empty());
}

/// As above, with a key the check can't know on `P`: a generated identity column that the rows leave out. Any row of
/// `P` going may then orphan one, but the second row of `U` 1 still stays.
#[test]
fn keep_last_keeps_an_unchecked_row_when_its_table_has_keys_the_check_cant_know() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE P (ID NUMBER PRIMARY KEY, U NUMBER, C_ID NUMBER, X NUMBER GENERATED ALWAYS AS IDENTITY UNIQUE);\n\
             CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
             ALTER TABLE P ADD CONSTRAINT P_C FOREIGN KEY (C_ID) REFERENCES C;\nCREATE INDEX P_I ON P (U);\n\
             INSERT INTO P (ID, U, C_ID) VALUES (1, 1, NULL);\nINSERT INTO P (ID, U, C_ID) VALUES (2, 1, NULL);\n\
             ALTER TABLE P ADD CONSTRAINT P_U UNIQUE (U) USING INDEX P_I ENABLE NOVALIDATE;\n\
             INSERT INTO C VALUES (10, 1);\nINSERT INTO P (ID, U, C_ID) VALUES (3, 1, 10);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let out = output(&report);
    assert!(
        out.contains("INSERT INTO P (ID, U, C_ID) VALUES (2, 1, NULL);"),
        "{out}"
    );
    assert!(report.output.findings.is_empty());
}

/// Under keep-last, `A`'s key with an unchecked row keeps line 6, which `B`'s key removes: `A` waits for that, so it
/// holds no other key of the table back, and `B` goes. Then nothing is left to fix.
#[test]
fn a_key_with_unchecked_rows_that_waits_holds_no_other_key_back() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (A NUMBER, B NUMBER UNIQUE);\nCREATE INDEX T_I ON T (A);\n\
             INSERT INTO T VALUES (1, 1);\nINSERT INTO T VALUES (1, 2);\n\
             ALTER TABLE T ADD CONSTRAINT T_A UNIQUE (A) USING INDEX T_I ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (1, 3);\nINSERT INTO T VALUES (2, 3);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let removed: Vec<u64> = report.removed.iter().map(|r| r.line).collect();
    assert_eq!(removed, [6]);
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

/// Two keys enabled without validation on one table, with rows before each. Keep-last's row for `B` 0 is one of the
/// rows `A` 4's key never checked, which goes once it comes first, so `B` waits for it, and a row of `B` 0 stays.
#[test]
fn keep_last_waits_for_the_rows_two_keys_never_checked() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (A NUMBER, B NUMBER);\nCREATE INDEX T_AI ON T (A);\nCREATE INDEX T_BI ON T (B);\n\
             INSERT INTO T VALUES (1, 0);\nINSERT INTO T VALUES (4, 2);\nINSERT INTO T VALUES (1, 0);\n\
             ALTER TABLE T ADD CONSTRAINT T_B UNIQUE (B) USING INDEX T_BI ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (4, 0);\n\
             ALTER TABLE T ADD CONSTRAINT T_A UNIQUE (A) USING INDEX T_AI ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (4, 4);\nINSERT INTO T VALUES (0, 4);\nINSERT INTO T VALUES (0, 2);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let out = output(&report);
    let rows: Vec<&str> = out.lines().filter(|l| l.starts_with("INSERT")).collect();
    assert_eq!(
        rows,
        [
            "INSERT INTO T VALUES (1, 0);",
            "INSERT INTO T VALUES (4, 4);",
            "INSERT INTO T VALUES (0, 2);"
        ]
    );
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
}

/// Keep-last keeps the last row of each key that stays. `A` 3's last row, r6, is one of the rows `B` 1's key never
/// checked, and goes for `B`; so `A` waits, and keeps r5, its last row that stays. Keeping r6 instead would have kept
/// more rows here, by luck of r6 going later; the report says `A` 3 kept another row.
#[test]
fn keep_last_keeps_the_last_row_that_stays_even_when_that_keeps_fewer_rows() {
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            "CREATE TABLE T (A NUMBER, B NUMBER);\nCREATE INDEX T_AI ON T (A);\nCREATE INDEX T_BI ON T (B);\n\
             INSERT INTO T VALUES (3, 3);\nINSERT INTO T VALUES (3, 0);\nINSERT INTO T VALUES (3, 3);\n\
             ALTER TABLE T ADD CONSTRAINT T_A UNIQUE (A) USING INDEX T_AI ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (3, 1);\nINSERT INTO T VALUES (3, 0);\nINSERT INTO T VALUES (3, 1);\n\
             ALTER TABLE T ADD CONSTRAINT T_B UNIQUE (B) USING INDEX T_BI ENABLE NOVALIDATE;\n\
             INSERT INTO T VALUES (2, 1);\nCOMMIT;\n",
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    let out = output(&report);
    let rows: Vec<&str> = out.lines().filter(|l| l.starts_with("INSERT")).collect();
    assert_eq!(
        rows,
        [
            "INSERT INTO T VALUES (3, 0);",
            "INSERT INTO T VALUES (2, 1);"
        ]
    );
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
    let kept: Vec<(u64, Option<u64>)> = report
        .kept
        .iter()
        .map(|k| (k.policy.line, k.kept.as_ref().map(|l| l.line)))
        .collect();
    assert!(kept.contains(&(10, Some(9))), "{kept:?}");
}

/// More rows a key never checked than the check keeps for one key value: keep-last can't know them all, so it leaves
/// the key for review.
#[test]
fn keep_last_leaves_a_key_with_more_unchecked_rows_than_it_tracks_for_review() {
    let before = "INSERT INTO T VALUES (1);\n".repeat(70);
    let (_dir, _, report) = fix_scripts(
        &[(
            "t.sql",
            &format!(
                "CREATE TABLE T (ID NUMBER);\nCREATE INDEX T_I ON T (ID);\n{before}\
                 ALTER TABLE T ADD CONSTRAINT T_U UNIQUE (ID) USING INDEX T_I ENABLE NOVALIDATE;\n\
                 INSERT INTO T VALUES (1);\nCOMMIT;\n"
            ),
        )],
        &Options::default(),
        keep_last(),
        false,
    );
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(report.left.len(), 1);
    assert!(report.left[0].finding.unchecked_more);
    assert_eq!(report.left[0].finding.unchecked.len(), 64);
}

/// Under `--on-fk null` the duplicate's own foreign key goes to NULL, and with it the column its unique key shares, so
/// it is no duplicate any more: it waits for that, and stays.
#[test]
fn a_duplicate_waits_while_a_foreign_key_set_to_null_can_change_its_key() {
    let (_dir, _, report) = fix_null(
        "CREATE TABLE P (A NUMBER, B NUMBER, PRIMARY KEY (A, B));\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, A NUMBER UNIQUE, B NUMBER, FOREIGN KEY (A, B) REFERENCES P);\n\
         INSERT INTO P VALUES (1, 1);\nINSERT INTO C VALUES (1, 1, 1);\nINSERT INTO C VALUES (2, 1, 9);\n",
    );
    let changed: Vec<(u64, bool)> = report
        .removed
        .iter()
        .map(|r| (r.line, r.nulled.is_empty()))
        .collect();
    assert_eq!(changed, [(5, false)], "row 2 is kept with NULL in A and B");
    assert!(report.output.findings.is_empty());
}

#[test]
fn a_duplicate_on_another_key_takes_the_children_of_its_primary_key() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, NAME VARCHAR2(9) UNIQUE);\n\
         CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
         INSERT INTO P VALUES (1, 'a');\nINSERT INTO P VALUES (2, 'a');\nINSERT INTO C VALUES (1, 2);\n",
    );
    let removed: Vec<(u64, usize)> = report.removed.iter().map(|r| (r.line, r.round)).collect();
    assert_eq!(removed, [(4, 1), (5, 2)]);
    assert!(report.output.findings.is_empty());
}

#[test]
fn what_the_output_still_has_is_told_at_the_output_even_when_nothing_was_removed() {
    let (_dir, files, report) = fix_sql(
        "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nINSERT INTO U VALUES (1);\n",
    );
    assert_eq!(report.rounds, 0);
    let place = |r: &check::Report| {
        r.findings
            .iter()
            .map(|f| f.file.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(place(&report.input), [Some(check::display(&files[0].path))]);
    assert_eq!(
        place(&report.output),
        [Some(check::display(&report.files[0].path))]
    );
}

#[test]
fn an_insert_all_loses_its_bad_rows_and_goes_whole_when_none_is_left() {
    let ddl = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\n";
    let (_dir, _, report) = fix_sql(&format!(
        "{ddl}INSERT ALL\n  INTO T VALUES (1)\n  INTO T VALUES (1)\n  INTO T VALUES (2)\nSELECT 1 FROM DUAL;\n"
    ));
    assert_eq!(
        output(&report),
        format!("{ddl}INSERT ALL\n  INTO T VALUES (1)\n  INTO T VALUES (2)\nSELECT 1 FROM DUAL;\n")
    );
    assert!(report.removed.iter().all(|r| r.clause));

    let (_dir, _, report) = fix_sql(&format!(
        "{ddl}INSERT INTO T VALUES (1);\nINSERT ALL\n  INTO T VALUES (1)\n  INTO T VALUES (1)\nSELECT 1 FROM DUAL;\nCOMMIT;\n"
    ));
    assert_eq!(
        output(&report),
        format!("{ddl}INSERT INTO T VALUES (1);\nCOMMIT;\n")
    );
    assert_eq!(report.removed.len(), 1, "{:#?}", report.removed);
    assert!(!report.removed[0].clause);
}

#[test]
fn rows_sharing_a_line_are_left_for_review() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE T (ID NUMBER PRIMARY KEY);\n\
         INSERT INTO T VALUES (1);\nINSERT ALL INTO T VALUES (1) INTO T VALUES (2) SELECT 1 FROM DUAL;\n",
    );
    assert!(report.removed.is_empty());
    assert_eq!(report.left.len(), 1);
    assert!(report.left[0].why.contains("several rows"));
}

#[test]
fn rows_a_later_round_leaves_are_left_for_review() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, N NUMBER NOT NULL);\n\
         CREATE TABLE C (ID NUMBER, P_ID NUMBER REFERENCES P);\n\
         INSERT INTO P VALUES (1, NULL);\nINSERT INTO P VALUES (2, 2);\n\
         INSERT ALL INTO C VALUES (1, 1) INTO C VALUES (2, 1) SELECT 1 FROM DUAL;\n",
    );
    assert_eq!(report.removed.len(), 1, "{:#?}", report.removed);
    assert_eq!(count(&report.output, FindingKind::FkMissingParent), 2);
    assert_eq!(report.left.len(), 2, "{:#?}", report.left);
    assert!(
        report
            .left
            .iter()
            .all(|l| l.finding.kind == FindingKind::FkMissingParent
                && l.why.contains("several rows"))
    );
}

#[test]
fn a_child_goes_with_the_last_row_of_its_parent_table() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, N NUMBER NOT NULL);\n\
         CREATE TABLE C (ID NUMBER, P_ID NUMBER REFERENCES P);\n\
         INSERT INTO P VALUES (1, NULL);\nINSERT INTO C VALUES (1, 1);\n",
    );
    assert_eq!(report.removed.len(), 2, "{:#?}", report.removed);
    assert!(
        report.output.findings.is_empty(),
        "{:#?}",
        report.output.findings
    );
}

#[test]
fn rows_left_for_review_stay_when_a_later_round_empties_their_parent_table() {
    let (dir, files) = write_scripts(&[(
        "t.sql",
        "CREATE TABLE P (ID NUMBER PRIMARY KEY, N NUMBER NOT NULL);\n\
         CREATE TABLE C (ID NUMBER, P_ID NUMBER REFERENCES P);\n\
         INSERT INTO P VALUES (1, NULL);\n\
         INSERT ALL INTO C VALUES (1, 9) INTO C VALUES (2, 9) SELECT 1 FROM DUAL;\n",
    )]);
    let out = dir.path().join("out");
    for dry_run in [true, false] {
        let report = fix::fix(
            &files,
            &Options::default(),
            Policy::default(),
            &fix::Target {
                dry_run,
                ..fix::Target::out(&out)
            },
        )
        .unwrap();
        assert_eq!(report.removed.len(), 1, "{:#?}", report.removed);
        assert_eq!(
            report.left.len(),
            2,
            "dry run {dry_run}: {:#?}",
            report.left
        );
        assert!(
            report
                .left
                .iter()
                .all(|l| l.finding.kind == FindingKind::FkMissingParent)
        );
    }
}

#[test]
fn findings_on_dates_read_from_text_are_left_for_review() {
    let (_dir, _, report) = fix_sql(
        "CREATE TABLE T (D DATE PRIMARY KEY);\nINSERT INTO T VALUES ('2024-01-02');\nINSERT INTO T VALUES (DATE '2024-01-02');\n",
    );
    assert!(report.removed.is_empty());
    assert_eq!(report.left.len(), 1);
    assert_eq!(count(&report.output, FindingKind::DuplicateKey), 1);
}

#[test]
fn the_originals_are_never_changed_even_through_hard_links() {
    let sql = format!("{FAMILY}{CASCADE}");
    let (_dir, files, report) = fix_sql(&sql);
    assert!(report.rounds > 1);
    assert_eq!(
        fs::read_to_string(&files[0].path).unwrap(),
        format!("{sql}{COMMIT}")
    );
}

/// The scripts as data files in one folder, by name, fixed into `out` under the same temporary folder.
fn fix_files(
    scripts: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    Vec<InputFile>,
    Result<FixReport, fix::FixError>,
) {
    let (dir, files) = write_scripts(scripts);
    let report = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &fix::Target::out(dir.path().join("out")),
    );
    (dir, files, report)
}

#[test]
fn an_insert_all_that_lost_a_row_goes_whole_when_a_later_round_orphans_the_rest() {
    let ddl = "CREATE TABLE G (ID NUMBER PRIMARY KEY);\n\
               CREATE TABLE P (ID NUMBER PRIMARY KEY, G_ID NUMBER REFERENCES G);\n\
               CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
               INSERT INTO G VALUES (1);\nINSERT INTO P VALUES (6, 1);\nINSERT INTO C VALUES (3, 6);\n";
    let (_dir, _, report) = fix_sql(&format!(
        "{ddl}INSERT INTO P VALUES (5, 99);\nINSERT ALL\n  INTO C VALUES (1, 77)\n  INTO C VALUES (2, 5)\nSELECT 1 FROM DUAL;\n"
    ));
    assert_eq!(report.rounds, 2, "{:#?}", report.removed);
    assert_eq!(output(&report), ddl);
    assert_eq!(
        report.removed.len(),
        2,
        "the clause cut in round 1 joins its statement: {:#?}",
        report.removed
    );
    assert!(report.removed.iter().all(|r| !r.clause));
}

#[test]
fn temporary_files_never_take_an_inputs_name() {
    let bad = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\n";
    let other = "COMMIT;\n";
    let also_bad = bad.replace(" T ", " U ");
    let (_dir, files, report) = fix_files(&[
        ("t.sql", bad),
        (".t.sql.sqlca-0.tmp", other),
        ("u.sqlca-tmp", &also_bad),
    ]);
    let report = report.unwrap();
    assert_eq!(report.removed.len(), 2);
    let originals: Vec<String> = files
        .iter()
        .map(|f| fs::read_to_string(&f.path).unwrap())
        .collect();
    assert_eq!(
        originals,
        [bad, other, also_bad.as_str()].map(|s| format!("{s}{COMMIT}"))
    );
    assert_eq!(outputs(&report)[1], other);
}

#[test]
fn an_input_where_the_report_goes_is_refused() {
    let (_dir, files, report) = fix_files(&[
        ("t.sql", "COMMIT;\n"),
        ("sqlca-report/rejects.sql", "COMMIT;\n"),
    ]);
    let error = report.unwrap_err();
    assert!(
        error.to_string().contains("where the report goes"),
        "{error}"
    );
    assert!(
        files
            .iter()
            .all(|f| fs::read_to_string(&f.path).unwrap() == format!("COMMIT;\n{COMMIT}"))
    );
}

#[test]
fn an_output_folder_that_isnt_empty_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sql");
    fs::write(&path, "COMMIT;\n").unwrap();
    let files = [InputFile {
        path,
        role: Role::Data,
        bytes: 8,
        order: OrderSource::Given,
        includes: Vec::new(),
    }];
    let error = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &fix::Target::out(dir.path()),
    )
    .unwrap_err();
    assert!(error.to_string().contains("isn't empty"), "{error}");
}

#[test]
fn an_output_folder_that_leads_back_to_the_inputs_is_refused() {
    let (dir, files) = two_files();
    let error = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &fix::Target {
            report: fix::ReportPlace::Off,
            ..fix::Target::out(dir.path().join("new/.."))
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("isn't empty"), "{error}");
    assert_eq!(fs::read_to_string(&files[0].path).unwrap(), ORPHAN);
    assert!(!dir.path().join("new").exists());
}

#[test]
fn a_failed_backup_leaves_every_input_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let late = "INSERT INTO C VALUES (3, 8);\n";
    let files: Vec<InputFile> = [("a.sql", ORPHAN), ("b.sql", late)]
        .into_iter()
        .map(|(name, sql)| {
            let path = dir.path().join(name);
            fs::write(&path, sql).unwrap();
            InputFile {
                path,
                role: Role::Data,
                bytes: u64::try_from(sql.len()).unwrap(),
                order: OrderSource::Given,
                includes: Vec::new(),
            }
        })
        .collect();
    // The report folder takes the second input's backup name.
    let error = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &in_place(
            Some(".orig"),
            fix::ReportPlace::At(dir.path().join("b.sql.orig")),
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
    assert_eq!(fs::read_to_string(&files[0].path).unwrap(), ORPHAN);
    assert_eq!(fs::read_to_string(&files[1].path).unwrap(), late);
}

#[cfg(unix)]
#[test]
fn an_output_folder_that_cant_be_listed_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, files) = two_files();
    let mode = |m| fs::set_permissions(dir.path(), fs::Permissions::from_mode(m)).unwrap();
    mode(0o300);
    let result = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &fix::Target {
            report: fix::ReportPlace::Off,
            ..fix::Target::out(dir.path())
        },
    );
    mode(0o700);
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&files[0].path).unwrap(), ORPHAN);
}

#[test]
fn outputs_keep_the_inputs_relative_paths() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gen/oracle/separate-sqldev");
    let files = inputs(&root);
    let (dir, report) = run(&files, &Options::default());
    let relative: BTreeMap<PathBuf, PathBuf> = report
        .files
        .iter()
        .map(|f| &f.path)
        .zip(&files)
        .map(|(w, f)| {
            (
                w.strip_prefix(fs::canonicalize(dir.path().join("fixed")).unwrap())
                    .unwrap()
                    .to_owned(),
                f.path.strip_prefix(&root).unwrap().to_owned(),
            )
        })
        .collect();
    for (written, given) in relative {
        assert_eq!(written, given);
    }
}

const ORPHAN: &str = "CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
                      CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
                      INSERT INTO P VALUES (1);\nINSERT INTO C VALUES (1, 1);\nINSERT INTO C VALUES (2, 9);\n\
                      COMMIT;\n";
const CLEAN: &str = "INSERT INTO P VALUES (2);\nCOMMIT;\n";

/// `t.sql` with an orphan and `u.sql` with nothing to fix, in a new folder.
#[expect(clippy::unwrap_used, reason = "a folder that fails fails the test")]
fn two_files() -> (tempfile::TempDir, Vec<InputFile>) {
    let dir = tempfile::tempdir().unwrap();
    let files = [("t.sql", ORPHAN), ("u.sql", CLEAN)]
        .into_iter()
        .map(|(name, sql)| {
            let path = dir.path().join(name);
            fs::write(&path, sql).unwrap();
            InputFile {
                path,
                role: Role::Data,
                bytes: u64::try_from(sql.len()).unwrap(),
                order: OrderSource::Given,
                includes: Vec::new(),
            }
        })
        .collect();
    (dir, files)
}

fn in_place(backup: Option<&str>, report: fix::ReportPlace) -> fix::Target {
    fix::Target {
        destination: fix::Destination::InPlace {
            backup: backup.map(str::to_owned),
        },
        report,
        dry_run: false,
        annotate: false,
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .flat_map(|b| [b >> 4, b & 0xf])
        .filter_map(|n| char::from_digit(u32::from(n), 16))
        .collect()
}

#[test]
fn an_in_place_fix_keeps_each_changed_original_as_its_backup() {
    let (dir, files) = two_files();
    let folder = dir.path().join("report");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&files[0].path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let report = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &in_place(Some(".orig"), fix::ReportPlace::At(folder.clone())),
    )
    .unwrap();
    let fixed = fs::read_to_string(&files[0].path).unwrap();
    assert_eq!(fixed, ORPHAN.replace("INSERT INTO C VALUES (2, 9);\n", ""));
    assert_eq!(
        fs::read_to_string(dir.path().join("t.sql.orig")).unwrap(),
        ORPHAN
    );
    assert_eq!(fs::read_to_string(&files[1].path).unwrap(), CLEAN);
    assert!(
        !dir.path().join("u.sql.orig").exists(),
        "an unchanged file gets no backup"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&files[0].path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
    assert_eq!(
        report.output.findings.len() + report.removed.len(),
        1,
        "the orphan went"
    );

    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let digests = report.files[0].sha256.clone().unwrap();
    assert_eq!(digests.before, sha256(ORPHAN.as_bytes()));
    assert_eq!(digests.after, sha256(fixed.as_bytes()));
    assert_eq!(report.files[1].sha256, None);
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(folder.join("fix-report.json")).unwrap()).unwrap();
    assert_eq!(json["in_place"], true);
    assert_eq!(json["files"][0]["sha256_before"], digests.before.as_str());
    assert_eq!(json["files"][0]["sha256_after"], digests.after.as_str());
    assert_eq!(json["files"][1]["changed"], false);
    assert_eq!(
        json["removed"][0]["reasons"][0]["kind"],
        "fk_missing_parent"
    );
    let rejects = fs::read_to_string(folder.join("rejects.sql")).unwrap();
    assert!(
        rejects.contains("INSERT INTO C VALUES (2, 9);"),
        "{rejects}"
    );
    let text = fs::read_to_string(folder.join("fix-report.txt")).unwrap();
    assert!(
        text.contains("t.sql, original kept beside it as t.sql.orig"),
        "{text}"
    );
}

#[test]
fn an_existing_backup_stops_the_fix_before_it_writes() {
    let (dir, files) = two_files();
    fs::write(dir.path().join("t.sql.orig"), "older").unwrap();
    let folder = dir.path().join("report");
    let error = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &in_place(Some(".orig"), fix::ReportPlace::At(folder.clone())),
    )
    .unwrap_err();
    assert!(error.to_string().contains("already exists"), "{error}");
    assert_eq!(fs::read_to_string(&files[0].path).unwrap(), ORPHAN);
    assert_eq!(
        fs::read_to_string(dir.path().join("t.sql.orig")).unwrap(),
        "older"
    );
    assert!(!folder.exists());
}

#[test]
fn an_in_place_fix_must_keep_the_removed_rows_somewhere() {
    let (_dir, files) = two_files();
    let error = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &in_place(None, fix::ReportPlace::Off),
    )
    .unwrap_err();
    assert!(error.to_string().contains("nowhere"), "{error}");
    assert_eq!(fs::read_to_string(&files[0].path).unwrap(), ORPHAN);

    let (dir, files) = two_files();
    let folder = dir.path().join("report");
    fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &in_place(None, fix::ReportPlace::At(folder.clone())),
    )
    .unwrap();
    assert!(!dir.path().join("t.sql.orig").exists());
    let rejects = fs::read_to_string(folder.join("rejects.sql")).unwrap();
    assert!(
        rejects.contains("INSERT INTO C VALUES (2, 9);"),
        "{rejects}"
    );
}

#[test]
fn a_dry_run_writes_nothing_and_plans_what_the_fix_does() {
    let (dir, files) = two_files();
    let out = dir.path().join("out");
    let plan = |target: fix::Target| {
        fix::fix(&files, &Options::default(), Policy::default(), &target).unwrap()
    };
    let planned = plan(fix::Target {
        dry_run: true,
        ..fix::Target::out(&out)
    });
    let in_place = plan(fix::Target {
        dry_run: true,
        ..in_place(
            Some(".orig"),
            fix::ReportPlace::At(dir.path().join("report")),
        )
    });
    let mut left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(left, ["t.sql", "u.sql"], "nothing written");
    assert_eq!(fs::read_to_string(&files[0].path).unwrap(), ORPHAN);
    assert_eq!(in_place.removed, planned.removed);
    assert!(planned.files.iter().all(|f| f.sha256.is_none()));

    let done = plan(fix::Target::out(&out));
    assert_eq!(done.removed, planned.removed);
    assert_eq!(done.rounds, planned.rounds);
    assert_eq!(
        done.files
            .iter()
            .map(|f| (&f.path, f.changed))
            .collect::<Vec<_>>(),
        planned
            .files
            .iter()
            .map(|f| (&f.path, f.changed))
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_report_goes_where_it_is_told_or_nowhere() {
    let (dir, files) = two_files();
    let fix_to =
        |target: fix::Target| fix::fix(&files, &Options::default(), Policy::default(), &target);
    let full = dir.path().join("full");
    fs::create_dir(&full).unwrap();
    fs::write(full.join("x"), "").unwrap();
    let error = fix_to(fix::Target {
        report: fix::ReportPlace::At(full),
        ..fix::Target::out(dir.path().join("a"))
    })
    .unwrap_err();
    assert!(error.to_string().contains("isn't empty"), "{error}");
    assert!(!dir.path().join("a").exists(), "refused before writing");

    let report = fix_to(fix::Target {
        report: fix::ReportPlace::Off,
        ..fix::Target::out(dir.path().join("b"))
    })
    .unwrap();
    assert_eq!(report.report, None);
    assert!(dir.path().join("b/t.sql").exists());
    assert!(!dir.path().join("b").join(fix::REPORT_FOLDER).exists());

    let elsewhere = dir.path().join("reports/run");
    fix_to(fix::Target {
        report: fix::ReportPlace::At(elsewhere.clone()),
        ..fix::Target::out(dir.path().join("c"))
    })
    .unwrap();
    assert!(elsewhere.join("rejects.sql").exists());
    assert!(elsewhere.join("fix-report.json").exists());
    assert!(!dir.path().join("c").join(fix::REPORT_FOLDER).exists());
}

#[cfg(feature = "gen")]
mod generated {
    use sqlca::check::{self, Options};
    use sqlca::finding::FindingKind;
    use sqlca::synth::{self, Layout, LineEnding, Noise, Placement};

    use super::{
        FIXED, Policy, assert_fixes_as_listed, assert_restores, count, fix, inputs, keep_last,
        null_fk, run_with,
    };

    fn assert_fixed(options: &synth::Options) {
        assert_fixed_with(options, Policy::default(), false);
    }

    #[expect(
        clippy::unwrap_used,
        reason = "a failed generation or a missing file fails the test"
    )]
    fn assert_fixed_with(options: &synth::Options, policy: Policy, annotate: bool) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("export");
        synth::generate(options, &root).unwrap();
        let files = inputs(&root);
        for load_order in [false, true] {
            let check = Options {
                encoding: options.encoding,
                load_order,
                ..Options::default()
            };
            let (_out, report) = run_with(&files, &check, policy, annotate);
            let at = format!("{options:?} {policy:?} annotate={annotate} load_order={load_order}");
            if annotate {
                // The annotated output reads as the plain one does.
                let (_plain_out, plain) = run_with(&files, &check, policy, false);
                assert_eq!(report.output.rows, plain.output.rows, "{at}");
                assert_eq!(report.output.unused, plain.output.unused, "{at}");
                assert_eq!(
                    report.output.errors.len(),
                    plain.output.errors.len(),
                    "{at}"
                );
                let kinds =
                    |r: &check::Report| r.findings.iter().map(|f| f.kind).collect::<Vec<_>>();
                assert_eq!(kinds(&report.output), kinds(&plain.output), "{at}");
                assert!(
                    report.removed.iter().all(|r| !r.replacement.is_empty()),
                    "{at}"
                );
            }
            let empty_parent = count(&report.input, FindingKind::ParentTableEmpty);
            for kind in FIXED {
                assert_eq!(count(&report.output, kind), 0, "{at}: {kind:?}");
            }
            assert_eq!(
                count(&report.output, FindingKind::ParentTableEmpty),
                empty_parent,
                "{at}"
            );
            // Load-order findings are never acted on: each one survives unless its row went for another reason.
            let removed: std::collections::BTreeSet<(String, u64)> = report
                .removed
                .iter()
                .map(|r| (r.file.clone(), r.line))
                .collect();
            let surviving = report
                .input
                .findings
                .iter()
                .filter(|f| f.kind == FindingKind::FkLoadOrder)
                .filter(|f| !removed.contains(&(f.file.clone().unwrap(), f.line.unwrap())))
                .count();
            // A planted bad row is no key's first and no row's parent, so under keep-first the duplicates go with it
            // in one round. Keep-last removes the first row, whose other keys can be parents.
            if policy.on_duplicate == fix::OnDuplicate::KeepFirst && policy.on_fk == fix::OnFk::Drop
            {
                assert_eq!(report.rounds, 1, "{at}: generated exports orphan nothing");
            }
            assert_eq!(
                count(&report.output, FindingKind::FkLoadOrder),
                surviving,
                "{at}"
            );
            // The generated schema has a deferred foreign key, so an added COMMIT could roll the load back.
            assert!(
                report
                    .left
                    .iter()
                    .all(|l| l.finding.kind == FindingKind::Uncommitted),
                "{at}: {:?}",
                report.left
            );
            assert_eq!(
                report.left.len(),
                count(&report.input, FindingKind::Uncommitted),
                "{at}"
            );
            assert!(report.committed.is_empty(), "{at}");
            assert_restores(&files, &report);
        }
    }

    #[test]
    fn generated_exports_are_fixed_in_every_layout_and_placement() {
        for seed in 1..=3 {
            for layout in [Layout::Single, Layout::Separate, Layout::Type] {
                for constraints in [Placement::BeforeData, Placement::AfterData] {
                    assert_fixed(&synth::Options {
                        seed,
                        rows: 40,
                        layout,
                        constraints,
                        ..synth::Options::default()
                    });
                }
            }
        }
    }

    #[test]
    fn generated_exports_are_fixed_with_annotations_under_keep_last_and_null_fks() {
        for seed in 1..=3 {
            for (layout, noise, line_ending) in [
                (Layout::Single, Noise::sqldev(), LineEnding::Lf),
                (Layout::Separate, Noise::sqldev(), LineEnding::Lf),
                (Layout::Single, Noise::all(), LineEnding::Crlf),
            ] {
                let options = synth::Options {
                    seed,
                    rows: 40,
                    layout,
                    noise,
                    line_ending,
                    ..synth::Options::default()
                };
                assert_fixed_with(&options, Policy::default(), true);
                assert_fixed_with(&options, keep_last(), true);
                assert_fixed_with(&options, null_fk(), true);
            }
        }
    }

    #[test]
    fn an_omitted_parent_table_and_shuffled_files_are_left_as_they_are() {
        assert_fixed(&synth::Options {
            seed: 5,
            rows: 40,
            omit_data: vec!["APP.PROJECTS".to_owned()],
            ..synth::Options::default()
        });
        assert_fixed(&synth::Options {
            seed: 3,
            rows: 40,
            shuffle_order: true,
            ..synth::Options::default()
        });
    }

    #[test]
    fn cascades_go_with_their_root_in_every_layout_and_placement() {
        for layout in [Layout::Single, Layout::Separate, Layout::Type] {
            for constraints in [Placement::BeforeData, Placement::AfterData] {
                for shuffle_order in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let root = dir.path().join("export");
                    let options = synth::Options {
                        rows: 40,
                        layout,
                        constraints,
                        shuffle_order,
                        // A plant at every row: the extras' keys must stay clear of the cascade rows'.
                        violations_ppm: 1_000_000,
                        cascades: true,
                        manager_chain: 3,
                        ..synth::Options::default()
                    };
                    let manifest = synth::generate(&options, &root).unwrap();
                    assert!(manifest.cascades.len() > 40, "{options:?}");
                    assert_fixes_as_listed(&root);
                }
            }
        }
    }

    /// Keep-last removes the first project of the name the key between rows judges, the only row its finding names;
    /// the second, which the key never checked, is then the third's first and goes in a later round. (Not the next:
    /// the other keys' first rows go too, and duplicates wait while their children go.)
    #[test]
    fn keep_last_takes_the_project_the_key_between_rows_hid() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("export");
        let options = synth::Options {
            rows: 20,
            cascades: true,
            ..synth::Options::default()
        };
        synth::generate(&options, &root).unwrap();
        let (_dir, report) = run_with(&inputs(&root), &Options::default(), keep_last(), false);
        let mut twins: Vec<(u64, usize)> = report
            .removed
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.text).contains("'Twin project'"))
            .map(|r| (r.line, r.round))
            .collect();
        twins.sort_unstable();
        let [(first, round), (second, next)] = twins[..] else {
            panic!("{twins:?}");
        };
        assert!(first < second, "{twins:?}");
        assert!(next > round, "{twins:?}");
        assert_eq!(count(&report.output, FindingKind::DuplicateKey), 0);
    }

    /// A manager chain loses a row per round: in an odd tree the department goes first, then its employee, then each
    /// employee of the chain. Without a chain the deepest row is the timesheet, four rounds down. The duplicates go in
    /// the last of these rounds, once nothing going can orphan a row or is a duplicated key's first.
    #[test]
    fn a_manager_chain_goes_one_round_per_employee() {
        for chain in [0, 1, 5, synth::MAX_MANAGER_CHAIN] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("export");
            let options = synth::Options {
                rows: 20,
                cascades: true,
                manager_chain: chain,
                ..synth::Options::default()
            };
            synth::generate(&options, &root).unwrap();
            let (_dir, report) = run_with(
                &inputs(&root),
                &Options::default(),
                Policy::default(),
                false,
            );
            let deepest = usize::try_from(chain).unwrap() + 2;
            assert_eq!(report.rounds, deepest.max(4), "chain {chain}");
            // Nothing past the round limit: only the COMMIT the departments' deferred key keeps out is left.
            assert!(
                report
                    .left
                    .iter()
                    .all(|l| l.finding.kind == FindingKind::Uncommitted),
                "chain {chain}: {:?}",
                report.left
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let options = synth::Options {
            cascades: true,
            manager_chain: synth::MAX_MANAGER_CHAIN + 1,
            ..synth::Options::default()
        };
        assert!(matches!(
            synth::generate(&options, &dir.path().join("export")),
            Err(synth::Error::ChainTooLong(21))
        ));
    }
}

/// The first output as written, its COMMIT included.
#[expect(clippy::unwrap_used, reason = "an output that fails fails the test")]
fn written(report: &FixReport) -> String {
    fs::read_to_string(&report.files.first().unwrap().path).unwrap()
}

/// `sql` written as it is, with no COMMIT added, as one data file fixed into a fresh folder and restored by the
/// report.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture or folder that fails fails the test"
)]
fn fix_uncommitted(
    sql: &str,
    target: impl Fn(&Path) -> fix::Target,
) -> (tempfile::TempDir, FixReport) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sql");
    fs::write(&path, sql).unwrap();
    let files = [InputFile {
        path,
        role: Role::Data,
        bytes: u64::try_from(sql.len()).unwrap(),
        order: OrderSource::Given,
        includes: Vec::new(),
    }];
    let report = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &target(&dir.path().join("out")),
    )
    .unwrap();
    if !report.target.dry_run {
        assert_restores(&files, &report);
    }
    (dir, report)
}

#[test]
fn a_commit_goes_at_the_end_of_a_file_whose_last_rows_have_none() {
    let note = "-- sqlca fix added this COMMIT: no COMMIT followed the rows before it";
    let cases = [
        (
            "INSERT INTO T VALUES (1);\n",
            false,
            "INSERT INTO T VALUES (1);\nCOMMIT;\n".to_owned(),
        ),
        (
            "INSERT INTO T VALUES (1);",
            false,
            "INSERT INTO T VALUES (1);\nCOMMIT;\n".to_owned(),
        ),
        (
            "INSERT INTO T VALUES (1);\r\n",
            false,
            "INSERT INTO T VALUES (1);\r\nCOMMIT;\r\n".to_owned(),
        ),
        (
            "INSERT INTO T VALUES (1);\r\nINSERT INTO T VALUES (2);",
            false,
            "INSERT INTO T VALUES (1);\r\nINSERT INTO T VALUES (2);\r\nCOMMIT;\r\n".to_owned(),
        ),
        (
            "INSERT INTO T VALUES (1);\n",
            true,
            format!("INSERT INTO T VALUES (1);\n{note}\nCOMMIT;\n"),
        ),
    ];
    for (rows, annotate, fixed) in cases {
        let ddl = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\n";
        let (_dir, report) = fix_uncommitted(&format!("{ddl}{rows}"), |out| fix::Target {
            annotate,
            ..fix::Target::out(out)
        });
        assert_eq!(written(&report), format!("{ddl}{fixed}"), "{rows:?}");
        assert!(report.removed.is_empty(), "{rows:?}");
        assert_eq!(report.committed.len(), 1, "{rows:?}");
        assert!(
            report.output.findings.is_empty(),
            "{rows:?}: {:?}",
            report.output.findings
        );
        assert_eq!(fix::summary(&report), "Fixed      COMMIT added to 1 file");
    }
}

/// A deferred foreign key is checked at commit, and a transaction can run across files: a COMMIT added after the
/// child, or after unrelated rows before its parent comes, would roll the load back. So none is added.
#[test]
fn no_commit_is_added_where_a_constraint_is_checked_at_commit() {
    let (_dir, files) = write_scripts(&[
        (
            "a.sql",
            "CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
             CREATE TABLE C (ID NUMBER, P_ID NUMBER REFERENCES P DEFERRABLE INITIALLY DEFERRED);\n\
             CREATE TABLE A (ID NUMBER);\n\
             INSERT INTO C VALUES (1, 2);",
        ),
        ("b.sql", "INSERT INTO A VALUES (1);"),
        ("c.sql", "INSERT INTO P VALUES (2);\n"),
    ]);
    let (_out, report) = run(&files, &Options::default());
    assert!(report.committed.is_empty(), "{:?}", report.committed);
    let left: Vec<(FindingKind, Option<&str>)> = report
        .left
        .iter()
        .map(|l| (l.finding.kind, l.finding.file.as_deref()))
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        left.iter()
            .all(|(kind, _)| *kind == FindingKind::Uncommitted)
    );
    assert!(report.files.iter().all(|f| !f.changed));
    assert_eq!(
        fix::summary(&report),
        "Fixed      0 rows in 0 rounds; 2 left for review"
    );
}

#[test]
fn a_commit_is_added_beside_the_rows_a_fix_removes_and_kept_out_of_the_rejects() {
    let sql = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\n";
    let (dir, report) = fix_uncommitted(sql, |out| fix::Target::out(out));
    assert_eq!(
        written(&report),
        "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nCOMMIT;\n"
    );
    assert_eq!(report.removed.len(), 1);
    assert!(
        fix::summary(&report)
            .starts_with("Fixed      1 rows in 1 round (duplicate key 1); COMMIT added to 1 file")
    );
    let rejects = fs::read_to_string(dir.path().join("out/sqlca-report/rejects.sql")).unwrap();
    assert!(!rejects.contains("COMMIT"), "{rejects}");
    let json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join("out/sqlca-report/fix-report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["removed"].as_array().unwrap().len(), 1);
    assert_eq!(json["committed"][0]["finding"]["kind"], "uncommitted");
    let text = fs::read_to_string(dir.path().join("out/sqlca-report/fix-report.txt")).unwrap();
    assert!(text.contains("Committed:\n  "), "{text}");
}

#[test]
fn a_dry_run_tells_the_commit_it_would_add_as_settled() {
    let sql = "CREATE TABLE T (ID NUMBER);\nINSERT INTO T VALUES (1);\n";
    let (dir, report) = fix_uncommitted(sql, |out| fix::Target {
        dry_run: true,
        ..fix::Target::out(out)
    });
    assert_eq!(report.committed.len(), 1);
    assert!(
        report.output.findings.is_empty(),
        "{:?}",
        report.output.findings
    );
    assert!(fix::summary(&report).starts_with("Would fix "));
    assert_eq!(fs::read_to_string(dir.path().join("t.sql")).unwrap(), sql);
    assert!(!dir.path().join("out").exists());
}

/// The stages a fix runs, as its observer hears them.
#[derive(Default)]
struct Stages(Vec<String>);

impl check::Observer for Stages {
    fn stage(&mut self, stage: &str) {
        self.0.push(stage.to_owned());
    }
}

/// Fixes one script into a new folder, and returns the stages it ran with the report.
#[expect(clippy::unwrap_used, reason = "a fix that fails fails the test")]
fn fix_stages(sql: &str) -> (Vec<String>, FixReport) {
    let (dir, files) = write_scripts(&[("t.sql", sql)]);
    let mut stages = Stages::default();
    let report = fix::fix_with(
        &files,
        &Options::default(),
        Policy::default(),
        &fix::Target::out(dir.path().join("out")),
        &mut stages,
    )
    .unwrap();
    (stages.0, report)
}

const PARENT_WITH_NOT_NULL: &str = "CREATE TABLE P (ID NUMBER PRIMARY KEY, N NUMBER NOT NULL);\n\
    CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n";

#[test]
fn a_removed_row_no_child_points_at_needs_no_second_round() {
    let (stages, report) = fix_stages(&format!(
        "{PARENT_WITH_NOT_NULL}INSERT INTO P VALUES (1, 1);\nINSERT INTO P VALUES (2, NULL);\nINSERT INTO C VALUES (1, 1);\n"
    ));
    assert_eq!(stages, ["check 1", "write", "verify"]);
    assert_eq!(report.removed.len(), 1, "{:#?}", report.removed);
}

#[test]
fn a_removed_row_a_child_points_at_takes_the_child_in_the_next_round() {
    let (stages, report) = fix_stages(&format!(
        "{PARENT_WITH_NOT_NULL}INSERT INTO P VALUES (1, 1);\nINSERT INTO P VALUES (2, NULL);\nINSERT INTO C VALUES (1, 2);\n"
    ));
    assert!(stages.contains(&"check 2".to_owned()), "{stages:?}");
    assert_eq!(report.removed.len(), 2, "{:#?}", report.removed);
    assert!(
        report.output.findings.is_empty(),
        "{:#?}",
        report.output.findings
    );
}

#[test]
fn a_child_written_before_its_parent_still_takes_the_next_round() {
    // The child's lookup waits for the end of the input, where it finds the parent that then goes.
    let (stages, report) = fix_stages(&format!(
        "{PARENT_WITH_NOT_NULL}INSERT INTO C VALUES (1, 2);\nINSERT INTO P VALUES (1, 1);\nINSERT INTO P VALUES (2, NULL);\n"
    ));
    assert!(stages.contains(&"check 2".to_owned()), "{stages:?}");
    assert_eq!(report.removed.len(), 2, "{:#?}", report.removed);
    assert!(
        report.output.findings.is_empty(),
        "{:#?}",
        report.output.findings
    );
}

#[test]
fn removing_a_parent_whose_key_the_check_cant_know_takes_the_next_round() {
    // While P holds a key the check can't know, C's missing parent is unverifiable; without that row it is missing.
    let (stages, report) = fix_stages(&format!(
        "{PARENT_WITH_NOT_NULL}INSERT INTO P VALUES (1, 1);\nINSERT INTO P VALUES (S.NEXTVAL, NULL);\nINSERT INTO C VALUES (1, 5);\n"
    ));
    assert!(stages.contains(&"check 2".to_owned()), "{stages:?}");
    assert_eq!(report.removed.len(), 2, "{:#?}", report.removed);
    assert!(
        report.output.findings.is_empty(),
        "{:#?}",
        report.output.findings
    );
}
