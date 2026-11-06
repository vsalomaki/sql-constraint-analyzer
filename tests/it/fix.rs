//! `fix` against the generator's exports and hand-written cases: the output has none of the violations it fixes, its
//! bytes plus the removed ones restore the input exactly, and what it must not touch survives.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sqlca::check::{self, Options};
use sqlca::finding::FindingKind;
use sqlca::fix::{self, FixReport, Policy};
use sqlca::input::{InputFile, OrderSource, Role};
use sqlca::source::Encoding;

#[derive(Deserialize)]
struct Expected {
    options: ExpectedOptions,
}

#[derive(Deserialize)]
struct ExpectedOptions {
    encoding: Encoding,
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture or folder that fails fails the test"
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
            bytes: fs::metadata(root.join(path)).unwrap().len(),
            order: OrderSource::Given,
        })
        .collect()
}

const FIXED: [FindingKind; 3] = [
    FindingKind::FkMissingParent,
    FindingKind::DuplicateKey,
    FindingKind::NotNull,
];

fn count(report: &check::Report, kind: FindingKind) -> usize {
    report.findings.iter().filter(|f| f.kind == kind).count()
}

/// Each input's bytes equal its output's with the removed bytes put back where they were.
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "byte bookkeeping over files the test wrote; a mismatch fails the test"
)]
fn assert_restores(files: &[InputFile], report: &FixReport) {
    for (file, written) in files.iter().zip(&report.written) {
        let original = fs::read(&file.path).unwrap();
        let output = fs::read(written).unwrap();
        let name = check::display(&file.path);
        let mut removed: Vec<_> = report.removed.iter().filter(|r| r.file == name).collect();
        removed.sort_by_key(|r| r.span.start);
        let (mut rebuilt, mut at_original, mut at_output) = (Vec::new(), 0_usize, 0_usize);
        for removal in removed {
            let start = usize::try_from(removal.span.start).unwrap();
            let kept = start - at_original;
            rebuilt.extend_from_slice(&output[at_output..at_output + kept]);
            rebuilt.extend_from_slice(&removal.text);
            at_output += kept;
            at_original = usize::try_from(removal.span.end).unwrap();
        }
        rebuilt.extend_from_slice(&output[at_output..]);
        assert!(rebuilt == original, "{name} isn't restored by its rejects");
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture or folder that fails fails the test"
)]
fn run(files: &[InputFile], options: &Options) -> (tempfile::TempDir, FixReport) {
    let out = tempfile::tempdir().unwrap();
    let report = fix::fix(files, options, Policy::default(), &out.path().join("fixed")).unwrap();
    (out, report)
}

#[test]
fn committed_exports_are_fixed_and_restored_by_their_rejects() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gen/oracle");
    for name in [
        "separate-sqldev",
        "single-all-crlf-1252",
        "type-after-data-shuffled",
    ] {
        let root = root.join(name);
        let expected: Expected =
            serde_json::from_str(&fs::read_to_string(root.join("expected.json")).unwrap()).unwrap();
        for load_order in [false, true] {
            let options = Options {
                encoding: expected.options.encoding,
                load_order,
                ..Options::default()
            };
            let files = inputs(&root);
            let (_dir, report) = run(&files, &options);
            let at = format!("{name} load_order={load_order}");
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
            // Load-order findings are valid rows in the end state: never removed.
            assert_eq!(
                count(&report.output, FindingKind::FkLoadOrder),
                count(&report.input, FindingKind::FkLoadOrder),
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

/// A script as one data file, fixed into a fresh folder.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture or folder that fails fails the test"
)]
fn fix_sql(sql: &str) -> (tempfile::TempDir, Vec<InputFile>, FixReport) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in").join("t.sql");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, sql).unwrap();
    let files = vec![InputFile {
        path,
        role: Role::Data,
        bytes: u64::try_from(sql.len()).unwrap(),
        order: OrderSource::Given,
    }];
    let report = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &dir.path().join("out"),
    )
    .unwrap();
    assert_restores(&files, &report);
    (dir, files, report)
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture or folder that fails fails the test"
)]
fn output(report: &FixReport) -> String {
    fs::read_to_string(report.written.first().unwrap()).unwrap()
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
    assert_eq!(fs::read_to_string(&files[0].path).unwrap(), sql);
}

/// The scripts as data files in one folder, by name, fixed into `out` under the same temporary folder.
#[expect(
    clippy::unwrap_used,
    reason = "a folder the test can't write fails the test"
)]
fn fix_files(
    scripts: &[(&str, &str)],
) -> (
    tempfile::TempDir,
    Vec<InputFile>,
    Result<FixReport, fix::FixError>,
) {
    let dir = tempfile::tempdir().unwrap();
    let files = scripts
        .iter()
        .map(|(name, sql)| {
            let path = dir.path().join("in").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, sql).unwrap();
            InputFile {
                path,
                role: Role::Data,
                bytes: u64::try_from(sql.len()).unwrap(),
                order: OrderSource::Given,
            }
        })
        .collect::<Vec<_>>();
    let report = fix::fix(
        &files,
        &Options::default(),
        Policy::default(),
        &dir.path().join("out"),
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
    assert_eq!(originals, [bad, other, also_bad.as_str()]);
    assert_eq!(fs::read_to_string(&report.written[1]).unwrap(), other);
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
            .all(|f| fs::read_to_string(&f.path).unwrap() == "COMMIT;\n")
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
    }];
    let error = fix::fix(&files, &Options::default(), Policy::default(), dir.path()).unwrap_err();
    assert!(error.to_string().contains("isn't empty"), "{error}");
}

#[test]
fn outputs_keep_the_inputs_relative_paths() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gen/oracle/separate-sqldev");
    let files = inputs(&root);
    let (dir, report) = run(&files, &Options::default());
    let relative: BTreeMap<PathBuf, PathBuf> = report
        .written
        .iter()
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

#[cfg(feature = "gen")]
mod generated {
    use sqlca::check::Options;
    use sqlca::finding::FindingKind;
    use sqlca::synth::{self, Layout, Placement};

    use super::{FIXED, assert_restores, count, inputs, run};

    #[expect(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        reason = "a failed generation or a missing file fails the test"
    )]
    fn assert_fixed(options: &synth::Options) {
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
            let (_out, report) = run(&files, &check);
            let at = format!("{options:?} load_order={load_order}");
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
                .map(|r| {
                    let index = files
                        .iter()
                        .position(|f| sqlca::check::display(&f.path) == r.file)
                        .unwrap();
                    (sqlca::check::display(&report.written[index]), r.line)
                })
                .collect();
            let surviving = report
                .input
                .findings
                .iter()
                .filter(|f| f.kind == FindingKind::FkLoadOrder)
                .filter(|f| !removed.contains(&(f.file.clone().unwrap(), f.line.unwrap())))
                .count();
            assert_eq!(report.rounds, 1, "{at}: generated exports orphan nothing");
            assert_eq!(
                count(&report.output, FindingKind::FkLoadOrder),
                surviving,
                "{at}"
            );
            assert!(report.left.is_empty(), "{at}: {:?}", report.left);
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
}
