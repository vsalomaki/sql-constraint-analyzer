//! Ground truth from a real Oracle: loads generated exports into Oracle Free and checks that Oracle rejects exactly
//! the rows and constraints the generator's manifest expects, then loads what `sqlca fix` writes of them and checks
//! that Oracle rejects only what the fix's check of its output still finds. Run it with `make oracle-check`, which
//! starts the container and sets `SQLCA_ORACLE_CONTAINER`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use super::RULES;
use crate::check;
use crate::finding::{Finding, FindingKind, Location};
use crate::fix::{self, FixReport, OnDuplicate, OnFk, Policy, Removal, ReportPlace, Target};
use crate::input::{InputFile, OrderSource, Role};
use crate::synth::schema::{APP_TABLES, Kind, REF_TABLES, constraints_of};
use crate::synth::{
    self, Cascade, Encoding, Fixes, Layout, LineEnding, Manifest, Noise, Nulled, Options, Placement,
};

const PDB: &str = "//localhost/FREEPDB1";

/// What Oracle reports about one rejected row or statement, reduced to what a manifest finding also says.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Signature {
    location: Option<(String, u64)>,
    what: &'static str,
    detail: String,
}

struct Oracle {
    container: String,
    password: String,
}

impl Oracle {
    fn from_env() -> Self {
        Self {
            container: std::env::var("SQLCA_ORACLE_CONTAINER").expect(
                "SQLCA_ORACLE_CONTAINER names the Oracle Free container; run `make oracle-check`",
            ),
            password: std::env::var("SQLCA_ORACLE_PASSWORD")
                .unwrap_or_else(|_| "sqlca-local".into()),
        }
    }

    fn docker(args: &[&str]) -> Vec<u8> {
        let out = Command::new("docker")
            .args(args)
            .output()
            .expect("docker runs");
        assert!(
            out.status.success(),
            "docker {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Loads `export` in its load order as SYSTEM, continuing past errors, and returns the SQL*Plus spool.
    fn load(&self, export: &Path, encoding: Encoding, run: &str) -> String {
        let order = fs::read_to_string(export.join(synth::ORDER_FILE)).unwrap();
        let files: Vec<&str> = order
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let include = |f: &&str| format!("@@{f}");
        let mut driver: Vec<String> = [
            "WHENEVER SQLERROR CONTINUE",
            "SET DEFINE OFF",
            "SET LINESIZE 32767",
            "SET FEEDBACK ON",
            "ALTER SESSION SET NLS_DATE_LANGUAGE=AMERICAN;",
            "ALTER SESSION SET NLS_NUMERIC_CHARACTERS='.,';",
        ]
        .map(String::from)
        .into();
        driver.push(format!("SPOOL /tmp/sqlca-{run}.log"));
        driver.extend(files.iter().filter(|f| f.starts_with("ref/")).map(include));
        // A foreign key into another schema needs the REFERENCES privilege on the parent.
        driver.push("GRANT REFERENCES ON REF.COUNTRIES TO APP;".into());
        driver.extend(files.iter().filter(|f| !f.starts_with("ref/")).map(include));
        driver.extend(["COMMIT;", "SET HEADING OFF", "SET FEEDBACK OFF"].map(String::from));
        driver.push(format!("SPOOL /tmp/sqlca-{run}-rows.log"));
        let tables: Vec<String> = REF_TABLES
            .iter()
            .chain(APP_TABLES.iter())
            .map(|t| format!("SELECT 1 FROM {}", t.qualified(&RULES)))
            .collect();
        driver.push(format!(
            "SELECT COUNT(*) FROM ({});",
            tables.join(" UNION ALL ")
        ));
        driver.extend(["SPOOL OFF", "EXIT", ""].map(String::from));
        fs::write(export.join("driver.sql"), driver.join("\n")).unwrap();
        fs::write(
            export.join("users.sql"),
            "WHENEVER SQLERROR CONTINUE\nDROP USER APP CASCADE;\nDROP USER REF CASCADE;\n\
             WHENEVER SQLERROR EXIT FAILURE\n\
             CREATE USER APP IDENTIFIED BY app QUOTA UNLIMITED ON USERS;\n\
             CREATE USER REF IDENTIFIED BY ref QUOTA UNLIMITED ON USERS;\nEXIT\n",
        )
        .unwrap();

        let dir = format!("/tmp/sqlca-{run}");
        Self::docker(&["exec", "-u", "root", &self.container, "rm", "-rf", &dir]);
        Self::docker(&[
            "cp",
            &export.to_string_lossy(),
            &format!("{}:{dir}", self.container),
        ]);
        let charset = match encoding {
            Encoding::Utf8 => "AL32UTF8",
            Encoding::Windows1252 => "WE8MSWIN1252",
        };
        // No shell in between, so the password reaches SQL*Plus as it is.
        let login = format!("system/\"{}\"@{PDB}", self.password);
        let nls = format!("NLS_LANG=AMERICAN_AMERICA.{charset}");
        for script in ["@users.sql", "@driver.sql"] {
            Self::docker(&[
                "exec",
                "-w",
                &dir,
                "-e",
                &nls,
                &self.container,
                "sqlplus",
                "-s",
                "-L",
                &login,
                script,
            ]);
        }
        let spool = Self::docker(&[
            "exec",
            "-u",
            "root",
            &self.container,
            "cat",
            &format!("/tmp/sqlca-{run}.log"),
        ]);
        match encoding {
            Encoding::Utf8 => String::from_utf8(spool).unwrap(),
            Encoding::Windows1252 => encoding_rs::WINDOWS_1252.decode(&spool).0.into_owned(),
        }
    }

    /// The rows the schema's tables hold after the `load` named `run`.
    fn rows(&self, run: &str) -> u64 {
        let spool = Self::docker(&[
            "exec",
            "-u",
            "root",
            &self.container,
            "cat",
            &format!("/tmp/sqlca-{run}-rows.log"),
        ]);
        let text = String::from_utf8(spool).unwrap();
        text.trim()
            .parse()
            .unwrap_or_else(|_| panic!("{run}: no row count in {text:?}"))
    }
}

/// One `ORA-`/`SP2-` error in a spool: its code, message, and the statement line SQL*Plus printed for it.
#[derive(Debug)]
struct Error {
    code: String,
    message: String,
    echo: Option<String>,
}

fn errors(spool: &str) -> Vec<Error> {
    let lines: Vec<&str> = spool.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // `ORA-` codes have five digits, `SP2-` codes four.
        let Some((code, message)) = line.split_once(':') else {
            continue;
        };
        let (digits, width) = match (code.strip_prefix("ORA-"), code.strip_prefix("SP2-")) {
            (Some(digits), _) => (digits, 5),
            (_, Some(digits)) => (digits, 4),
            _ => continue,
        };
        if digits.len() != width || !digits.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // ORA-03301 details the ORA-00001 before it; it is not an error of its own.
        if code == "ORA-03301" {
            continue;
        }
        // SQL*Plus prints the failing line, a `*` under the error, then `ERROR at line N:`.
        let echo =
            (i >= 3 && lines[i - 1].starts_with("ERROR at line") && lines[i - 2].trim() == "*")
                .then(|| lines[i - 3].to_owned());
        found.push(Error {
            code: code.to_owned(),
            message: message.trim().to_owned(),
            echo,
        });
    }
    found
}

/// The text between the first `open` and the next `close`.
fn between(text: &str, open: char, close: char) -> Option<&str> {
    let (_, rest) = text.split_once(open)?;
    rest.split_once(close).map(|(inside, _)| inside)
}

fn unquote(name: &str) -> String {
    name.replace('"', "")
}

/// Every line of the export's SQL files, by text, to find the line SQL*Plus printed.
fn line_index(export: &Path, manifest: &Manifest) -> BTreeMap<String, Vec<(String, u64)>> {
    let mut index: BTreeMap<String, Vec<(String, u64)>> = BTreeMap::new();
    for file in &manifest.files {
        let bytes = fs::read(export.join(&file.path)).unwrap();
        let text = match manifest.options.encoding {
            Encoding::Utf8 => String::from_utf8(bytes).unwrap(),
            Encoding::Windows1252 => encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned(),
        };
        for (n, line) in (1..).zip(text.lines()) {
            index
                .entry(line.trim_end().to_owned())
                .or_default()
                .push((file.path.clone(), n));
        }
    }
    index
}

fn locate(index: &BTreeMap<String, Vec<(String, u64)>>, echo: &str) -> (String, u64) {
    let statement = format!("{};", echo.trim_end());
    match index.get(&statement).map(Vec::as_slice) {
        Some([only]) => only.clone(),
        other => panic!("the line SQL*Plus printed matches {other:?} lines, not one: {statement}"),
    }
}

/// A rejected row as Oracle reports it, or `None` for an error that isn't about one row.
fn row_signature(error: &Error) -> Option<(&'static str, String)> {
    let detail = |text: &str| text.to_owned();
    Some(match error.code.as_str() {
        "ORA-02291" => ("fk", detail(between(&error.message, '(', ')')?)),
        "ORA-00001" => ("duplicate", detail(between(&error.message, '(', ')')?)),
        "ORA-01400" => ("not null", unquote(between(&error.message, '(', ')')?)),
        "ORA-00904" => (
            "unknown column",
            unquote(between(&error.message, '"', '"')?),
        ),
        "ORA-00942" => (
            "unknown table",
            unquote(error.message.split_whitespace().nth(3)?),
        ),
        "ORA-00947" | "ORA-00913" => ("value count", String::new()),
        _ => return None,
    })
}

/// What Oracle reports for a finding's row; nothing for a missing commit, since the load commits at its end.
fn expected_row(finding: &Finding) -> Option<(&'static str, String)> {
    let constraint = || finding.constraint.clone().unwrap_or_default();
    let column = || finding.columns.first().cloned().unwrap_or_default();
    Some(match finding.kind {
        FindingKind::FkMissingParent | FindingKind::FkLoadOrder => ("fk", constraint()),
        FindingKind::DuplicateKey => ("duplicate", constraint()),
        FindingKind::NotNull => (
            "not null",
            format!("{}.{}", unquote(&finding.table), column()),
        ),
        FindingKind::UnknownColumn => ("unknown column", column()),
        FindingKind::UnknownTable | FindingKind::AmbiguousTable => {
            ("unknown table", unquote(&finding.table))
        }
        FindingKind::ValueCountMismatch => ("value count", String::new()),
        FindingKind::ParentTableEmpty => ("parent table empty", constraint()),
        FindingKind::Uncommitted => return None,
    })
}

fn counts(signatures: impl IntoIterator<Item = Signature>) -> BTreeMap<Signature, usize> {
    let mut counts = BTreeMap::new();
    for signature in signatures {
        *counts.entry(signature).or_default() += 1;
    }
    counts
}

fn generate(options: &Options) -> (tempfile::TempDir, std::path::PathBuf, Manifest) {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export");
    let manifest = synth::generate(options, &export).unwrap();
    (dir, export, manifest)
}

/// Constraints in before the data: Oracle rejects each bad row at its insert, so the rows with manifest findings
/// must be the rejected rows, on the same lines. With `exact` off (wrapped statements, multi-line strings, where
/// SQL*Plus prints a line other than the first), only the kinds and counts are compared.
fn check_load_order(oracle: &Oracle, options: &Options, run: &str, exact: bool) {
    let (_dir, export, manifest) = generate(options);
    let spool = oracle.load(&export, options.encoding, run);
    let observed = observed_load_order(&spool, &line_index(&export, &manifest), run, exact);
    let files = load_order(&manifest);
    let expected = expected_load_order(&manifest.findings, &manifest.cascades, &files, exact);
    assert_rejections(
        &observed,
        &expected,
        &format!("{run}: Oracle's rejections (left) differ from the manifest (right)"),
    );
}

/// A rejected row's place, and the reasons for it.
type Rows<'a> = BTreeMap<&'a Option<(String, u64)>, Vec<(&'static str, &'a str)>>;

/// Each row's reasons, once per count.
fn by_row(signatures: &BTreeMap<Signature, usize>) -> Rows<'_> {
    let mut rows: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (signature, &count) in signatures {
        let reasons = rows.entry(&signature.location).or_default();
        reasons.extend((0..count).map(|_| (signature.what, signature.detail.as_str())));
    }
    rows
}

/// Oracle rejects a row once, for the first constraint it fails, while a row can have several findings: a child
/// before its parent that is also a duplicate, say. With places, each rejected row must be a row with findings, and
/// Oracle's reason one of them; every row with findings must be rejected. Without places, the counts must match.
fn assert_rejections(
    observed: &BTreeMap<Signature, usize>,
    expected: &BTreeMap<Signature, usize>,
    message: &str,
) {
    if observed
        .keys()
        .chain(expected.keys())
        .any(|s| s.location.is_none())
    {
        assert_eq!(observed, expected, "{message}");
        return;
    }
    let (observed, expected) = (by_row(observed), by_row(expected));
    let mismatched: Vec<_> = observed
        .keys()
        .chain(expected.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|row| match (observed.get(*row), expected.get(*row)) {
            (Some(rejected), Some(found)) => {
                rejected.len() != 1 || !rejected.iter().all(|reason| found.contains(reason))
            }
            _ => true,
        })
        .map(|row| (row, observed.get(row), expected.get(row)))
        .collect();
    assert!(mismatched.is_empty(), "{message}: {mismatched:#?}");
}

fn observed_load_order(
    spool: &str,
    index: &BTreeMap<String, Vec<(String, u64)>>,
    run: &str,
    exact: bool,
) -> BTreeMap<Signature, usize> {
    counts(errors(spool).into_iter().map(|error| {
        let (what, detail) =
            row_signature(&error).unwrap_or_else(|| panic!("{run}: unexpected {error:?}"));
        let location = exact.then(|| locate(index, error.echo.as_deref().unwrap_or_default()));
        Signature {
            location,
            what,
            detail,
        }
    }))
}

fn load_order(manifest: &Manifest) -> Vec<String> {
    manifest.files.iter().map(|f| f.path.clone()).collect()
}

fn location(finding: &Finding) -> Option<Location> {
    Some(Location {
        file: finding.file.clone()?,
        line: finding.line?,
    })
}

/// The rows of each duplicated key, its first row among them, by constraint and key value.
fn holders(findings: &[Finding]) -> BTreeMap<(String, Vec<Option<String>>), BTreeSet<Location>> {
    let mut keys: BTreeMap<_, BTreeSet<Location>> = BTreeMap::new();
    for finding in findings
        .iter()
        .filter(|f| f.kind == FindingKind::DuplicateKey)
    {
        let (Some(constraint), Some(row), Some(first)) = (
            finding.constraint.clone(),
            location(finding),
            finding.first.clone(),
        ) else {
            continue;
        };
        keys.entry((constraint, finding.key.clone()))
            .or_default()
            .extend([first, row]);
    }
    keys
}

/// The duplicates Oracle accepts, by constraint and row: a row it rejects at its insert for another reason, or as a
/// cascade row, never holds its key, so the key's first row in load order that Oracle accepts holds it instead. It
/// knows only the rows findings name, so a bad first row must not share its key with rows that precede a key enabled
/// between rows, which hold the key with no finding.
fn accepted_duplicates(
    findings: &[Finding],
    cascades: &[Cascade],
    files: &[String],
) -> BTreeSet<(String, Location)> {
    let rejected: BTreeSet<Location> = findings
        .iter()
        .filter(|f| f.kind != FindingKind::DuplicateKey && expected_row(f).is_some())
        .filter_map(location)
        .chain(cascades.iter().map(|c| Location {
            file: c.file.clone(),
            line: c.line,
        }))
        .collect();
    let load_order = |row: &Location| (files.iter().position(|f| *f == row.file), row.line);
    holders(findings)
        .into_iter()
        .filter_map(|((constraint, _), rows)| {
            let mut rows: Vec<Location> = rows.into_iter().collect();
            rows.sort_by_key(load_order);
            let holder = rows.into_iter().find(|row| !rejected.contains(row))?;
            Some((constraint, holder))
        })
        .collect()
}

/// The findings' rows, and the cascade rows, rejected for their foreign key once their parent is; but not a duplicate
/// Oracle accepts. `files` are in load order.
fn expected_load_order(
    findings: &[Finding],
    cascades: &[Cascade],
    files: &[String],
    exact: bool,
) -> BTreeMap<Signature, usize> {
    let accepted = accepted_duplicates(findings, cascades, files);
    let mut rows: Vec<Signature> = findings
        .iter()
        .filter(|f| {
            let holds = f.constraint.clone().zip(location(f));
            f.kind != FindingKind::DuplicateKey || holds.is_none_or(|key| !accepted.contains(&key))
        })
        .filter_map(|f| {
            let (what, detail) = expected_row(f)?;
            let location = (
                f.file.clone().unwrap_or_default(),
                f.line.unwrap_or_default(),
            );
            Some(Signature {
                location: Some(location),
                what,
                detail,
            })
        })
        .collect();
    rows.extend(cascades.iter().map(|cascade| Signature {
        location: Some((cascade.file.clone(), cascade.line)),
        what: "fk",
        detail: cascade.constraint.clone(),
    }));
    if !exact {
        for row in &mut rows {
            row.location = None;
        }
    }
    counts(rows)
}

/// What an after-data load ends with: the rows rejected at insert, the keys that failed to validate, the foreign
/// keys that failed, and the parent tables of the foreign keys that couldn't be added because their key failed.
#[derive(Debug, Default, PartialEq, Eq)]
struct AfterData {
    rows: BTreeMap<Signature, usize>,
    keys: Vec<String>,
    fks: Vec<String>,
    blocked: Vec<String>,
}

impl AfterData {
    fn sorted(mut self) -> Self {
        for list in [&mut self.keys, &mut self.fks, &mut self.blocked] {
            list.sort();
        }
        self.keys.dedup();
        self.fks.dedup();
        self
    }
}

fn observed_after_data(
    spool: &str,
    index: &BTreeMap<String, Vec<(String, u64)>>,
    run: &str,
) -> AfterData {
    let mut rows = Vec::new();
    let mut seen = AfterData::default();
    for error in errors(spool) {
        let echo = error.echo.clone().unwrap_or_default();
        match error.code.as_str() {
            "ORA-02437" | "ORA-02299" => seen
                .keys
                .push(between(&error.message, '(', ')').unwrap().to_owned()),
            // A NULL in a key column: `ALTER TABLE "APP"."T" ADD CONSTRAINT "NAME" PRIMARY KEY …`.
            "ORA-01449" => {
                let schema = between(&echo, '"', '"').unwrap();
                let (_, after) = echo.split_once("CONSTRAINT").unwrap();
                seen.keys
                    .push(format!("{schema}.{}", between(after, '"', '"').unwrap()));
            }
            "ORA-01452" => seen
                .keys
                .push(unquote(echo.split_whitespace().nth(3).unwrap())),
            "ORA-02298" => seen
                .fks
                .push(between(&error.message, '(', ')').unwrap().to_owned()),
            "ORA-02270" | "ORA-02268" => {
                let (_, parent) = echo.split_once("REFERENCES").unwrap();
                seen.blocked
                    .push(unquote(parent.split_whitespace().next().unwrap()));
            }
            _ => {
                let (what, detail) =
                    row_signature(&error).unwrap_or_else(|| panic!("{run}: unexpected {error:?}"));
                rows.push(Signature {
                    location: Some(locate(index, &echo)),
                    what,
                    detail,
                });
            }
        }
    }
    AfterData {
        rows: counts(rows),
        ..seen
    }
    .sorted()
}

/// Oracle rejects at insert a row with NULL in a NOT NULL column or a structure error; every other row loads, since
/// no key is in place yet. A key then fails when two of a key value's rows loaded, and a foreign key when a row's
/// parent is missing, a cascade row's included when its parent was rejected at insert.
fn expected_after_data(findings: &[Finding], cascades: &[Cascade]) -> AfterData {
    let mut rows = Vec::new();
    let mut rejected = BTreeSet::new();
    let mut want = AfterData::default();
    for finding in findings {
        match (finding.kind, &finding.constraint) {
            (FindingKind::NotNull, Some(c)) => want.keys.push(c.clone()),
            (FindingKind::DuplicateKey, Some(_)) => {}
            (FindingKind::FkMissingParent, Some(c)) => want.fks.push(c.clone()),
            _ => {
                let Some((what, detail)) = expected_row(finding) else {
                    continue;
                };
                rejected.extend(location(finding));
                let location = Some((
                    finding.file.clone().unwrap_or_default(),
                    finding.line.unwrap_or_default(),
                ));
                rows.push(Signature {
                    location,
                    what,
                    detail,
                });
            }
        }
    }
    for ((constraint, _), rows) in holders(findings) {
        if rows.iter().filter(|row| !rejected.contains(row)).count() > 1 {
            want.keys.push(constraint);
        }
    }
    want.fks.extend(
        cascades
            .iter()
            .filter(|c| rejected.contains(&c.parent))
            .map(|c| c.constraint.clone()),
    );
    for fk in REF_TABLES
        .iter()
        .chain(APP_TABLES.iter())
        .flat_map(|t| constraints_of(*t))
    {
        if let Kind::Foreign { parent, .. } = fk.kind
            && blocked_fk(&fk.qualified(&RULES), &want.keys)
        {
            want.blocked.push(unquote(&parent.qualified(&RULES)));
        }
    }
    // Only a foreign key that could be added at all can report its missing parents.
    let keys = want.keys.clone();
    want.fks.retain(|fk| !blocked_fk(fk, &keys));
    AfterData {
        rows: counts(rows),
        ..want
    }
    .sorted()
}

/// Constraints after the data: the rows load except those with a NOT NULL or structure error, then each key and
/// foreign key either validates or fails. A foreign key whose parent key failed can't be added at all.
fn check_after_data(oracle: &Oracle, options: &Options, run: &str) {
    let (_dir, export, manifest) = generate(options);
    let spool = oracle.load(&export, options.encoding, run);
    let observed = observed_after_data(&spool, &line_index(&export, &manifest), run);
    assert_eq!(
        observed,
        expected_after_data(&manifest.findings, &manifest.cascades),
        "{run}: Oracle (left) differs from the manifest (right)"
    );
}

fn blocked_fk(name: &str, failed_keys: &[String]) -> bool {
    REF_TABLES
        .iter()
        .chain(APP_TABLES.iter())
        .flat_map(|t| constraints_of(*t))
        .any(|fk| {
            let Kind::Foreign {
                parent,
                parent_columns,
                ..
            } = fk.kind
            else {
                return false;
            };
            fk.qualified(&RULES) == name
                && constraints_of(parent).any(|c| {
                    let matches = match parent_columns {
                        None => c.kind == Kind::Primary,
                        Some(columns) => !c.is_foreign() && c.columns == columns,
                    };
                    matches && failed_keys.contains(&c.qualified(&RULES))
                })
        })
}

/// `sqlca fix` of `export` into `fixed`, with its order file, and the findings its check of the output still has, by
/// the export's relative paths as the manifest names files.
fn fix_export(
    export: &Path,
    fixed: &Path,
    check: &check::Options,
    policy: Policy,
    annotate: bool,
) -> (FixReport, Vec<Finding>) {
    let order = fs::read_to_string(export.join(synth::ORDER_FILE)).unwrap();
    let relative: Vec<&str> = order.lines().filter(|l| !l.trim().is_empty()).collect();
    let files: Vec<InputFile> = relative
        .iter()
        .map(|path| InputFile {
            path: export.join(path),
            role: if path.starts_with("ref/") {
                Role::Reference
            } else {
                Role::Data
            },
            bytes: fs::metadata(export.join(path)).unwrap().len(),
            order: OrderSource::Given,
        })
        .collect();
    let target = Target {
        report: ReportPlace::Off,
        annotate,
        ..Target::out(fixed)
    };
    let report = fix::fix(&files, check, policy, &target).unwrap();
    fs::copy(
        export.join(synth::ORDER_FILE),
        fixed.join(synth::ORDER_FILE),
    )
    .unwrap();
    let names: BTreeMap<String, &str> = report
        .files
        .iter()
        .zip(&relative)
        .map(|(output, path)| {
            assert_eq!(output.path, fixed.join(path), "outputs keep their paths");
            (check::display(&output.path), *path)
        })
        .collect();
    let left = report
        .output
        .findings
        .iter()
        .map(|f| Finding {
            file: f.file.as_ref().map(|name| names[name].to_owned()),
            ..f.clone()
        })
        .collect();
    (report, left)
}

/// Every input has an output, and each output with the removed bytes put back in place of what it has there instead
/// is its input exactly: the fix changed nothing but the rows it reports, and a row it set to NULL reads as
/// `nulled_row` builds it. `inputs` are the bytes read before the fix, since an unchanged output is a hard link to its
/// input.
fn assert_restores(
    inputs: &[(String, Vec<u8>)],
    fixed: &Path,
    report: &FixReport,
    annotate: bool,
    run: &str,
) {
    assert_eq!(
        report.files.len(),
        inputs.len(),
        "{run}: every input has an output"
    );
    for (output, (path, input)) in report.files.iter().zip(inputs) {
        let written = fs::read(fixed.join(path))
            .unwrap_or_else(|e| panic!("{run}: {path} has no output: {e}"));
        let removals = report.removed.iter().filter(|r| r.file == output.input);
        for removal in removals.clone().filter(|r| !r.nulled.is_empty()) {
            assert!(
                removal.replacement == nulled_row(removal, annotate),
                "{run}: {path} has the row at {} other than with NULL in {:?}",
                removal.span.start,
                removal.nulled
            );
        }
        // Each change as (span, what the output has, what the input had); an added commit replaces nothing.
        let commits = report.committed.iter().filter(|c| c.file == output.input);
        let mut changes: Vec<(u64, u64, &[u8], &[u8])> = removals
            .map(|r| (r.span.start, r.span.end, &r.replacement[..], &r.text[..]))
            .chain(commits.map(|c| (c.at, c.at, &c.text[..], &b""[..])))
            .collect();
        changes.sort_by_key(|c| c.0);
        let (mut rebuilt, mut at_input, mut at_output) = (Vec::new(), 0_usize, 0_usize);
        // `get`, not indexing: an output cut short fails the comparison below, not a slice.
        for (start, end, replacement, text) in changes {
            let start = usize::try_from(start).unwrap();
            let kept = start - at_input;
            rebuilt.extend_from_slice(written.get(at_output..at_output + kept).unwrap_or_default());
            at_output += kept;
            let replaced = written.get(at_output..at_output + replacement.len());
            assert_eq!(
                replaced,
                Some(replacement),
                "{run}: {path} has no replacement at {start}"
            );
            rebuilt.extend_from_slice(text);
            at_output += replacement.len();
            at_input = usize::try_from(end).unwrap();
        }
        rebuilt.extend_from_slice(written.get(at_output..).unwrap_or_default());
        assert!(
            rebuilt == *input,
            "{run}: {path} with its removed rows put back isn't its input"
        );
    }
}

/// Under keep-first the fix removes exactly the rows the manifest lists for its foreign key policy, and sets to NULL
/// exactly the columns it lists: a row more is a valid row lost, and NULL in the wrong column leaves a key broken.
fn assert_fixes(
    report: &FixReport,
    inputs: &[(String, Vec<u8>)],
    fixes: &Fixes,
    on_fk: OnFk,
    run: &str,
) {
    let relative: BTreeMap<&str, &str> = report
        .files
        .iter()
        .zip(inputs)
        .map(|(output, (path, _))| (output.input.as_str(), path.as_str()))
        .collect();
    let file = |removal: &Removal| relative[removal.file.as_str()].to_owned();
    let removed: BTreeSet<Location> = report
        .removed
        .iter()
        .filter(|r| r.nulled.is_empty())
        .map(|r| Location {
            file: file(r),
            line: r.line,
        })
        .collect();
    let nulled: BTreeSet<Nulled> = report
        .removed
        .iter()
        .filter(|r| !r.nulled.is_empty())
        .map(|r| {
            let mut columns: Vec<String> = r.nulled.iter().map(|(c, _)| c.clone()).collect();
            columns.sort();
            Nulled {
                file: file(r),
                line: r.line,
                columns,
            }
        })
        .collect();
    let (want_removed, want_nulled) = match on_fk {
        OnFk::Null => (&fixes.removed_on_fk_null, &fixes.nulled_on_fk_null[..]),
        _ => (&fixes.removed, &[][..]),
    };
    assert_eq!(
        removed,
        want_removed.iter().cloned().collect(),
        "{run}: the fix removed other rows (left) than the manifest lists (right)"
    );
    assert_eq!(
        nulled,
        want_nulled.iter().cloned().collect(),
        "{run}: the fix set other columns to NULL (left) than the manifest lists (right)"
    );
}

/// A row set to NULL as the output should have it, from its original text alone: NULL at each value's place, after it
/// with `annotate` the comment saying so, and every other byte as it was.
fn nulled_row(removal: &Removal, annotate: bool) -> Vec<u8> {
    let offset = |at: u64| usize::try_from(at - removal.span.start).unwrap();
    let (mut row, mut at) = (Vec::new(), 0);
    for (column, span) in &removal.nulled {
        row.extend_from_slice(removal.text.get(at..offset(span.start)).unwrap_or_default());
        row.extend_from_slice(b"NULL");
        if annotate {
            row.extend_from_slice(
                format!(" /* sqlca fix set {column} to NULL; see rejects.sql */").as_bytes(),
            );
        }
        at = offset(span.end);
    }
    row.extend_from_slice(removal.text.get(at..).unwrap_or_default());
    row
}

/// The rejections of a kind the fix removes, beyond the rows the fix's check of the output finds before their
/// parents, which the fix leaves.
fn acted_on(
    observed: &BTreeMap<Signature, usize>,
    load_order: &BTreeMap<Signature, usize>,
) -> Vec<(Signature, usize)> {
    observed
        .iter()
        .filter(|(s, _)| ["fk", "duplicate", "not null"].contains(&s.what))
        .filter_map(|(s, &n)| {
            let extra = n.saturating_sub(load_order.get(s).copied().unwrap_or_default());
            (extra > 0).then(|| (s.clone(), extra))
        })
        .collect()
}

/// `sqlca fix` of an export, loaded the way the export is: Oracle rejects no row for a kind the fix acts on, rejects
/// exactly what the fix's check of its output still finds, and keeps every other row the output holds. Before the
/// data, constraints judge rows as they come, so the fix runs in load-order mode.
fn check_fixed(
    oracle: &Oracle,
    options: &Options,
    run: &str,
    exact: bool,
    policy: Policy,
    annotate: bool,
) {
    let (dir, export, manifest) = generate(options);
    let fixed = fs::canonicalize(dir.path()).unwrap().join("fixed");
    let after_data = options.constraints == Placement::AfterData;
    let check = check::Options {
        encoding: options.encoding,
        load_order: !after_data,
        ..check::Options::default()
    };
    let inputs: Vec<(String, Vec<u8>)> = manifest
        .files
        .iter()
        .map(|f| (f.path.clone(), fs::read(export.join(&f.path)).unwrap()))
        .collect();
    let (report, left) = fix_export(&export, &fixed, &check, policy, annotate);
    assert!(!report.removed.is_empty(), "{run}: the fix removed nothing");
    assert!(
        policy.on_fk != OnFk::Null || report.removed.iter().any(|r| !r.nulled.is_empty()),
        "{run}: the fix set nothing to NULL"
    );
    if policy.on_duplicate == OnDuplicate::KeepFirst {
        assert_fixes(&report, &inputs, &manifest.fixes, policy.on_fk, run);
    }
    assert_restores(&inputs, &fixed, &report, annotate, run);
    let spool = oracle.load(&fixed, options.encoding, run);
    let index = line_index(&fixed, &manifest);
    let rejected = if after_data {
        let observed = observed_after_data(&spool, &index, run);
        let failed = [&observed.keys, &observed.fks, &observed.blocked];
        assert!(
            failed.iter().all(|list| list.is_empty()),
            "{run}: Oracle fails constraints on the fixed output: {failed:?}"
        );
        let acted = acted_on(&observed.rows, &BTreeMap::new());
        assert!(
            acted.is_empty(),
            "{run}: Oracle rejects fixed rows {acted:?}"
        );
        assert_eq!(
            observed,
            expected_after_data(&left, &[]),
            "{run}: Oracle (left) differs from the fix's check of its output (right)"
        );
        observed.rows.values().sum::<usize>()
    } else {
        let observed = observed_load_order(&spool, &index, run, exact);
        let files = load_order(&manifest);
        let before_parents: Vec<Finding> = left
            .iter()
            .filter(|f| f.kind == FindingKind::FkLoadOrder)
            .cloned()
            .collect();
        let acted = acted_on(
            &observed,
            &expected_load_order(&before_parents, &[], &files, exact),
        );
        assert!(
            acted.is_empty(),
            "{run}: Oracle rejects fixed rows {acted:?}"
        );
        assert_rejections(
            &observed,
            &expected_load_order(&left, &[], &files, exact),
            &format!(
                "{run}: Oracle's rejections (left) differ from the fix's check of its output (right)"
            ),
        );
        observed.values().sum::<usize>()
    };
    assert_eq!(
        oracle.rows(run),
        report.output.rows - u64::try_from(rejected).unwrap(),
        "{run}: Oracle holds other than the output's rows less those it rejected"
    );
}

fn every_quirk_but_wrapping() -> Noise {
    Noise {
        wrapped_statements: false,
        multiline_strings: false,
        ..Noise::all()
    }
}

#[test]
#[ignore = "needs Oracle Free in Docker: run `make oracle-check`"]
fn oracle_agrees_with_the_manifest_and_the_fix() {
    let oracle = Oracle::from_env();
    // A deferred violation would roll back the whole load at COMMIT, with no row to match.
    let base = Options {
        rows: 200,
        violations_ppm: 20_000,
        deferred_violations: false,
        ..Options::default()
    };
    // Oracle doesn't see a missing commit, since the load commits at its end; the fix adds one, which it loads.
    let quirks = Options {
        seed: 2,
        noise: every_quirk_but_wrapping(),
        line_ending: LineEnding::Crlf,
        encoding: Encoding::Windows1252,
        omit_commit: vec!["APP.TIMESHEETS".to_owned()],
        // Parts end inside the employees, before every manager the departments' deferred key points at is written.
        rows_per_file: Some(40),
        cascades: true,
        manager_chain: 3,
        ..base.clone()
    };
    let wrapped = Options {
        seed: 3,
        noise: Noise::all(),
        ..base.clone()
    };
    let after = Options {
        seed: 4,
        constraints: Placement::AfterData,
        deferred_violations: true,
        layout: Layout::Type,
        rows_per_file: Some(150),
        cascades: true,
        manager_chain: 3,
        ..base.clone()
    };
    // The departments come before their managers with other tables between, and children before their parents: no
    // COMMIT may come before the managers, and a bad row or a cascade row can be a child before its parent too.
    let shuffled = Options {
        seed: 6,
        layout: Layout::Single,
        shuffle_order: true,
        cascades: true,
        manager_chain: 3,
        ..base.clone()
    };
    // With `exact` off, only kinds and counts are compared.
    let exports = [
        (base, "sqldev", true),
        (quirks, "quirks", true),
        (wrapped, "wrapped", false),
        (after, "after-data", true),
        (shuffled, "shuffled", true),
    ];
    for (options, run, exact) in &exports {
        match options.constraints {
            Placement::BeforeData => check_load_order(&oracle, options, run, *exact),
            Placement::AfterData => check_after_data(&oracle, options, run),
        }
    }
    let keep_last = Policy {
        on_duplicate: OnDuplicate::KeepLast,
        ..Policy::default()
    };
    let null_fk = Policy {
        on_fk: OnFk::Null,
        ..Policy::default()
    };
    let fixes = [
        (Policy::default(), false, "fix"),
        (Policy::default(), true, "fix-annotate"),
        (keep_last, true, "fix-keep-last-annotate"),
        (null_fk, true, "fix-null-annotate"),
    ];
    for (options, run, exact) in &exports {
        // The fix removes the deferred violation, so the load commits.
        let options = Options {
            deferred_violations: true,
            ..options.clone()
        };
        for (policy, annotate, how) in fixes {
            check_fixed(
                &oracle,
                &options,
                &format!("{how}-{run}"),
                *exact,
                policy,
                annotate,
            );
        }
    }
}

#[test]
fn spool_errors_catch_sql_plus_errors_and_skip_details() {
    let spool = "Insert into APP.T (A) values ('1')\n*\nERROR at line 1:\n\
        ORA-00001: unique constraint (APP.T_PK) violated on table APP.T columns (A)\n\
        ORA-03301: (ORA-00001 details) row with column values (A:1) already exists\n\
        Help: https://docs.oracle.com/error-help/db/ora-00001/\n\n\
        SP2-0310: Unable to open file: \"app/data/X.sql\"\n\
        SP2-0734: unknown command beginning \"foo\" - rest of line ignored.\n\
        ORA-1234: four digits is not an ORA- code\n";
    let found = errors(spool);
    let codes: Vec<&str> = found.iter().map(|e| e.code.as_str()).collect();
    assert_eq!(codes, ["ORA-00001", "SP2-0310", "SP2-0734"]);
    assert_eq!(
        found[0].echo.as_deref(),
        Some("Insert into APP.T (A) values ('1')")
    );
    assert_eq!(
        row_signature(&found[0]),
        Some(("duplicate", "APP.T_PK".to_owned()))
    );
    assert_eq!(row_signature(&found[1]), None, "an SP2 error fails the run");
}

/// A finding on `line` of `f.sql`, a duplicate of `first` when there is one.
fn found(kind: FindingKind, constraint: Option<&str>, line: u64, first: Option<u64>) -> Finding {
    Finding {
        kind,
        constraint: constraint.map(str::to_owned),
        table: "APP.T".to_owned(),
        columns: vec!["A".to_owned()],
        file: Some("f.sql".to_owned()),
        line: Some(line),
        key: vec![Some("1".to_owned())],
        first: first.map(|line| Location {
            file: "f.sql".to_owned(),
            line,
        }),
        count: None,
        assumed: false,
    }
}

/// A key's rows on lines 1 to 3, line 1 rejected for NULL in another column: Oracle takes line 2 for the key and
/// rejects line 3 as its duplicate. After the data, line 1 is rejected at insert, and the key fails on 2 and 3; with
/// line 3 gone too, it holds.
#[test]
fn a_duplicate_of_a_row_rejected_at_insert_holds_its_key() {
    let null = found(FindingKind::NotNull, None, 1, None);
    let second = found(FindingKind::DuplicateKey, Some("APP.T_UK"), 2, Some(1));
    let third = found(FindingKind::DuplicateKey, Some("APP.T_UK"), 3, Some(1));
    let files = ["f.sql".to_owned()];
    let signature = |line, what: &'static str, detail: &str| Signature {
        location: Some(("f.sql".to_owned(), line)),
        what,
        detail: detail.to_owned(),
    };
    let findings = [null.clone(), second.clone(), third.clone()];
    assert_eq!(
        expected_load_order(&findings, &[], &files, true),
        counts([
            signature(1, "not null", "APP.T.A"),
            signature(3, "duplicate", "APP.T_UK"),
        ])
    );
    let cascade = Cascade {
        file: "f.sql".to_owned(),
        line: 1,
        constraint: "APP.T_FK".to_owned(),
        parent: Location {
            file: "f.sql".to_owned(),
            line: 9,
        },
    };
    assert_eq!(
        expected_load_order(std::slice::from_ref(&second), &[cascade], &files, true),
        counts([signature(1, "fk", "APP.T_FK")]),
        "a cascade row never holds its key either"
    );
    assert_eq!(
        expected_load_order(std::slice::from_ref(&second), &[], &files, true),
        counts([signature(2, "duplicate", "APP.T_UK")]),
        "a first row Oracle accepts holds its key"
    );
    assert_eq!(expected_after_data(&findings, &[]).keys, ["APP.T_UK"]);
    let after = expected_after_data(&[null, second], &[]);
    assert!(after.keys.is_empty(), "{after:?}");
}
