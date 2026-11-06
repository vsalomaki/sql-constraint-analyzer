use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;
use tempfile::TempDir;

#[expect(
    clippy::unwrap_used,
    reason = "a missing binary is a broken build, not a test result"
)]
fn sqlca_in(dir: &TempDir) -> Command {
    let mut command = Command::cargo_bin("sqlca").unwrap();
    command.current_dir(dir.path());
    command
}

#[expect(
    clippy::unwrap_used,
    reason = "a fixture that can't be written fails the test"
)]
fn workspace(files: &[(&str, &[u8])]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    dir
}

/// A relative path written with the platform's separator.
fn path_of(parts: &[&str]) -> PathBuf {
    parts.iter().collect()
}

struct Listed {
    role: String,
    path: PathBuf,
    order: String,
}

/// The listing as JSON Lines.
#[expect(clippy::unwrap_used, reason = "output that isn't JSON fails the test")]
fn listing(command: &mut Command) -> Vec<Listed> {
    let output = command.args(["--format", "json"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).unwrap();
            let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap().to_owned();
            Listed {
                role: field("role"),
                path: PathBuf::from(field("path")),
                order: field("order"),
            }
        })
        .collect()
}

fn paths(listing: &[Listed]) -> Vec<&Path> {
    listing.iter().map(|l| l.path.as_path()).collect()
}

#[test]
fn glob_matches_are_version_sorted_and_patterns_keep_their_order() {
    let dir = workspace(&[
        ("data/10_emp.sql", b""),
        ("data/2_dept.sql", b""),
        ("data/1_ddl.sql", b""),
        ("extra.sql", b""),
    ]);
    let found = listing(sqlca_in(&dir).args(["inputs", "extra.sql", "data/*.sql"]));
    assert_eq!(
        paths(&found),
        [
            Path::new("extra.sql"),
            &path_of(&["data", "1_ddl.sql"]),
            &path_of(&["data", "2_dept.sql"]),
            &path_of(&["data", "10_emp.sql"]),
        ]
    );
    assert_eq!(found[0].order, "as given");
    assert_eq!(found[1].order, "version-sorted glob matches");
}

#[test]
fn recursive_globs_match_nested_files() {
    let dir = workspace(&[("a/x.sql", b""), ("a/b/y.sql", b""), ("a/z.txt", b"")]);
    let found = listing(sqlca_in(&dir).args(["inputs", "a/**/*.sql"]));
    assert_eq!(found.len(), 2);
}

#[test]
fn the_text_listing_has_position_role_path_size_and_order() {
    let dir = workspace(&[("a.sql", b"12345")]);
    sqlca_in(&dir)
        .args(["inputs", "a.sql"])
        .assert()
        .success()
        .stdout(contains("   1  data       a.sql  5 bytes  (as given)\n"))
        .stderr(contains(
            "Inputs     1 files, 5 B: 0 schema, 1 data, 0 reference",
        ));
}

#[test]
fn files_from_stdin_keeps_the_given_order_and_accepts_crlf_and_blank_lines() {
    let dir = workspace(&[("b.sql", b""), ("a.sql", b"")]);
    let found = listing(
        sqlca_in(&dir)
            .args(["inputs", "--files-from", "-"])
            .write_stdin("b.sql\r\n\r\na.sql\r\n"),
    );
    assert_eq!(paths(&found), [Path::new("b.sql"), Path::new("a.sql")]);
    assert_eq!(found[0].order, "as listed in standard input");
}

#[test]
fn files_from_a_file_with_nul_separators() {
    let dir = workspace(&[("b.sql", b""), ("a.sql", b""), ("list", b"b.sql\0a.sql\0")]);
    let found = listing(sqlca_in(&dir).args(["inputs", "--files-from", "list", "-0"]));
    assert_eq!(paths(&found), [Path::new("b.sql"), Path::new("a.sql")]);
    assert_eq!(found[0].order, "as listed in list");
}

#[test]
fn files_from_entries_are_literal_not_globs() {
    let dir = workspace(&[("a.sql", b"")]);
    sqlca_in(&dir)
        .args(["inputs", "--files-from", "-"])
        .write_stdin("*.sql\n")
        .assert()
        .code(2)
        .stderr(contains("'*.sql'"));
}

#[test]
fn more_than_one_input_form_is_a_usage_error() {
    let dir = workspace(&[("a.sql", b""), ("list", b"a.sql\n"), ("m.sql", b"")]);
    let combinations: [&[&str]; 3] = [
        &["inputs", "a.sql", "--files-from", "list"],
        &["inputs", "a.sql", "--script", "m.sql"],
        &["inputs", "--files-from", "list", "--script", "m.sql"],
    ];
    for args in combinations {
        sqlca_in(&dir)
            .args(args)
            .assert()
            .code(2)
            .stdout("")
            .stderr(contains("cannot be used with"));
    }
}

#[test]
fn null_needs_files_from() {
    let dir = workspace(&[("a.sql", b"")]);
    sqlca_in(&dir)
        .args(["inputs", "-0", "a.sql"])
        .assert()
        .code(2)
        .stderr(contains("cannot be used with"));
}

#[test]
fn a_pattern_matching_nothing_and_a_missing_file_exit_2_naming_them() {
    let dir = workspace(&[("a.sql", b"")]);
    sqlca_in(&dir)
        .args(["inputs", "data/*.sql"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(contains("no file matches the pattern 'data/*.sql'"));
    sqlca_in(&dir)
        .args(["inputs", "gone.sql"])
        .assert()
        .code(2)
        .stderr(contains("input file 'gone.sql' does not exist"));
}

#[test]
fn no_input_at_all_exits_2() {
    let dir = workspace(&[]);
    sqlca_in(&dir)
        .arg("inputs")
        .assert()
        .code(2)
        .stderr(contains("no input files"));
}

#[test]
fn a_file_listed_twice_is_read_once_at_its_first_position_with_one_warning() {
    let dir = workspace(&[("a.sql", b""), ("b.sql", b"")]);
    let output = sqlca_in(&dir)
        .args(["inputs", "a.sql", "*.sql", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("warning:").count(), 1, "{stderr}");
    assert!(
        stderr.contains("'a.sql' is listed more than once; it is read once, at position 1"),
        "{stderr}"
    );
}

#[test]
fn schema_and_reference_files_come_first_with_their_roles() {
    let dir = workspace(&[("d.sql", b""), ("s.sql", b""), ("r.sql", b"")]);
    let found =
        listing(sqlca_in(&dir).args(["inputs", "d.sql", "--schema", "s.sql", "--reference=r.sql"]));
    let summary: Vec<_> = found
        .iter()
        .map(|l| (l.role.as_str(), l.path.as_path()))
        .collect();
    assert_eq!(
        summary,
        [
            ("schema", Path::new("s.sql")),
            ("reference", Path::new("r.sql")),
            ("data", Path::new("d.sql")),
        ]
    );
}

#[test]
fn a_script_is_followed_through_nested_includes() {
    let dir = workspace(&[
        ("load.sql", b"@@ddl\n@@data/main\n@@last.sql\n"),
        ("ddl.sql", b"create table t (a number);\n"),
        ("data/main.sql", b"@@part\n"),
        ("data/part.sql", b"insert into t values (1);\n"),
        ("last.sql", b""),
    ]);
    let found = listing(sqlca_in(&dir).args(["inputs", "--script", "load.sql"]));
    assert_eq!(
        paths(&found),
        [
            Path::new("load.sql"),
            Path::new("ddl.sql"),
            &path_of(&["data", "main.sql"]),
            &path_of(&["data", "part.sql"]),
            Path::new("last.sql"),
        ]
    );
    assert_eq!(found[1].order, "as included by load.sql");
}

#[test]
fn a_script_that_includes_itself_is_refused() {
    let dir = workspace(&[("a.sql", b"@@b\n"), ("b.sql", b"@@a\n")]);
    sqlca_in(&dir)
        .args(["inputs", "--script", "a.sql"])
        .assert()
        .code(2)
        .stderr(contains("includes itself"));
}

#[test]
fn utf16_input_is_refused_and_a_utf8_bom_is_accepted() {
    let dir = workspace(&[
        ("wide.sql", b"\xFF\xFEi\0n\0"),
        ("bom.sql", b"\xEF\xBB\xBFselect 1;\n"),
    ]);
    sqlca_in(&dir)
        .args(["inputs", "wide.sql"])
        .assert()
        .code(2)
        .stderr(contains("'wide.sql' looks like UTF-16"));
    sqlca_in(&dir)
        .args(["inputs", "bom.sql"])
        .assert()
        .success();
}

#[test]
fn json_lines_carry_position_role_path_bytes_and_order() {
    let dir = workspace(&[("a.sql", b"abc"), ("b.sql", b"")]);
    let assert = sqlca_in(&dir)
        .args(["inputs", "*.sql", "--format", "json"])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert_eq!(text.lines().count(), 2);
    assert_eq!(
        text.lines().next(),
        Some(
            r#"{"position":1,"role":"data","path":"a.sql","bytes":3,"order":"version-sorted glob matches"}"#
        )
    );
}

#[test]
fn check_and_fix_accept_the_input_options() {
    let dir = workspace(&[("a.sql", b"")]);
    let args = ["a.sql", "--encoding", "windows-1252", "--runner", "sqlcl"];
    sqlca_in(&dir)
        .arg("check")
        .args(args)
        .assert()
        .code(0)
        .stderr(contains("Result     0 findings"));
    sqlca_in(&dir)
        .args(["fix", "--out", "fixed"])
        .args(args)
        .assert()
        .code(0)
        .stderr(contains("Fixed      0 rows in 0 rounds"));
}

#[test]
fn check_and_fix_refuse_schema_files_without_data() {
    let dir = workspace(&[("schema.sql", b""), ("ref.sql", b"")]);
    for command in [&["check"][..], &["fix", "--out", "fixed"]] {
        sqlca_in(&dir)
            .args(command)
            .args(["--schema", "schema.sql", "--reference", "ref.sql"])
            .assert()
            .code(2)
            .stderr(contains("no data files to check"));
    }
    sqlca_in(&dir)
        .args(["inputs", "--schema", "schema.sql"])
        .assert()
        .success();
}

const TABLES: &[u8] = b"CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER, CONSTRAINT C_P FOREIGN KEY (P_ID) REFERENCES P (ID));\n";

const TABLES_WITHOUT_KEY: &[u8] =
    b"CREATE TABLE P (ID NUMBER PRIMARY KEY);\nCREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER);\n";

/// A child file given before its parent's, the tables between them.
fn parent_and_child() -> TempDir {
    workspace(&[
        ("c.sql", b"INSERT INTO C VALUES (1, 1);\nCOMMIT;\n"),
        ("ddl.sql", TABLES),
        ("p.sql", b"INSERT INTO P VALUES (1);\nCOMMIT;\n"),
    ])
}

#[test]
fn dependency_order_lists_parents_first_for_files_from() {
    let dir = parent_and_child();
    let output = sqlca_in(&dir)
        .args([
            "inputs",
            "c.sql",
            "p.sql",
            "--schema",
            "ddl.sql",
            "--dependency-order",
            "--format",
            "paths",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let list = String::from_utf8(output.stdout).unwrap();
    assert_eq!(list, "p.sql\nc.sql\n");

    let load_order = |stdin: &str| {
        let output = sqlca_in(&dir)
            .args([
                "check",
                "--load-order",
                "--format",
                "json",
                "--schema",
                "ddl.sql",
                "--files-from",
                "-",
            ])
            .write_stdin(stdin)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(load_order("c.sql\np.sql\n").contains("\"fk_load_order\""));
    assert!(!load_order(&list).contains("\"fk_load_order\""));
}

#[test]
fn the_dependency_order_says_why_each_data_file_moved() {
    let dir = parent_and_child();
    let found = listing(sqlca_in(&dir).args([
        "inputs",
        "c.sql",
        "p.sql",
        "--schema",
        "ddl.sql",
        "--dependency-order",
    ]));
    assert_eq!(
        paths(&found),
        [Path::new("ddl.sql"), Path::new("p.sql"), Path::new("c.sql")]
    );
    assert_eq!(found[0].order, "as given");
    assert!(found[1..].iter().all(|l| l.order == "parents first"));
}

#[test]
fn no_file_moves_across_table_ddl_so_every_row_keeps_its_constraints() {
    // The child's row comes before its key is added without validating old rows, so it is never judged by it;
    // moving it after the parent's file would move it after the key too.
    let dir = workspace(&[
        ("ddl.sql", TABLES_WITHOUT_KEY),
        ("c.sql", b"INSERT INTO C VALUES (1, 9);\nCOMMIT;\n"),
        (
            "fk.sql",
            b"ALTER TABLE C ADD CONSTRAINT C_P FOREIGN KEY (P_ID) REFERENCES P (ID) ENABLE NOVALIDATE;\n",
        ),
        ("p.sql", b"INSERT INTO P VALUES (1);\nCOMMIT;\n"),
    ]);
    let output = sqlca_in(&dir)
        .args([
            "inputs",
            "ddl.sql",
            "c.sql",
            "fk.sql",
            "p.sql",
            "--dependency-order",
            "--format",
            "paths",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let list = String::from_utf8(output.stdout).unwrap();
    assert_eq!(list, "ddl.sql\nc.sql\nfk.sql\np.sql\n");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("warning: c.sql is listed before p.sql, which holds parent rows for C_P"),
    );
    sqlca_in(&dir)
        .args(["check", "--files-from", "-"])
        .write_stdin(list)
        .assert()
        .code(0);
}

#[test]
fn files_that_depend_on_each_other_keep_the_given_order_with_a_warning() {
    let dir = workspace(&[
        (
            "ddl.sql",
            b"CREATE TABLE A (ID NUMBER PRIMARY KEY, B_ID NUMBER);\n\
              CREATE TABLE B (ID NUMBER PRIMARY KEY, A_ID NUMBER);\n\
              ALTER TABLE A ADD CONSTRAINT A_B FOREIGN KEY (B_ID) REFERENCES B (ID);\n\
              ALTER TABLE B ADD CONSTRAINT B_A FOREIGN KEY (A_ID) REFERENCES A (ID);\n",
        ),
        ("b.sql", b"INSERT INTO B VALUES (1, NULL);\nCOMMIT;\n"),
        ("a.sql", b"INSERT INTO A VALUES (1, 1);\nCOMMIT;\n"),
    ]);
    sqlca_in(&dir)
        .args([
            "inputs",
            "ddl.sql",
            "b.sql",
            "a.sql",
            "--dependency-order",
            "--format",
            "paths",
        ])
        .assert()
        .success()
        .stdout("ddl.sql\nb.sql\na.sql\n")
        .stderr(contains(
            "warning: b.sql is listed before a.sql, which holds parent rows for B_A",
        ));
}

#[test]
fn print0_ends_each_path_with_a_nul_and_leaves_out_schema_files() {
    let dir = parent_and_child();
    sqlca_in(&dir)
        .args([
            "inputs",
            "c.sql",
            "p.sql",
            "--schema",
            "ddl.sql",
            "--dependency-order",
            "--print0",
        ])
        .assert()
        .success()
        .stdout("p.sql\0c.sql\0");
}

#[test]
fn a_disabled_or_deferred_key_doesnt_order_files() {
    for state in ["DISABLE", "DEFERRABLE INITIALLY DEFERRED"] {
        let ddl = format!(
            "CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
             CREATE TABLE C (ID NUMBER PRIMARY KEY, P_ID NUMBER, CONSTRAINT C_P FOREIGN KEY (P_ID) REFERENCES P (ID) {state});\n"
        );
        let dir = workspace(&[
            ("c.sql", b"INSERT INTO C VALUES (1, 1);\nCOMMIT;\n"),
            ("ddl.sql", ddl.as_bytes()),
            ("p.sql", b"INSERT INTO P VALUES (1);\nCOMMIT;\n"),
        ]);
        sqlca_in(&dir)
            .args([
                "inputs",
                "c.sql",
                "ddl.sql",
                "p.sql",
                "--dependency-order",
                "--format",
                "paths",
            ])
            .assert()
            .success()
            .stdout("c.sql\nddl.sql\np.sql\n");
    }
}

#[test]
fn a_cycle_is_broken_inside_it_not_at_a_file_that_waits_on_it() {
    let dir = workspace(&[
        (
            "ddl.sql",
            b"CREATE TABLE A (ID NUMBER PRIMARY KEY, B_ID NUMBER);\n\
              CREATE TABLE B (ID NUMBER PRIMARY KEY, A_ID NUMBER);\n\
              CREATE TABLE D (ID NUMBER PRIMARY KEY, A_ID NUMBER);\n\
              ALTER TABLE A ADD CONSTRAINT A_B FOREIGN KEY (B_ID) REFERENCES B (ID);\n\
              ALTER TABLE B ADD CONSTRAINT B_A FOREIGN KEY (A_ID) REFERENCES A (ID);\n\
              ALTER TABLE D ADD CONSTRAINT D_A FOREIGN KEY (A_ID) REFERENCES A (ID);\n",
        ),
        ("d.sql", b"INSERT INTO D VALUES (1, 1);\nCOMMIT;\n"),
        ("a.sql", b"INSERT INTO A VALUES (1, NULL);\nCOMMIT;\n"),
        ("b.sql", b"INSERT INTO B VALUES (1, 1);\nCOMMIT;\n"),
    ]);
    let output = sqlca_in(&dir)
        .args([
            "inputs",
            "d.sql",
            "a.sql",
            "b.sql",
            "--schema",
            "ddl.sql",
            "--dependency-order",
            "--format",
            "paths",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // D waits only on A, so it follows A as soon as the cycle is broken there.
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "a.sql\nd.sql\nb.sql\n"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("warning:"))
        .collect();
    assert_eq!(
        warnings,
        [
            "warning: a.sql is listed before b.sql, which holds parent rows for A_B: they wait on each other, or \
             table DDL between them or rows of a table both hold keep them in order"
        ]
    );
}

#[test]
fn a_statement_the_order_cant_read_is_an_error_and_the_listing_still_comes() {
    let dir = workspace(&[
        (
            "c.sql",
            b"INSERT INTO C VALUES (1, 1);\nINSERT INTO C VALUES (2;\nCOMMIT;\n",
        ),
        ("ddl.sql", TABLES),
        ("p.sql", b"INSERT INTO P VALUES (1);\nCOMMIT;\n"),
    ]);
    sqlca_in(&dir)
        .args([
            "inputs",
            "c.sql",
            "p.sql",
            "--schema",
            "ddl.sql",
            "--dependency-order",
            "--format",
            "paths",
        ])
        .assert()
        .code(1)
        .stdout("p.sql\nc.sql\n")
        .stderr(contains("error: c.sql:2:"));
}

#[test]
fn files_inserting_into_one_table_keep_their_order_so_duplicates_stay_the_same() {
    // a.sql's row would wait for its parent in b.sql, but moving it after b.sql would make b.sql's row of the same
    // key the first one, and a.sql's the duplicate.
    let dir = workspace(&[
        ("ddl.sql", TABLES),
        ("a.sql", b"INSERT INTO C VALUES (1, 1);\nCOMMIT;\n"),
        (
            "b.sql",
            b"INSERT INTO P VALUES (1);\nINSERT INTO C VALUES (1, NULL);\nCOMMIT;\n",
        ),
    ]);
    sqlca_in(&dir)
        .args([
            "inputs",
            "a.sql",
            "b.sql",
            "--schema",
            "ddl.sql",
            "--dependency-order",
            "--format",
            "paths",
        ])
        .assert()
        .success()
        .stdout("a.sql\nb.sql\n")
        .stderr(contains(
            "warning: a.sql is listed before b.sql, which holds parent rows for C_P",
        ));
}
