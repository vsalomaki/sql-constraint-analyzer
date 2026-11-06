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
