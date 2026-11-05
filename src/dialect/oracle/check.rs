//! Ground truth from a real Oracle: loads generated exports into Oracle Free and checks that Oracle rejects exactly
//! the rows and constraints the generator's manifest expects. Run it with `make oracle-check`, which starts the
//! container and sets `SQLCA_ORACLE_CONTAINER`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use super::RULES;
use crate::finding::{Finding, FindingKind};
use crate::synth::schema::{APP_TABLES, Kind, REF_TABLES, constraints_of};
use crate::synth::{self, Encoding, LineEnding, Manifest, Noise, Options, Placement};

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
        driver.extend(["COMMIT;", "SPOOL OFF", "EXIT", ""].map(String::from));
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

fn expected_row(finding: &Finding) -> (&'static str, String) {
    let constraint = || finding.constraint.clone().unwrap_or_default();
    let column = || finding.columns.first().cloned().unwrap_or_default();
    match finding.kind {
        FindingKind::FkMissingParent | FindingKind::FkLoadOrder => ("fk", constraint()),
        FindingKind::DuplicateKey => ("duplicate", constraint()),
        FindingKind::NotNull => (
            "not null",
            format!("{}.{}", unquote(&finding.table), column()),
        ),
        FindingKind::UnknownColumn => ("unknown column", column()),
        FindingKind::UnknownTable => ("unknown table", unquote(&finding.table)),
        FindingKind::ValueCountMismatch => ("value count", String::new()),
        FindingKind::ParentTableEmpty => ("parent table empty", constraint()),
    }
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

/// Constraints in before the data: Oracle rejects each bad row at its insert, so every manifest finding must be
/// one rejected row, on the same line. With `exact` off (wrapped statements, multi-line strings, where SQL*Plus
/// prints a line other than the first), only the kinds and counts are compared.
fn check_load_order(oracle: &Oracle, options: &Options, run: &str, exact: bool) {
    let (_dir, export, manifest) = generate(options);
    let spool = oracle.load(&export, options.encoding, run);
    let index = line_index(&export, &manifest);
    let mut observed = Vec::new();
    for error in errors(&spool) {
        let (what, detail) =
            row_signature(&error).unwrap_or_else(|| panic!("{run}: unexpected {error:?}"));
        let location = exact.then(|| locate(&index, error.echo.as_deref().unwrap_or_default()));
        observed.push(Signature {
            location,
            what,
            detail,
        });
    }
    let expected = manifest.findings.iter().map(|f| {
        let (what, detail) = expected_row(f);
        let location = exact.then(|| {
            (
                f.file.clone().unwrap_or_default(),
                f.line.unwrap_or_default(),
            )
        });
        Signature {
            location,
            what,
            detail,
        }
    });
    let (expected, observed) = (counts(expected), counts(observed));
    assert_eq!(
        observed, expected,
        "{run}: Oracle's rejections (left) differ from the manifest (right)"
    );
    if exact {
        // Oracle reports one error per row, so the manifest must have one finding per row.
        let mut rows: Vec<_> = expected.keys().map(|s| &s.location).collect();
        let all = rows.len();
        rows.dedup();
        assert_eq!(
            rows.len(),
            all,
            "{run}: a row has two findings, but Oracle reports one"
        );
    }
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

fn expected_after_data(manifest: &Manifest) -> AfterData {
    let mut rows = Vec::new();
    let mut want = AfterData::default();
    for finding in &manifest.findings {
        match (finding.kind, &finding.constraint) {
            (FindingKind::DuplicateKey | FindingKind::NotNull, Some(c)) => {
                want.keys.push(c.clone());
            }
            (FindingKind::FkMissingParent, Some(c)) => want.fks.push(c.clone()),
            _ => {
                let (what, detail) = expected_row(finding);
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
        expected_after_data(&manifest),
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

fn every_quirk_but_wrapping() -> Noise {
    Noise {
        wrapped_statements: false,
        multiline_strings: false,
        ..Noise::all()
    }
}

#[test]
#[ignore = "needs Oracle Free in Docker: run `make oracle-check`"]
fn oracle_agrees_with_the_manifest() {
    let oracle = Oracle::from_env();
    // A deferred violation would roll back the whole load at COMMIT, with no row to match.
    let base = Options {
        rows: 200,
        violations_ppm: 20_000,
        deferred_violations: false,
        ..Options::default()
    };
    check_load_order(&oracle, &base, "sqldev", true);
    let quirks = Options {
        seed: 2,
        noise: every_quirk_but_wrapping(),
        line_ending: LineEnding::Crlf,
        encoding: Encoding::Windows1252,
        ..base.clone()
    };
    check_load_order(&oracle, &quirks, "quirks", true);
    check_load_order(
        &oracle,
        &Options {
            seed: 3,
            noise: Noise::all(),
            ..base.clone()
        },
        "wrapped",
        false,
    );
    let after = Options {
        seed: 4,
        constraints: Placement::AfterData,
        deferred_violations: true,
        ..base
    };
    check_after_data(&oracle, &after, "after-data");
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
