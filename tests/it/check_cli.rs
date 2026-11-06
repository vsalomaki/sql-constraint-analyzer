//! `sqlca check` as a user runs it, from an export's root with relative paths: the text output (golden), JSON Lines
//! that equal the generator's manifest, exit codes, and what `-q`, `-v`, the cap and `--default-schema` change.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde::Deserialize;
use sqlca::finding::{Finding, FindingKind};

#[derive(Deserialize)]
struct Expected {
    options: Options,
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
struct Options {
    encoding: String,
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// `sqlca check` in an export's root: its `app/` files in `order.txt` order on stdin, `ref/` as reference files.
#[expect(
    clippy::unwrap_used,
    reason = "a missing binary or fixture fails the test"
)]
fn check_export(root: &Path, encoding: &str, args: &[&str]) -> (i32, String, String) {
    let order = fs::read_to_string(root.join("order.txt")).unwrap();
    let app = order
        .lines()
        .filter(|l| !l.starts_with("ref/"))
        .collect::<Vec<_>>()
        .join("\n");
    let output = Command::cargo_bin("sqlca")
        .unwrap()
        .current_dir(root)
        .args(["check", "--files-from", "-", "--encoding", encoding])
        .args(args)
        .args(["--reference", "ref/**/*.sql"])
        .write_stdin(app)
        .output()
        .unwrap();
    let text = |b: Vec<u8>| {
        String::from_utf8(b)
            .unwrap()
            .replace(env!("CARGO_PKG_VERSION"), "VERSION")
    };
    (
        output.status.code().unwrap(),
        text(output.stdout),
        untimed(&text(output.stderr)),
    )
}

/// `stderr` with its timings and rates as `T`, so it compares from run to run.
fn untimed(stderr: &str) -> String {
    stderr
        .lines()
        .map(|line| {
            let words: Vec<&str> = line.split(' ').collect();
            let timed = |i: usize| {
                words.get(i).is_some_and(|w| w.parse::<f64>().is_ok())
                    && words
                        .get(i + 1)
                        .is_some_and(|u| *u == "s" || u.ends_with("B/s"))
            };
            let mut out = Vec::with_capacity(words.len());
            let mut i = 0;
            while let Some(word) = words.get(i) {
                if timed(i) {
                    out.push("T");
                    i += 2;
                } else {
                    out.push(word);
                    i += 1;
                }
            }
            out.join(" ") + "\n"
        })
        .collect()
}

#[expect(clippy::unwrap_used, reason = "a malformed manifest fails the test")]
fn expected(root: &Path) -> Expected {
    serde_json::from_str(&fs::read_to_string(root.join("expected.json")).unwrap()).unwrap()
}

#[test]
fn the_text_output_of_a_generated_export() {
    let root = fixture("gen/oracle/separate-sqldev");
    let (code, stdout, stderr) = check_export(&root, "utf-8", &[]);
    assert_eq!(code, 1);
    insta::assert_snapshot!("separate_sqldev_stdout", stdout);
    insta::assert_snapshot!("separate_sqldev_stderr", stderr);
}

#[test]
fn json_lines_equal_the_manifest_in_both_modes() {
    for name in [
        "separate-sqldev",
        "single-all-crlf-1252",
        "type-after-data-shuffled",
    ] {
        let root = fixture(&format!("gen/oracle/{name}"));
        let manifest = expected(&root);
        for load_order in [false, true] {
            let mut args = vec!["--format", "json", "--max-per-constraint", "0"];
            if load_order {
                args.push("--load-order");
            }
            let (code, stdout, stderr) = check_export(&root, &manifest.options.encoding, &args);
            assert_eq!(code, 1, "{name}: {stderr}");
            let mut lines: Vec<serde_json::Value> = stdout
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            let summary = lines.pop().unwrap();
            let found: BTreeSet<Finding> = lines
                .into_iter()
                .map(|l| serde_json::from_value(l).unwrap())
                .collect();
            let wanted: BTreeSet<Finding> = manifest
                .findings
                .iter()
                .filter(|f| load_order || f.kind != FindingKind::FkLoadOrder)
                .cloned()
                .collect();
            assert_eq!(found, wanted, "{name} load_order={load_order}");
            assert_eq!(summary["summary"]["findings"], wanted.len(), "{name}");
            assert_eq!(summary["summary"]["exit"], 1, "{name}");
        }
    }
}

#[test]
fn a_clean_export_exits_0_and_minus_q_shows_only_the_result() {
    // SQLcl writes no COMMIT into its data files.
    let (code, _, stderr) = check_export(&fixture("sqlcl"), "utf-8", &["-q"]);
    assert_eq!(code, 1, "{stderr}");
    assert_eq!(
        stderr,
        "Result     7 findings (no COMMIT 7) · 1 warnings · 0 errors → exit 1\n"
    );
    let (code, stdout, stderr) =
        check_export(&fixture("sqlcl"), "utf-8", &["-q", "--no-commit-check"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        "Result     0 findings · 1 warnings · 0 errors → exit 0\n"
    );
}

#[test]
fn minus_v_lists_the_statements_the_check_does_not_follow() {
    let (_, _, stderr) = check_export(&fixture("sqlcl"), "utf-8", &[]);
    assert!(
        stderr.contains("warning: 8 statements change data in ways the check doesn't follow"),
        "{stderr}"
    );
    assert!(stderr.contains("(-v lists them)"), "{stderr}");
    let (_, _, stderr) = check_export(&fixture("sqlcl"), "utf-8", &["-v"]);
    assert_eq!(stderr.matches(": INSERT … SELECT\n").count(), 8, "{stderr}");
}

#[test]
fn the_cap_hides_findings_but_not_their_count() {
    let root = fixture("gen/oracle/separate-sqldev");
    let (_, stdout, stderr) = check_export(&root, "utf-8", &["--max-per-constraint", "1"]);
    assert_eq!(
        stdout
            .matches("NULL in NOT NULL column APP.EMPLOYEES.EMAIL")
            .count(),
        1,
        "{stdout}"
    );
    assert!(
        stderr.contains("note: 2 more findings not shown, past the cap in 2 constraints"),
        "{stderr}"
    );
    assert!(stderr.contains("Result     26 findings"), "{stderr}");
    let (_, _, stderr) = check_export(&root, "utf-8", &["--max-per-constraint", "1", "-v"]);
    assert!(
        stderr.contains("\n  1 of NULL in NOT NULL in APP.EMPLOYEES (EMAIL)\n"),
        "{stderr}"
    );
}

#[test]
fn the_default_schema_resolves_unqualified_names_and_folds_unless_quoted() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("t.sql"),
        "CREATE TABLE APP.T (ID NUMBER PRIMARY KEY);\nCREATE TABLE \"app\".T (ID NUMBER PRIMARY KEY);\n\
         INSERT INTO t VALUES (1);\nINSERT INTO t VALUES (1);\n",
    )
    .unwrap();
    let run = |extra: &[&str]| {
        let output = Command::cargo_bin("sqlca")
            .unwrap()
            .current_dir(dir.path())
            .args(["check", "t.sql", "-q"])
            .args(extra)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(run(&[]).matches("exists in several schemas").count(), 2);
    assert!(run(&["--default-schema", "app"]).contains("t.sql:4: duplicate key of APP.T: ID='1'"));
    assert!(
        run(&["--default-schema", "\"app\""])
            .contains("t.sql:4: duplicate key of \"app\".T: ID='1'")
    );
}

#[test]
fn unnamed_constraints_of_one_table_are_capped_apart() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("t.sql"),
        "CREATE TABLE T (ID NUMBER PRIMARY KEY, V NUMBER UNIQUE);\n\
         INSERT INTO T VALUES (1, 10);\nINSERT INTO T VALUES (1, 20);\nINSERT INTO T VALUES (2, 10);\n",
    )
    .unwrap();
    let output = Command::cargo_bin("sqlca")
        .unwrap()
        .current_dir(dir.path())
        .args(["check", "t.sql", "-q", "--max-per-constraint", "1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("t.sql:3: duplicate key of T: ID='1'"),
        "{stdout}"
    );
    assert!(
        stdout.contains("t.sql:4: duplicate key of T: V='10'"),
        "{stdout}"
    );
}

#[test]
fn progress_lines_and_the_catalog_listing() {
    let root = fixture("gen/oracle/separate-sqldev");
    let (_, _, plain) = check_export(&root, "utf-8", &[]);
    assert!(
        plain.contains("[2/3] app/data/EMPLOYEES.sql  8.2 kB  T\n"),
        "{plain}"
    );
    let (_, _, never) = check_export(&root, "utf-8", &["--progress", "never"]);
    assert!(!never.contains("[2/3] app/"), "{never}");
    assert!(
        never.contains("[2/3] Rows      23 files, 40.9 kB · 215 rows · T · T\n"),
        "{never}"
    );
    assert!(!never.contains("primary key"), "{never}");
    let (_, _, verbose) = check_export(&root, "utf-8", &["--progress", "never", "-v"]);
    for line in [
        "  APP.EMPLOYEES  10 columns\n",
        "    EMPLOYEES_PK  primary key (EMP_ID)\n",
        "    EMPLOYEES_EMAIL_UIX  unique index (EMAIL)\n",
        "    PROJECTS_DEPT_FK  foreign key (DEPT_NAME) → APP.DEPARTMENTS (NAME), not validated\n",
        "    LEGACY_NOTES_EMP_FK  foreign key (EMP_ID) → APP.EMPLOYEES (EMP_ID), disabled\n",
        "    DEPARTMENTS_MANAGER_FK  foreign key (MANAGER_ID) → APP.EMPLOYEES (EMP_ID), deferred\n",
    ] {
        assert!(verbose.contains(line), "{line:?} in {verbose}");
    }
}

#[test]
fn fix_shows_its_progress_unless_told_not_to() {
    let run = |args: &[&str]| {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("t.sql"), ORPHAN).unwrap();
        let output = Command::cargo_bin("sqlca")
            .unwrap()
            .current_dir(dir.path())
            .args(["fix", "t.sql", "--in-place"])
            .args(args)
            .output()
            .unwrap();
        (dir, String::from_utf8(output.stderr).unwrap())
    };
    let (dir, plain) = run(&[]);
    let fixed = fs::metadata(dir.path().join("t.sql")).unwrap().len();
    assert!(fixed < ORPHAN.len() as u64);
    let verify = format!("[verify] t.sql  {fixed} B  ");
    for stage in ["[check 1] t.sql  ", "[write] t.sql  ", &verify] {
        assert!(plain.contains(stage), "{stage:?} in {plain}");
    }
    for quiet in [&["--progress", "never"][..], &["-q"]] {
        let (_, stderr) = run(quiet);
        assert!(!stderr.contains("t.sql  "), "{quiet:?}: {stderr}");
    }

    // An unchanged file is written too with --out, copied in full where it can't be a hard link.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("t.sql"), ORPHAN).unwrap();
    fs::write(dir.path().join("u.sql"), "INSERT INTO P VALUES (2);\n").unwrap();
    let output = Command::cargo_bin("sqlca")
        .unwrap()
        .current_dir(dir.path())
        .args(["fix", "t.sql", "u.sql", "--out", "out"])
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    for stage in ["[write] t.sql  ", "[write] u.sql  "] {
        assert!(stderr.contains(stage), "{stage:?} in {stderr}");
    }
}

#[test]
fn fix_writes_fixed_copies_and_a_report_folder() {
    let root = fixture("gen/oracle/separate-sqldev");
    let out = tempfile::tempdir().unwrap();
    let order = fs::read_to_string(root.join("order.txt")).unwrap();
    let app = order
        .lines()
        .filter(|l| !l.starts_with("ref/"))
        .collect::<Vec<_>>()
        .join("\n");
    let fixed = out.path().join("fixed");
    let output = Command::cargo_bin("sqlca")
        .unwrap()
        .current_dir(&root)
        .args([
            "fix",
            "--files-from",
            "-",
            "--reference",
            "ref/**/*.sql",
            "--out",
        ])
        .arg(&fixed)
        .write_stdin(app)
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    // Structure errors are never fixed, so findings remain.
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "Fixed      22 rows in 2 rounds (missing parent 9, duplicate key 9, NULL in NOT NULL 4)"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "Result     4 findings (unknown table 1, unknown column 1, value count 1, no COMMIT 1)"
        ),
        "{stderr}"
    );
    let rejects = fs::read_to_string(fixed.join("sqlca-report/rejects.sql")).unwrap();
    assert_eq!(rejects.matches("\n-- app/data/").count(), 22, "{rejects}");
    let report = fs::read_to_string(fixed.join("sqlca-report/fix-report.txt")).unwrap();
    assert!(report.contains("Still in the output:"), "{report}");
    assert!(fixed.join("ref/data/COUNTRIES.sql").exists());
    assert_eq!(
        fs::read(root.join("ref/data/COUNTRIES.sql")).unwrap(),
        fs::read(fixed.join("ref/data/COUNTRIES.sql")).unwrap()
    );

    let kept = out.path().join("kept");
    let output = Command::cargo_bin("sqlca")
        .unwrap()
        .current_dir(&root)
        .args([
            "fix",
            "--files-from",
            "-",
            "--reference",
            "ref/**/*.sql",
            "--on-fk",
            "keep",
            "-q",
            "--out",
        ])
        .arg(&kept)
        .write_stdin(
            order
                .lines()
                .filter(|l| !l.starts_with("ref/"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("Result     13 findings (missing parent 9,"),
        "{stderr}"
    );
}

#[test]
fn a_report_folder_that_already_exists_in_the_output_stops_the_run() {
    // On a case-insensitive file system (macOS by default) the copied SQLCA-REPORT is the report folder.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in");
    fs::create_dir_all(input.join("SQLCA-REPORT")).unwrap();
    let kept = "CREATE TABLE U (ID NUMBER);\nINSERT INTO U VALUES (1);\nCOMMIT;\n";
    fs::write(input.join("SQLCA-REPORT/rejects.sql"), kept).unwrap();
    fs::write(
        input.join("t.sql"),
        "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\nCOMMIT;\n",
    )
    .unwrap();
    let output = Command::cargo_bin("sqlca")
        .unwrap()
        .current_dir(&input)
        .args(["fix", "t.sql", "SQLCA-REPORT/rejects.sql", "-q", "--out"])
        .arg(dir.path().join("out"))
        .output()
        .unwrap();
    let copied = fs::read_to_string(dir.path().join("out/SQLCA-REPORT/rejects.sql")).unwrap();
    assert_eq!(copied, kept, "the copied input is never overwritten");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let case_insensitive = dir.path().join("out/sqlca-report").exists()
        && !dir.path().join("out/sqlca-report/fix-report.txt").exists();
    if case_insensitive {
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains("report folder") && stderr.contains("isn't empty"),
            "{stderr}"
        );
    } else {
        assert_eq!(output.status.code(), Some(0), "{stderr}");
    }
}

const ORPHAN: &str = "CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
                      CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER REFERENCES P);\n\
                      INSERT INTO P VALUES (1);\nINSERT INTO C VALUES (2, 9);\nCOMMIT;\n";

/// `sqlca fix t.sql ARGS` in a new folder holding `t.sql` with an orphan.
#[expect(
    clippy::unwrap_used,
    reason = "a missing binary or folder fails the test"
)]
fn fix_orphan(args: &[&str]) -> (tempfile::TempDir, i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("t.sql"), ORPHAN).unwrap();
    let output = Command::cargo_bin("sqlca")
        .unwrap()
        .current_dir(dir.path())
        .args(["fix", "t.sql", "--progress", "never"])
        .args(args)
        .output()
        .unwrap();
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (
        dir,
        output.status.code().unwrap(),
        text(output.stdout),
        text(output.stderr),
    )
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn fix_in_place_keeps_backups_and_a_report_named_by_the_time() {
    let (dir, code, _, stderr) = fix_orphan(&["--in-place"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains(
            "Output     the inputs, in place; each changed one kept first as <file>.orig"
        ),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("t.sql.orig")).unwrap(),
        ORPHAN
    );
    assert!(
        !fs::read_to_string(dir.path().join("t.sql"))
            .unwrap()
            .contains("(2, 9)")
    );
    let names = names(dir.path());
    let [report, fixed, backup] = names.as_slice() else {
        panic!("{names:?}");
    };
    assert_eq!((fixed.as_str(), backup.as_str()), ("t.sql", "t.sql.orig"));
    let stamp = report.strip_prefix("sqlca-fix-").unwrap();
    assert!(
        stamp.len() == 16 && stamp.starts_with("20") && stamp.ends_with('Z'),
        "{report}"
    );
    assert!(
        stderr.contains(&format!(
            "Report     {}",
            dir.path().canonicalize().unwrap().join(report).display()
        )),
        "{stderr}"
    );
    assert_eq!(
        self::names(&dir.path().join(report)),
        ["fix-report.json", "fix-report.txt", "rejects.sql"]
    );
}

#[test]
fn fix_refuses_unclear_or_unsafe_destinations() {
    for args in [
        &["--in-place", "--no-backup", "--no-report"][..],
        &[][..],
        &["--in-place", "--out", "o"][..],
        &["--out", "o", "--backup", ".bak"][..],
        &["--out", "o", "--no-backup"][..],
        &["--in-place", "--backup", ""][..],
        &["--in-place", "--backup", "x/y"][..],
        &["--in-place", "--report-dir", "r", "--no-report"][..],
    ] {
        let (dir, code, _, stderr) = fix_orphan(args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert_eq!(names(dir.path()), ["t.sql"], "{args:?}: nothing written");
        assert_eq!(
            fs::read_to_string(dir.path().join("t.sql")).unwrap(),
            ORPHAN
        );
    }
    let (_, _, _, stderr) = fix_orphan(&["--in-place", "--no-backup", "--no-report"]);
    assert!(
        stderr.contains("would keep the removed rows nowhere"),
        "{stderr}"
    );
}

#[test]
fn fix_takes_the_policy_and_annotation_options() {
    let (dir, code, _, stderr) = fix_orphan(&[
        "--out",
        "o",
        "--annotate",
        "--on-duplicate",
        "keep-last",
        "--fix-assumed",
    ]);
    assert_eq!(code, 0, "{stderr}");
    let fixed = fs::read_to_string(dir.path().join("o/t.sql")).unwrap();
    assert!(
        fixed.ends_with(
            "INSERT INTO P VALUES (1);\n-- sqlca fix removed an INSERT here (missing parent for a foreign key of C); see rejects.sql\nCOMMIT;\n"
        ),
        "{fixed}"
    );
}

#[test]
fn a_dry_run_prints_the_report_it_would_write() {
    let (dir, code, stdout, stderr) = fix_orphan(&["--in-place", "--dry-run"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(names(dir.path()), ["t.sql"], "nothing written");
    assert!(stdout.contains("Dry run: nothing was written"), "{stdout}");
    assert!(
        stdout.contains("Would fix  1 rows in 1 round (missing parent 1)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("t.sql, original kept beside it as t.sql.orig"),
        "{stdout}"
    );
    assert!(stderr.contains("(would be)"), "{stderr}");

    let (dir, code, stdout, stderr) = fix_orphan(&["--out", "o", "--dry-run", "--format", "json"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(names(dir.path()), ["t.sql"], "nothing written");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["removed"][0]["line"], 4);
    assert!(json["files"][0].get("sha256_after").is_none(), "{json}");
}
