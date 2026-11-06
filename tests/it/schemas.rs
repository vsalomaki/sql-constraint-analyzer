//! Unqualified table names in several exports at once: each folder is a schema of its own, so tables of the same name
//! in two exports stay apart.

use std::fs;

use sqlca::check::{self, FolderSchema, Options, Report};
use sqlca::finding::{Finding, FindingKind};
use sqlca::input::{InputError, InputFile, OrderSource, Role};

const PARENT_AND_CHILD: &str = "CREATE TABLE T (ID NUMBER PRIMARY KEY, P NUMBER);\n\
                                CREATE TABLE P (ID NUMBER PRIMARY KEY);\n\
                                ALTER TABLE T ADD CONSTRAINT T_P FOREIGN KEY (P) REFERENCES P (ID);\n";

/// Checks `scripts`, each a path below a new folder and its SQL, in the order given.
#[expect(
    clippy::unwrap_used,
    reason = "a folder that can't be written fails the test"
)]
fn check_in_folders(
    scripts: &[(&str, &str)],
    options: &Options,
) -> (tempfile::TempDir, Result<Report, InputError>) {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<InputFile> = scripts
        .iter()
        .map(|(name, sql)| {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let sql = format!("{sql}COMMIT;\n");
            fs::write(&path, &sql).unwrap();
            InputFile {
                path,
                role: Role::Data,
                bytes: u64::try_from(sql.len()).unwrap(),
                order: OrderSource::Given,
            }
        })
        .collect();
    let report = check::run(&files, options);
    (dir, report)
}

/// Each finding's kind, table and constraint, and its file's name below the folder.
fn found(report: &Report) -> Vec<(FindingKind, String, Option<String>, String)> {
    report
        .findings
        .iter()
        .map(|f: &Finding| {
            let file = f.file.as_deref().unwrap_or_default();
            let short = file.rsplit(['/', '\\']).take(2).collect::<Vec<_>>();
            let short = short.into_iter().rev().collect::<Vec<_>>().join("/");
            (f.kind, f.table.clone(), f.constraint.clone(), short)
        })
        .collect()
}

fn two_exports() -> [(&'static str, String); 2] {
    [
        (
            "a/x.sql",
            format!(
                "{PARENT_AND_CHILD}INSERT INTO P VALUES (1);\nINSERT INTO T VALUES (1, 1);\nINSERT INTO T VALUES (2, 1);\n"
            ),
        ),
        (
            "b/x.sql",
            format!(
                "{PARENT_AND_CHILD}INSERT INTO P VALUES (5);\nINSERT INTO T VALUES (1, 5);\nINSERT INTO T VALUES (2, 1);\n"
            ),
        ),
    ]
}

#[test]
fn two_exports_with_the_same_unqualified_tables_stay_apart() {
    let exports = two_exports();
    let scripts: Vec<(&str, &str)> = exports.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let (_dir, report) = check_in_folders(&scripts, &Options::default());
    let report = report.unwrap();
    // B's row points at a parent only A has: no false duplicates, and the missing parent is found.
    assert_eq!(
        found(&report),
        [(
            FindingKind::FkMissingParent,
            "B.T".to_owned(),
            Some("B.T_P".to_owned()),
            "b/x.sql".to_owned()
        )]
    );
    assert!(report.catalog.is_empty(), "{:?}", report.catalog);
}

#[test]
fn without_folder_schemas_the_second_export_creates_its_tables_again() {
    let exports = two_exports();
    let scripts: Vec<(&str, &str)> = exports.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let off = Options {
        folder_schema: FolderSchema::Off,
        ..Options::default()
    };
    let (_dir, report) = check_in_folders(&scripts, &off);
    let Err(InputError::TableCreatedTwice {
        table,
        first,
        again,
    }) = report
    else {
        panic!("{report:?}");
    };
    assert_eq!(table, "T");
    assert!(first.ends_with("x.sql:1") && first.contains('a'), "{first}");
    assert!(
        again.ends_with("b/x.sql:1") || again.ends_with("b\\x.sql:1"),
        "{again}"
    );
}

#[test]
fn top_takes_the_first_folder_so_tables_and_data_folders_share_a_schema() {
    let ddl = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\n";
    let scripts = [
        ("a/tables/t.sql", ddl),
        (
            "a/data/t.sql",
            "INSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\n",
        ),
        ("b/tables/t.sql", ddl),
        ("b/data/t.sql", "INSERT INTO T VALUES (1);\n"),
    ];
    let top = Options {
        folder_schema: FolderSchema::Top,
        ..Options::default()
    };
    let (_dir, report) = check_in_folders(&scripts, &top);
    let found = found(&report.unwrap());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        (found[0].0, found[0].1.as_str(), found[0].3.as_str()),
        (FindingKind::DuplicateKey, "A.T", "data/t.sql")
    );
    // Taken by their own folders, both exports' tables are in schema TABLES.
    let (_dir, report) = check_in_folders(&scripts, &Options::default());
    assert!(
        matches!(&report, Err(InputError::TableCreatedTwice { table, .. }) if table == "TABLES.T"),
        "{report:?}"
    );
}

#[test]
fn a_name_its_folder_lacks_means_the_only_table_of_that_name() {
    let exports = two_exports();
    let (_, only_a) = &exports[0];
    let scripts = [
        ("a/x.sql", only_a.as_str()),
        (
            "c/rows.sql",
            "INSERT INTO T VALUES (3, 1);\nINSERT INTO T VALUES (4, 9);\n",
        ),
    ];
    let (_dir, report) = check_in_folders(&scripts, &Options::default());
    assert_eq!(
        found(&report.unwrap()),
        [(
            FindingKind::FkMissingParent,
            "A.T".to_owned(),
            Some("A.T_P".to_owned()),
            "c/rows.sql".to_owned()
        )]
    );

    // With the table in two other folders, a name the row's folder lacks is ambiguous.
    let exports = two_exports();
    let mut scripts: Vec<(&str, &str)> = exports.iter().map(|(n, s)| (*n, s.as_str())).collect();
    scripts.push(("c/rows.sql", "INSERT INTO T VALUES (3, 1);\n"));
    let (_dir, report) = check_in_folders(&scripts, &Options::default());
    let found = found(&report.unwrap());
    assert!(
        found.contains(&(
            FindingKind::AmbiguousTable,
            "T".to_owned(),
            None,
            "c/rows.sql".to_owned()
        )),
        "{found:?}"
    );
}

#[test]
fn a_default_schema_and_qualified_names_beat_the_folder() {
    let rows = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\n";
    let scripts = [
        ("a/x.sql", rows),
        ("b/y.sql", "INSERT INTO Q.U VALUES (1);\n"),
    ];
    let given = Options {
        default_schema: Some("D".to_owned()),
        ..Options::default()
    };
    let (_dir, report) = check_in_folders(&scripts, &given);
    let found = found(&report.unwrap());
    assert!(
        found
            .iter()
            .any(|f| f.0 == FindingKind::DuplicateKey && f.1 == "D.T"),
        "{found:?}"
    );
    // A qualified name keeps its schema whatever the folder.
    assert!(
        found
            .iter()
            .any(|f| f.0 == FindingKind::UnknownTable && f.1 == "Q.U"),
        "{found:?}"
    );
}

#[test]
fn files_in_the_shared_folder_have_no_folder_schema_and_odd_names_are_kept() {
    let rows = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nINSERT INTO T VALUES (1);\n";
    let (_dir, report) = check_in_folders(&[("x.sql", rows)], &Options::default());
    let found_here = found(&report.unwrap());
    assert_eq!(found_here.len(), 1, "{found_here:?}");
    assert_eq!(found_here[0].1, "T");

    let scripts = [
        ("my-schema/x.sql", rows),
        ("app/y.sql", "INSERT INTO T VALUES (2);\n"),
    ];
    let (_dir, report) = check_in_folders(&scripts, &Options::default());
    let found = found(&report.unwrap());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].1, "\"my-schema\".T");
}

/// A folder is the file's real one: `a/../b/x.sql` is in `b`, however the path to it is written.
#[test]
fn a_file_s_folder_is_its_real_one() {
    let qualified = |schema: &str| {
        format!("CREATE TABLE {schema}.T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\n")
    };
    let (a, b) = (qualified("A"), qualified("B"));
    let (_dir, report) = check_in_folders(
        &[("a/x.sql", &a), ("b/x.sql", &b), ("load.sql", "")],
        &Options::default(),
    );
    let plain = found(&report.unwrap());
    assert!(plain.is_empty(), "{plain:?}");
    for mode in [FolderSchema::Immediate, FolderSchema::Top] {
        let options = Options {
            folder_schema: mode,
            ..Options::default()
        };
        let (_dir, report) = check_in_folders(
            &[("a/x.sql", &a), ("a/../b/x.sql", &b), ("load.sql", "")],
            &options,
        );
        assert_eq!(found(&report.unwrap()), plain, "{mode:?}");
    }
}

/// A link counts where it points, as `fix --out` writes after it: two links in different folders to files in one
/// folder are in no folder schema, so the tables they both create stop the check before anything is written.
#[cfg(unix)]
#[test]
fn a_linked_file_is_in_the_folder_it_points_to() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    fs::create_dir_all(&store).unwrap();
    let mut files = Vec::new();
    for (folder, target) in [("a", "one.sql"), ("b", "two.sql")] {
        let sql = "CREATE TABLE T (ID NUMBER PRIMARY KEY);\nINSERT INTO T VALUES (1);\nCOMMIT;\n";
        fs::write(store.join(target), sql).unwrap();
        let link = dir.path().join(folder).join("x.sql");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(store.join(target), &link).unwrap();
        files.push(InputFile {
            path: link,
            role: Role::Data,
            bytes: u64::try_from(sql.len()).unwrap(),
            order: OrderSource::Given,
        });
    }
    let out = dir.path().join("fixed");
    let error = sqlca::fix::fix(
        &files,
        &Options::default(),
        sqlca::fix::Policy::default(),
        &sqlca::fix::Target::out(out.clone()),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("table T is created again"),
        "{error}"
    );
    assert!(!out.exists(), "nothing is written");
}
