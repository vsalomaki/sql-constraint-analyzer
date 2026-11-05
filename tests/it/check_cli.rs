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
        text(output.stderr),
    )
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
    let (code, stdout, stderr) = check_export(&fixture("sqlcl"), "utf-8", &["-q"]);
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
        stderr.contains(
            "note: 1 more findings for NULL in NOT NULL in APP.EMPLOYEES (EMAIL) not shown"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("Result     25 findings"), "{stderr}");
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
