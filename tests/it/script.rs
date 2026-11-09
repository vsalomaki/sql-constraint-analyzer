//! `--script`: each include runs where its line is, so the script's own statements run between what it includes.

use std::fs;
use std::io;
use std::path::Path;

use sqlca::check::{self, Options, Report};
use sqlca::finding::{FindingKind, Location};
use sqlca::fix::{self, FixReport, OnDuplicate, OnFk, Policy, ReportPlace};
use sqlca::input::{self, InputFile, InputSpec};

const TABLES: &str = "CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
                      CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER);\n";
const FK: &str = "ALTER TABLE C ADD CONSTRAINT C_FK FOREIGN KEY (P_ID) REFERENCES P (ID);\n";

#[expect(
    clippy::unwrap_used,
    reason = "a file that can't be written fails the test"
)]
fn export(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    dir
}

/// The files `--script load.sql` reads, with `spec` changed as given.
#[expect(
    clippy::unwrap_used,
    reason = "a script that can't be resolved fails the test"
)]
fn script(dir: &Path, change: impl FnOnce(&mut InputSpec)) -> Vec<InputFile> {
    let mut spec = InputSpec {
        script: Some(dir.join("load.sql")),
        ..InputSpec::default()
    };
    change(&mut spec);
    input::resolve(&spec, &mut io::empty()).unwrap().files
}

/// Each finding's kind, file below `dir` and line.
fn places(report: &Report, dir: &Path) -> Vec<(FindingKind, String, Option<u64>)> {
    let root = check::display(dir);
    report
        .findings
        .iter()
        .map(|f| {
            let file = f.file.as_deref().unwrap_or_default();
            let file = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .trim_start_matches('/');
            (f.kind, file.to_owned(), f.line)
        })
        .collect()
}

/// Each removed or changed row, by file below `dir` and line.
fn removed(report: &FixReport, dir: &Path) -> Vec<(String, u64)> {
    let root = check::display(dir);
    report
        .removed
        .iter()
        .flat_map(|r| {
            let file = r
                .file
                .strip_prefix(&root)
                .unwrap_or(&r.file)
                .trim_start_matches('/');
            r.rows.iter().map(move |line| (file.to_owned(), *line))
        })
        .collect()
}

#[expect(clippy::unwrap_used, reason = "a fix that fails fails the test")]
fn fix_into(files: &[InputFile], out: &Path, policy: Policy) -> FixReport {
    fix::fix(files, &Options::default(), policy, &fix::Target::out(out)).unwrap()
}

/// The issue's example: the key comes after the data with NOVALIDATE, so Oracle keeps the row, and so must `fix`.
#[test]
fn a_script_runs_each_include_at_its_line() {
    let load = "@@tables.sql\n\
                Insert into P (ID) values (1);\n\
                COMMIT;\n\
                @@data/C.sql\n\
                Insert into P (ID) values (2);\n\
                COMMIT;\n\
                ALTER TABLE C ADD CONSTRAINT C_FK FOREIGN KEY (P_ID) REFERENCES P (ID) ENABLE NOVALIDATE;\n";
    let dir = export(&[
        ("load.sql", load.as_bytes()),
        ("tables.sql", TABLES.as_bytes()),
        ("data/C.sql", b"Insert into C (ID,P_ID) values (11,3);\n"),
    ]);
    let files = script(dir.path(), |_| {});
    let report = check::run(&files, &Options::default()).unwrap();
    assert_eq!(places(&report, dir.path()), []);
    assert_eq!(report.rows, 3);

    let fixed = fix_into(&files, &dir.path().join("out"), Policy::default());
    assert!(fixed.removed.is_empty(), "{:?}", fixed.removed);
    assert!(fixed.committed.is_empty());
    assert_eq!(places(&fixed.output, dir.path()), []);
}

#[test]
fn a_commit_in_the_script_ends_what_its_includes_left_open() {
    let data: &[u8] = b"Insert into C (ID,P_ID) values (11,3);\n";
    let committed = export(&[
        ("load.sql", b"@@tables.sql\n@@data/C.sql\nCOMMIT;\n"),
        ("tables.sql", TABLES.as_bytes()),
        ("data/C.sql", data),
    ]);
    let files = script(committed.path(), |_| {});
    let report = check::run(&files, &Options::default()).unwrap();
    assert_eq!(places(&report, committed.path()), []);

    // Without it, the rows of the include and the script after it are open at the end: the finding names the first,
    // and the commit goes at the end of the script.
    let open = export(&[
        (
            "load.sql",
            b"@@tables.sql\n@@data/C.sql\nInsert into P (ID) values (3);\n",
        ),
        ("tables.sql", TABLES.as_bytes()),
        ("data/C.sql", data),
    ]);
    let files = script(open.path(), |_| {});
    let report = check::run(&files, &Options::default()).unwrap();
    assert_eq!(
        places(&report, open.path()),
        [(FindingKind::Uncommitted, "data/C.sql".to_owned(), Some(1))]
    );
    assert_eq!(report.findings[0].count, Some(2));
    let dry_run = fix::Target {
        dry_run: true,
        ..fix::Target::out(open.path().join("dry"))
    };
    let dry = fix::fix(&files, &Options::default(), Policy::default(), &dry_run).unwrap();
    assert_eq!(dry.committed.len(), 1);
    assert_eq!(
        places(&dry.output, open.path()),
        [],
        "the commit it would add settles it"
    );
    let out = open.path().join("out");
    let fixed = fix_into(&files, &out, Policy::default());
    assert_eq!(fixed.committed.len(), 1);
    assert!(
        fixed.committed[0].file.ends_with("/load.sql"),
        "{}",
        fixed.committed[0].file
    );
    assert_eq!(fs::read(out.join("data/C.sql")).unwrap(), data);
    assert!(
        fs::read_to_string(out.join("load.sql"))
            .unwrap()
            .ends_with("(3);\nCOMMIT;\n")
    );
    assert_eq!(places(&fixed.output, open.path()), []);
}

#[test]
fn rows_in_the_script_after_an_include_load_after_it() {
    let tables = format!("{TABLES}{FK}");
    let dir = export(&[
        (
            "load.sql",
            b"@@tables.sql\n@@data/C.sql\nInsert into P (ID) values (3);\nCOMMIT;\n",
        ),
        ("tables.sql", tables.as_bytes()),
        ("data/C.sql", b"Insert into C (ID,P_ID) values (11,3);\n"),
    ]);
    let files = script(dir.path(), |_| {});
    let end_state = check::run(&files, &Options::default()).unwrap();
    assert_eq!(places(&end_state, dir.path()), []);
    let load_order = Options {
        load_order: true,
        ..Options::default()
    };
    let report = check::run(&files, &load_order).unwrap();
    assert_eq!(
        places(&report, dir.path()),
        [(FindingKind::FkLoadOrder, "data/C.sql".to_owned(), Some(1))]
    );
}

/// The script's row runs after the include's, though the script is listed first.
#[test]
fn keep_first_and_keep_last_follow_the_run_order() {
    let dir = export(&[
        (
            "load.sql",
            b"@@tables.sql\n@@data/T.sql\nInsert into T (ID,V) values (1,'script');\nCOMMIT;\n",
        ),
        (
            "tables.sql",
            b"CREATE TABLE T (ID NUMBER PRIMARY KEY, V VARCHAR2(10));\n",
        ),
        (
            "data/T.sql",
            b"Insert into T (ID,V) values (1,'include');\nCOMMIT;\n",
        ),
    ]);
    let files = script(dir.path(), |_| {});
    let first = fix_into(&files, &dir.path().join("first"), Policy::default());
    assert_eq!(removed(&first, dir.path()), [("load.sql".to_owned(), 3)]);
    let keep_last = Policy {
        on_duplicate: OnDuplicate::KeepLast,
        ..Policy::default()
    };
    let last = fix_into(&files, &dir.path().join("last"), keep_last);
    assert_eq!(removed(&last, dir.path()), [("data/T.sql".to_owned(), 1)]);
    assert_eq!(places(&last.output, dir.path()), []);
}

/// The key's first row in run order is the include's, and goes for its missing parent: the script's row is kept.
#[test]
fn a_key_whose_first_row_goes_names_the_row_kept_in_run_order() {
    let tables = "CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
                  CREATE TABLE T (ID NUMBER PRIMARY KEY, P_ID NUMBER);\n\
                  ALTER TABLE T ADD CONSTRAINT T_FK FOREIGN KEY (P_ID) REFERENCES P (ID);\n";
    let dir = export(&[
        (
            "load.sql",
            b"@@tables.sql\nInsert into P (ID) values (1);\n@@data/T.sql\n\
              Insert into T (ID,P_ID) values (1,1);\nCOMMIT;\n",
        ),
        ("tables.sql", tables.as_bytes()),
        ("data/T.sql", b"Insert into T (ID,P_ID) values (1,9);\n"),
    ]);
    let fixed = fix_into(
        &script(dir.path(), |_| {}),
        &dir.path().join("out"),
        Policy::default(),
    );
    assert_eq!(removed(&fixed, dir.path()), [("data/T.sql".to_owned(), 1)]);
    let kept: Vec<_> = fixed
        .kept
        .iter()
        .map(|k| (k.policy.line, k.kept.as_ref().map(|l| l.line)))
        .collect();
    assert_eq!(kept, [(1, Some(4))], "{:?}", fixed.kept);
    assert!(fixed.kept[0].policy.file.ends_with("/data/T.sql"));
}

/// A NOT NULL enabled after the include doesn't judge its rows, so `--on-fk null` may set their key to NULL.
#[test]
fn on_fk_null_asks_the_constraints_that_judge_the_row_where_it_runs() {
    let tables = format!("{TABLES}{FK}");
    let dir = export(&[
        (
            "load.sql",
            b"@@tables.sql\nInsert into P (ID) values (1);\n@@data/C.sql\n\
              ALTER TABLE C MODIFY (P_ID NOT NULL ENABLE NOVALIDATE);\n",
        ),
        ("tables.sql", tables.as_bytes()),
        (
            "data/C.sql",
            b"Insert into C (ID,P_ID) values (11,3);\nCOMMIT;\n",
        ),
    ]);
    let files = script(dir.path(), |_| {});
    let null_fk = Policy {
        on_fk: OnFk::Null,
        ..Policy::default()
    };
    let fixed = fix_into(&files, &dir.path().join("out"), null_fk);
    assert_eq!(fixed.removed.len(), 1);
    let nulled: Vec<&str> = fixed.removed[0]
        .nulled
        .iter()
        .map(|(c, _)| c.as_str())
        .collect();
    assert_eq!(nulled, ["P_ID"], "{:?}", fixed.removed[0].reasons);
    assert_eq!(places(&fixed.output, dir.path()), []);

    // A NOT NULL an include enables judges the script's rows after it, so the row goes instead.
    let after = export(&[
        (
            "load.sql",
            b"@@tables.sql\nInsert into P (ID) values (1);\n@@not_null.sql\n\
              Insert into C (ID,P_ID) values (11,3);\nCOMMIT;\n",
        ),
        ("tables.sql", tables.as_bytes()),
        (
            "not_null.sql",
            b"ALTER TABLE C MODIFY (P_ID NOT NULL ENABLE NOVALIDATE);\n",
        ),
    ]);
    let files = script(after.path(), |_| {});
    let fixed = fix_into(&files, &after.path().join("out"), null_fk);
    assert_eq!(fixed.removed.len(), 1);
    let reasons: Vec<&str> = fixed.removed[0]
        .reasons
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(
        reasons,
        ["missing parent for C_FK: P_ID='3'; the row went, since P_ID is NOT NULL"]
    );
    assert_eq!(places(&fixed.output, after.path()), []);
}

/// A row the fix cuts before an include moves the include up; the verify runs it from where it is now.
#[test]
fn the_verify_finds_an_include_that_a_cut_before_it_moved() {
    let tables = format!("{TABLES}{FK}");
    let load = b"\xEF\xBB\xBF@@tables.sql\n\
                 Insert into P (ID) values (1);\n\
                 Insert into P (ID) values (1);\n\
                 @@data/C.sql\n\
                 COMMIT;\n";
    let files = [
        ("load.sql", &load[..]),
        ("tables.sql", tables.as_bytes()),
        ("data/C.sql", b"Insert into C (ID,P_ID) values (11,1);\n"),
    ];
    let out = export(&files);
    let fixed = fix_into(
        &script(out.path(), |_| {}),
        &out.path().join("out"),
        Policy::default(),
    );
    assert_eq!(removed(&fixed, out.path()), [("load.sql".to_owned(), 3)]);
    assert_eq!(places(&fixed.output, out.path()), []);
    assert!(fixed.output.errors.is_empty(), "{:?}", fixed.output.errors);

    let in_place = export(&files);
    let target = fix::Target {
        destination: fix::Destination::InPlace {
            backup: Some(".orig".to_owned()),
        },
        report: ReportPlace::Off,
        dry_run: false,
        annotate: true,
    };
    let fixed = fix::fix(
        &script(in_place.path(), |_| {}),
        &Options::default(),
        Policy::default(),
        &target,
    )
    .unwrap();
    assert_eq!(
        removed(&fixed, in_place.path()),
        [("load.sql".to_owned(), 3)]
    );
    assert_eq!(places(&fixed.output, in_place.path()), []);
    assert!(fixed.output.errors.is_empty(), "{:?}", fixed.output.errors);
}

/// A file given with `--reference` or `--schema` and included by the script is read once, where it is given.
#[test]
fn a_file_given_and_included_is_read_where_it_is_given() {
    let tables = format!("{TABLES}{FK}Insert into P (ID) values (1);\n");
    let dir = export(&[
        ("load.sql", b"@@tables.sql\n@@data/C.sql\nCOMMIT;\n"),
        ("tables.sql", tables.as_bytes()),
        ("data/C.sql", b"Insert into C (ID,P_ID) values (11,1);\n"),
    ]);
    let given = check::display(&dir.path().join("tables.sql"));
    let files = script(dir.path(), |spec| spec.reference = vec![given.clone()]);
    let report = check::run(&files, &Options::default()).unwrap();
    assert_eq!(places(&report, dir.path()), []);
    assert_eq!(report.rows, 2, "the reference file's row is read once");

    let files = script(dir.path(), |spec| spec.schema = vec![given.clone()]);
    let report = check::run(&files, &Options::default()).unwrap();
    assert_eq!(report.schema.tables, 2);
    assert_eq!(report.rows, 1, "a schema file's rows aren't read");
    assert_eq!(
        places(&report, dir.path()),
        [(FindingKind::ParentTableEmpty, String::new(), None)]
    );
}

/// `script` in `root` with each `@@path` line replaced by that file's lines, and for each line of the result, the
/// file and line it came from.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture file that can't be read fails the test"
)]
fn flatten(root: &Path, script: &str, text: &mut Vec<u8>, lines: &mut Vec<(String, u64)>) {
    let bytes = fs::read(root.join(script)).unwrap();
    for (line, content) in (1..).zip(bytes.split_inclusive(|&b| b == b'\n')) {
        let command = std::str::from_utf8(content).unwrap().trim();
        if let Some(name) = command.strip_prefix("@@") {
            flatten(root, name, text, lines);
            continue;
        }
        text.extend_from_slice(content);
        if !content.ends_with(b"\n") {
            text.push(b'\n');
        }
        lines.push((script.to_owned(), line));
    }
}

/// The findings with each place below `root`, or for the flattened file, where its line came from.
fn placed(
    report: &Report,
    place: &dyn Fn(&str, u64) -> (String, u64),
) -> Vec<sqlca::finding::Finding> {
    let locate = |l: &Location| {
        let (file, line) = place(&l.file, l.line);
        Location { file, line }
    };
    let mut findings: Vec<_> = report
        .findings
        .iter()
        .map(|f| {
            let at = f
                .file
                .as_deref()
                .zip(f.line)
                .map(|(file, line)| place(file, line));
            sqlca::finding::Finding {
                file: at.as_ref().map(|(file, _)| file.clone()),
                line: at.map(|(_, line)| line),
                first: f.first.as_ref().map(locate),
                unchecked: f.unchecked.iter().map(locate).collect(),
                ..f.clone()
            }
        })
        .collect();
    findings.sort();
    findings
}

/// The generator's export whose `load.sql` holds statements of its own: what `--script` finds is what the file it
/// runs, flattened, finds.
#[test]
fn the_generated_script_finds_what_its_flattened_file_finds() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gen/oracle/separate-script-statements");
    let (mut text, mut lines) = (Vec::new(), Vec::new());
    flatten(&root, "load.sql", &mut text, &mut lines);
    let dir = tempfile::tempdir().unwrap();
    let flat = dir.path().join("flat.sql");
    fs::write(&flat, &text).unwrap();
    let flat_files = input::resolve(
        &InputSpec {
            paths: vec![check::display(&flat)],
            ..InputSpec::default()
        },
        &mut io::empty(),
    )
    .unwrap()
    .files;
    let script_files = script(&root, |_| {});
    let prefix = format!("{}/", check::display(&root));
    let from_script =
        |file: &str, line: u64| (file.strip_prefix(&prefix).unwrap_or(file).to_owned(), line);
    let from_flat = |_: &str, line: u64| {
        let index = usize::try_from(line - 1).unwrap();
        lines[index].clone()
    };
    for load_order in [false, true] {
        let options = Options {
            load_order,
            ..Options::default()
        };
        let by_script = check::run(&script_files, &options).unwrap();
        let by_flat = check::run(&flat_files, &options).unwrap();
        assert!(by_script.errors.is_empty(), "{:?}", by_script.errors);
        assert_eq!(by_script.rows, by_flat.rows);
        assert_eq!(
            placed(&by_script, &from_script),
            placed(&by_flat, &from_flat),
            "load order: {load_order}"
        );
        if load_order {
            assert!(
                by_script
                    .findings
                    .iter()
                    .any(|f| f.kind == FindingKind::FkLoadOrder)
            );
        }
    }
}

/// Through its own `load.sql`, the export has the manifest's row findings and no missing COMMIT, and keep-first
/// removes the rows the manifest lists: what `make oracle-check` loads and fixes it by.
#[test]
fn the_generated_script_has_the_manifests_rows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gen/oracle/separate-script-statements");
    let manifest = crate::fixtures::manifest(&root);
    let files = script(&root, |_| {});
    let prefix = format!("{}/", check::display(&root));
    let relative =
        |file: &str, line: u64| (file.strip_prefix(&prefix).unwrap_or(file).to_owned(), line);
    for load_order in [false, true] {
        let options = Options {
            load_order,
            ..Options::default()
        };
        let report = check::run(&files, &options).unwrap();
        let mut expected: Vec<_> = manifest
            .findings
            .iter()
            .filter(|f| f.kind != FindingKind::Uncommitted)
            .filter(|f| load_order || f.kind != FindingKind::FkLoadOrder)
            .cloned()
            .collect();
        expected.sort();
        assert_eq!(
            placed(&report, &relative),
            expected,
            "load order: {load_order}"
        );
    }
    let out = tempfile::tempdir().unwrap();
    let fixed = fix_into(&files, &out.path().join("out"), Policy::default());
    let mut removed: Vec<Location> = fixed
        .removed
        .iter()
        .filter(|r| r.nulled.is_empty())
        .map(|r| {
            let (file, line) = relative(&r.file, r.line);
            Location { file, line }
        })
        .collect();
    removed.sort();
    let mut expected = manifest.fixes.removed.clone();
    expected.sort();
    assert_eq!(removed, expected);
    assert!(fixed.committed.is_empty());
}
