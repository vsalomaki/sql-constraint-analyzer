use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sqlca::finding::{Finding, FindingKind, Location};
use sqlca::synth::{self, Encoding, Layout, LineEnding, Manifest, Noise, Options, Placement};

#[expect(
    clippy::unwrap_used,
    reason = "a failed generation fails the test that called it"
)]
fn generate(options: &Options) -> (tempfile::TempDir, Manifest) {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export");
    let manifest = synth::generate(options, &out).unwrap();
    (dir, manifest)
}

fn root(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("export")
}

fn small() -> Options {
    Options {
        rows: synth::MIN_ROWS,
        ..Options::default()
    }
}

/// Every file under `dir`, by relative path.
#[expect(
    clippy::unwrap_used,
    reason = "an unreadable test folder fails the test that called it"
)]
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn constraints(findings: &[Finding], kind: FindingKind) -> BTreeSet<String> {
    findings
        .iter()
        .filter(|f| f.kind == kind)
        .filter_map(|f| f.constraint.clone())
        .collect()
}

#[test]
fn the_same_seed_and_options_give_the_same_bytes() {
    let options = Options {
        noise: Noise::all(),
        shuffle_order: true,
        ..small()
    };
    let (a, _) = generate(&options);
    let (b, _) = generate(&options);
    assert_eq!(tree(&root(&a)), tree(&root(&b)));
    let (c, _) = generate(&Options { seed: 2, ..options });
    assert_ne!(tree(&root(&a)), tree(&root(&c)));
}

#[test]
fn every_finding_points_at_the_row_it_is_about() {
    for layout in [Layout::Single, Layout::Separate, Layout::Type] {
        let (dir, manifest) = generate(&Options {
            layout,
            noise: Noise::all(),
            ..small()
        });
        let locations = manifest.findings.iter().flat_map(|f| {
            let at = f.file.clone().zip(f.line);
            let first = f.first.as_ref().map(|l| (l.file.clone(), l.line));
            at.into_iter().chain(first)
        });
        for (file, line) in locations {
            let text = fs::read_to_string(root(&dir).join(&file)).unwrap();
            let row = text
                .lines()
                .nth(usize::try_from(line - 1).unwrap())
                .unwrap()
                .trim_start()
                .to_ascii_lowercase();
            assert!(
                row.starts_with("insert into ") || row.starts_with("into "),
                "{layout:?} {file}:{line}: {row}"
            );
        }
    }
}

#[test]
fn wrapped_statements_spread_one_insert_over_several_lines() {
    let noise = Noise {
        wrapped_statements: true,
        ..Noise::sqldev()
    };
    let (dir, _) = generate(&Options { noise, ..small() });
    let text = fs::read_to_string(root(&dir).join("app/data/EMPLOYEES.sql")).unwrap();
    let continuation_lines = text.lines().filter(|l| l.starts_with("    ")).count();
    assert!(continuation_lines > 10, "{text}");
}

#[test]
fn every_kind_of_plant_appears_at_the_smallest_size() {
    let (_dir, manifest) = generate(&Options {
        noise: Noise::all(),
        ..small()
    });
    let kinds: BTreeSet<FindingKind> = manifest.findings.iter().map(|f| f.kind).collect();
    for kind in [
        FindingKind::FkMissingParent,
        FindingKind::FkLoadOrder,
        FindingKind::DuplicateKey,
        FindingKind::NotNull,
        FindingKind::UnknownTable,
        FindingKind::UnknownColumn,
        FindingKind::ValueCountMismatch,
    ] {
        assert!(kinds.contains(&kind), "{kind:?} missing");
    }
    assert_eq!(
        constraints(&manifest.findings, FindingKind::FkMissingParent),
        [
            "APP.ASSIGNMENTS_EMP_FK",
            "APP.ASSIGNMENTS_PROJECT_FK",
            "APP.AUDITLOG_EMP_FK",
            "APP.DEPARTMENTS_COUNTRY_FK",
            "APP.DEPARTMENTS_MANAGER_FK",
            "APP.EMPLOYEES_DEPT_FK",
            "APP.EMPLOYEES_MANAGER_FK",
            "APP.PROJECTS_DEPT_FK",
            "APP.TIMESHEETS_ASSIGNMENT_FK",
        ]
        .map(String::from)
        .into(),
    );
    assert_eq!(
        constraints(&manifest.findings, FindingKind::DuplicateKey),
        [
            "APP.ASSIGNMENTS_PK",
            "APP.ASSIGNMENTS_ROLE_UK",
            "APP.DEPARTMENTS_NAME_UK",
            "APP.DEPARTMENTS_PK",
            "APP.EMPLOYEES_EMAIL_UIX",
            "APP.EMPLOYEES_PK",
            "APP.PROJECTS_EXTREF_UK",
            "APP.PROJECTS_PK",
            "APP.TIMESHEETS_PK",
        ]
        .map(String::from)
        .into(),
    );
    assert!(
        manifest.findings.iter().any(|f| f.constraint.as_deref()
            == Some("APP.ASSIGNMENTS_ROLE_UK")
            && f.key.get(1) == Some(&None)),
        "the partially NULL unique key duplicate"
    );
    assert!(
        manifest
            .findings
            .iter()
            .all(|f| f.constraint.as_deref() != Some("APP.LEGACY_NOTES_EMP_FK")),
        "the disabled foreign key is never reported"
    );
}

#[test]
fn constraints_after_the_data_give_no_load_order_findings_and_skip_novalidate() {
    let options = Options {
        constraints: Placement::AfterData,
        shuffle_order: true,
        ..small()
    };
    let (_dir, manifest) = generate(&options);
    assert!(
        manifest
            .findings
            .iter()
            .all(|f| f.kind != FindingKind::FkLoadOrder)
    );
    assert!(
        manifest
            .findings
            .iter()
            .all(|f| f.constraint.as_deref() != Some("APP.PROJECTS_DEPT_FK"))
    );
    assert!(!constraints(&manifest.findings, FindingKind::FkMissingParent).is_empty());
}

#[test]
fn shuffled_data_reports_children_loaded_before_their_parents() {
    let before = generate(&small()).1;
    let shuffled = generate(&Options {
        shuffle_order: true,
        ..small()
    })
    .1;
    let count = |m: &Manifest| {
        m.findings
            .iter()
            .filter(|f| f.kind == FindingKind::FkLoadOrder)
            .count()
    };
    assert!(
        count(&shuffled) > count(&before),
        "{} vs {}",
        count(&shuffled),
        count(&before)
    );
    assert!(
        shuffled
            .findings
            .iter()
            .all(|f| f.kind != FindingKind::FkLoadOrder
                || f.constraint.as_deref() != Some("APP.DEPARTMENTS_MANAGER_FK")),
        "the deferred foreign key has no load order"
    );
}

#[test]
fn omitted_data_becomes_one_summary_finding_per_foreign_key() {
    let (dir, manifest) = generate(&Options {
        omit_data: vec!["APP.PROJECTS".into()],
        ..small()
    });
    assert!(!root(&dir).join("app/data/PROJECTS.sql").exists());
    let empty: Vec<&Finding> = manifest
        .findings
        .iter()
        .filter(|f| f.kind == FindingKind::ParentTableEmpty)
        .collect();
    assert_eq!(empty.len(), 1, "{empty:?}");
    let summary = empty.first().unwrap();
    assert_eq!(
        summary.constraint.as_deref(),
        Some("APP.ASSIGNMENTS_PROJECT_FK")
    );
    assert!(summary.count.unwrap() >= 2 * synth::MIN_ROWS);
    assert!(manifest.findings.iter().all(|f| f.table != "APP.PROJECTS"));
    assert!(manifest.findings.iter().all(|f| f.constraint.as_deref()
        != Some("APP.ASSIGNMENTS_PROJECT_FK")
        || f.kind == FindingKind::ParentTableEmpty));
}

#[test]
fn data_files_go_on_in_numbered_parts_each_ending_with_its_commit() {
    for layout in [Layout::Separate, Layout::Type] {
        let (dir, manifest) = generate(&Options {
            layout,
            rows_per_file: Some(10),
            omit_commit: vec!["APP.EMPLOYEES".into()],
            ..small()
        });
        let parts: Vec<&str> = manifest
            .files
            .iter()
            .map(|f| f.path.as_str())
            .filter(|p| p.contains("data") && p.contains('_'))
            .collect();
        assert!(!parts.is_empty(), "{layout:?}: {:?}", manifest.files);
        for file in &manifest.files {
            let text = fs::read_to_string(root(&dir).join(&file.path)).unwrap();
            let rows = text.lines().filter(|l| inserted_table(l).is_some()).count();
            assert!(rows <= 10, "{layout:?} {}: {rows} rows", file.path);
            // A part without its commit has rows the manifest says nothing commits after.
            let uncommitted = manifest.findings.iter().any(|f| {
                f.kind == FindingKind::Uncommitted && f.file.as_deref() == Some(file.path.as_str())
            });
            if rows > 0 && !file.path.starts_with("ref/") {
                assert!(
                    text.contains("COMMIT;") || uncommitted,
                    "{layout:?} {}",
                    file.path
                );
            }
        }
        // In its own files, each part of the uncommitted table has rows nothing commits after; in a shared file, the
        // next table's commit takes those of the last part.
        let uncommitted: Vec<&Finding> = manifest
            .findings
            .iter()
            .filter(|f| f.kind == FindingKind::Uncommitted && f.table == "APP.EMPLOYEES")
            .collect();
        let parts = if layout == Layout::Separate { 2 } else { 1 };
        assert_eq!(uncommitted.len(), parts, "{layout:?}: {uncommitted:?}");
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        synth::generate(
            &Options {
                layout: Layout::Single,
                rows_per_file: Some(25),
                ..small()
            },
            &dir.path().join("export")
        ),
        Err(synth::Error::SplitSingleFile)
    ));
}

/// The table an `INSERT INTO` line names, unquoted and upper-cased.
fn inserted_table(line: &str) -> Option<String> {
    let upper = line.to_uppercase();
    let (_, rest) = upper.split_once("INSERT INTO ")?;
    let name = rest.split([' ', '(']).next()?;
    Some(name.replace('"', ""))
}

/// A commit checks a deferred foreign key, so none may come between a department and the employees its manager key
/// points at, whatever tables the shuffled order puts between them.
#[test]
fn no_commit_comes_between_a_deferred_child_and_its_parent() {
    let mut between = 0;
    for seed in 1..=12 {
        let (dir, _) = generate(&Options {
            seed,
            layout: Layout::Single,
            constraints: Placement::BeforeData,
            shuffle_order: true,
            deferred_violations: false,
            ..small()
        });
        let sql = fs::read_to_string(root(&dir).join("app/export.sql")).unwrap();
        let lines: Vec<&str> = sql.lines().collect();
        let first = |table: &str| {
            lines
                .iter()
                .position(|l| inserted_table(l).as_deref() == Some(table))
        };
        let (Some(department), Some(employee)) = (first("APP.DEPARTMENTS"), first("APP.EMPLOYEES"))
        else {
            panic!("seed {seed}: no rows");
        };
        if department < employee {
            between += 1;
            let last_employee = lines
                .iter()
                .rposition(|l| inserted_table(l).as_deref() == Some("APP.EMPLOYEES"))
                .unwrap();
            assert!(
                !lines[department..last_employee]
                    .iter()
                    .any(|l| l.trim().eq_ignore_ascii_case("COMMIT;")),
                "seed {seed}: a COMMIT before the parents of the departments"
            );
        }
    }
    assert!(between > 0, "some seed loads the departments first");
}

#[test]
fn no_part_boundary_commits_before_the_parents_of_a_deferred_child() {
    for layout in [Layout::Separate, Layout::Type] {
        for rows_per_file in [1, 3] {
            let (dir, manifest) = generate(&Options {
                layout,
                rows_per_file: Some(rows_per_file),
                deferred_violations: false,
                ..small()
            });
            let lines: Vec<String> = manifest
                .files
                .iter()
                .filter(|f| f.path.starts_with("app/"))
                .flat_map(|f| {
                    let text = fs::read_to_string(root(&dir).join(&f.path)).unwrap();
                    text.lines().map(str::to_owned).collect::<Vec<_>>()
                })
                .collect();
            let table = |l: &String| inserted_table(l);
            let department = lines
                .iter()
                .position(|l| table(l).as_deref() == Some("APP.DEPARTMENTS"))
                .unwrap();
            let last_employee = lines
                .iter()
                .rposition(|l| table(l).as_deref() == Some("APP.EMPLOYEES"))
                .unwrap();
            assert!(
                !lines[department..last_employee]
                    .iter()
                    .any(|l| l.trim().eq_ignore_ascii_case("COMMIT;")),
                "{layout:?}, {rows_per_file} rows per file: a COMMIT before the parents of the departments"
            );
        }
    }
}

#[test]
fn the_deferred_foreign_key_can_be_left_without_violations() {
    let (_dir, manifest) = generate(&Options {
        deferred_violations: false,
        ..small()
    });
    assert!(
        manifest
            .findings
            .iter()
            .all(|f| f.constraint.as_deref() != Some("APP.DEPARTMENTS_MANAGER_FK"))
    );
}

#[test]
fn crlf_changes_only_the_structural_line_breaks() {
    let noise = Noise {
        multiline_strings: true,
        ..Noise::sqldev()
    };
    let (dir, manifest) = generate(&Options {
        line_ending: LineEnding::Crlf,
        noise,
        ..small()
    });
    for file in &manifest.files {
        let bytes = fs::read(root(&dir).join(&file.path)).unwrap();
        let starts_bare = usize::from(bytes.first() == Some(&b'\n'));
        let bare = starts_bare
            + bytes
                .windows(2)
                .filter(|w| matches!(w, [first, b'\n'] if *first != b'\r'))
                .count();
        let expected = if file.path.ends_with("data/LEGACY_NOTES.sql") {
            3
        } else {
            0
        };
        assert_eq!(bare, expected, "{}", file.path);
    }
}

#[test]
fn windows_1252_output_is_not_utf8_but_decodes() {
    let (dir, _) = generate(&Options {
        encoding: Encoding::Windows1252,
        ..small()
    });
    let bytes = fs::read(root(&dir).join("app/data/EMPLOYEES.sql")).unwrap();
    assert!(std::str::from_utf8(&bytes).is_err());
    let (text, _, unmappable) = encoding_rs::WINDOWS_1252.decode(&bytes);
    assert!(!unmappable);
    assert!(text.contains("Mäkinen"));
}

#[test]
fn layouts_write_the_files_sql_developer_would() {
    let paths = |layout| {
        generate(&Options { layout, ..small() })
            .1
            .files
            .into_iter()
            .map(|f| f.path)
            .collect::<Vec<_>>()
    };
    assert_eq!(paths(Layout::Single), ["ref/export.sql", "app/export.sql"]);
    assert_eq!(
        paths(Layout::Type),
        [
            "ref/tables.sql",
            "ref/data.sql",
            "app/tables.sql",
            "app/ref_constraints.sql",
            "app/data.sql"
        ]
    );
    let separate = paths(Layout::Separate);
    assert_eq!(
        separate.first().map(String::as_str),
        Some("ref/tables/COUNTRIES.sql")
    );
    assert!(separate.iter().any(|p| p == "app/data/AuditLog.sql"));
}

#[test]
fn load_script_and_order_list_follow_the_load_order() {
    let (dir, manifest) = generate(&small());
    let order = fs::read_to_string(root(&dir).join(synth::ORDER_FILE)).unwrap();
    let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(order.lines().collect::<Vec<_>>(), paths);
    let load = fs::read_to_string(root(&dir).join(synth::LOAD_SCRIPT)).unwrap();
    assert_eq!(load.lines().next(), Some("SET DEFINE OFF"));
    assert_eq!(
        load.lines()
            .skip(1)
            .map(|l| l.trim_start_matches("@@"))
            .collect::<Vec<_>>(),
        paths
    );
}

#[test]
fn output_into_a_non_empty_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("keep.txt"), "x").unwrap();
    assert!(matches!(
        synth::generate(&small(), dir.path()),
        Err(synth::Error::OutputNotEmpty(_))
    ));
    assert!(matches!(
        synth::generate(&Options { rows: 3, ..small() }, &dir.path().join("new")),
        Err(synth::Error::TooFewRows(3))
    ));
}

#[test]
fn cascades_hang_from_rows_with_null_in_a_not_null_column() {
    let (dir, manifest) = generate(&Options {
        cascades: true,
        manager_chain: 3,
        noise: Noise::all(),
        ..small()
    });
    let place = |file: &Option<String>, line: Option<u64>| (file.clone().unwrap(), line.unwrap());
    let roots: BTreeSet<(String, u64)> = manifest
        .findings
        .iter()
        .filter(|f| f.kind == FindingKind::NotNull)
        .map(|f| place(&f.file, f.line))
        .collect();
    let rows: BTreeSet<(String, u64)> = manifest
        .cascades
        .iter()
        .map(|c| (c.file.clone(), c.line))
        .collect();
    let depth = |mut at: (String, u64)| {
        let mut depth = 1;
        while let Some(c) = manifest
            .cascades
            .iter()
            .find(|c| (c.file.clone(), c.line) == at)
        {
            at = (c.parent.file.clone(), c.parent.line);
            depth += 1;
        }
        assert!(roots.contains(&at), "{at:?} is no NOT NULL row");
        depth
    };
    let depths: BTreeSet<usize> = rows.iter().cloned().map(depth).collect();
    // A department with no name, its employee, and the three employees of its manager chain.
    assert_eq!(depths.last(), Some(&5));
    assert!(
        manifest
            .findings
            .iter()
            .all(|f| !rows.contains(&place(&f.file, f.line))),
        "a cascade row has a finding of its own"
    );
    // Oracle rejects an `INSERT ALL` whole when one of its rows fails, so a cascade row is an insert of its own.
    let files = tree(&root(&dir));
    for (file, line) in &rows {
        let text = String::from_utf8_lossy(&files[file]).into_owned();
        let insert = text
            .lines()
            .nth(usize::try_from(*line - 1).unwrap())
            .unwrap();
        assert!(
            insert
                .trim_start()
                .to_uppercase()
                .starts_with("INSERT INTO"),
            "{file}:{line}: {insert}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        synth::generate(
            &Options {
                cascades: true,
                omit_data: vec!["APP.PROJECTS".to_owned()],
                ..small()
            },
            &dir.path().join("out")
        ),
        Err(synth::Error::CascadesWithoutData)
    ));
}

/// With cascades, some duplicates' first row is a bad row: one with a missing parent, one with NULL in a NOT NULL
/// column, and a cascade row. The key's next row is the one the fix keeps, under `--on-fk null` too unless that keeps
/// the first row; a third row stays a duplicate and goes.
#[test]
fn duplicates_of_bad_rows_are_kept_once_the_bad_row_goes() {
    let (_dir, manifest) = generate(&Options {
        cascades: true,
        ..small()
    });
    let place = |f: &Finding| Location {
        file: f.file.clone().unwrap(),
        line: f.line.unwrap(),
    };
    let reason = |at: &Location| {
        if manifest
            .cascades
            .iter()
            .any(|c| c.file == at.file && c.line == at.line)
        {
            return Some(FindingKind::FkMissingParent);
        }
        manifest
            .findings
            .iter()
            .find(|f| place(f) == *at && f.kind != FindingKind::DuplicateKey)
            .map(|f| f.kind)
    };
    let mut seconds = BTreeMap::new();
    let mut thirds = 0;
    for finding in manifest
        .findings
        .iter()
        .filter(|f| f.kind == FindingKind::DuplicateKey)
    {
        let first = finding.first.clone().unwrap();
        let Some(why) = reason(&first) else {
            continue;
        };
        let at = place(finding);
        let earlier = manifest.findings.iter().any(|f| {
            f.kind == FindingKind::DuplicateKey && f.first == finding.first && place(f) < at
        });
        if earlier {
            thirds += 1;
            assert!(manifest.fixes.removed.contains(&at), "{at:?}");
            assert!(manifest.fixes.removed_on_fk_null.contains(&at), "{at:?}");
            continue;
        }
        let first_stays = manifest
            .fixes
            .nulled_on_fk_null
            .iter()
            .any(|n| n.file == first.file && n.line == first.line);
        assert!(!manifest.fixes.removed.contains(&at), "{at:?}");
        assert_eq!(
            manifest.fixes.removed_on_fk_null.contains(&at),
            first_stays,
            "{at:?}"
        );
        let cascade = manifest
            .cascades
            .iter()
            .any(|c| c.file == first.file && c.line == first.line);
        *seconds.entry((why, cascade)).or_insert(0) += 1;
    }
    let trees = 2;
    assert_eq!(
        seconds,
        BTreeMap::from([
            ((FindingKind::NotNull, false), trees),
            ((FindingKind::FkMissingParent, false), trees),
            ((FindingKind::FkMissingParent, true), trees / 2),
        ])
    );
    assert_eq!(thirds, trees);
}

/// The committed fixtures (`tests/fixtures/gen/<dialect>/<variant>`) are what the generator writes today; `make
/// fixtures` refreshes them on purpose.
#[test]
fn committed_fixtures_regenerate_byte_for_byte() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gen");
    let mut checked = 0;
    let variants = fs::read_dir(&fixtures)
        .unwrap()
        .flat_map(|dialect| fs::read_dir(dialect.unwrap().path()).unwrap())
        .map(|variant| variant.unwrap().path());
    for committed in variants {
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(committed.join(synth::MANIFEST_FILE)).unwrap())
                .unwrap();
        let (dir, _) = generate(&manifest.options);
        assert_eq!(
            tree(&root(&dir)),
            tree(&committed),
            "{} differs; run `make fixtures`",
            committed.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 3,
        "fixtures missing under {}",
        fixtures.display()
    );
}
