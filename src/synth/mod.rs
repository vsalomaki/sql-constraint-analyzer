//! Test-data generator: writes SQL Developer–style Oracle exports with planted constraint violations, and a
//! manifest listing the findings an analyzer must report for them.
//!
//! Every planted violation is an extra row with keys outside the normal rows' range, so no child row ever
//! references a bad one. Oracle (which rejects a bad row and then its children) and the analyzer (which judges each
//! row on its own) therefore agree on the findings by construction.

mod data;
mod out;
mod rng;
mod schema;
mod value;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::finding::Finding;
pub use out::{Encoding, LineEnding, OutputFile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// One file per schema.
    Single,
    /// One file per object and kind: `tables/EMPLOYEES.sql`, `data/EMPLOYEES.sql`, …
    #[default]
    Separate,
    /// One file per kind: `tables.sql`, `ref_constraints.sql`, `data.sql`, …
    Type,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    /// Keys and foreign keys exist before the data is loaded, so load order matters.
    #[default]
    BeforeData,
    /// Keys and foreign keys are added after the data; NOT NULL stays in the table DDL.
    AfterData,
}

/// Export quirks to write; each one is something a parser has to survive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches, one per quirk"
)]
pub struct Noise {
    pub rem_header: bool,
    pub set_define_off: bool,
    pub comments: bool,
    pub storage_clauses: bool,
    pub plsql_trigger: bool,
    pub quoted_numbers: bool,
    pub numeric_forms: bool,
    pub multiline_strings: bool,
    pub q_quote: bool,
    pub embedded_delimiters: bool,
    pub nextval: bool,
    pub insert_all: bool,
    pub mixed_case: bool,
    pub no_column_list: bool,
    /// Column and value lists broken over several lines.
    pub wrapped_statements: bool,
}

type NoiseSwitch = fn(&mut Noise) -> &mut bool;

const NOISE_SWITCHES: [(&str, NoiseSwitch); 15] = [
    ("rem-header", |n| &mut n.rem_header),
    ("set-define-off", |n| &mut n.set_define_off),
    ("comments", |n| &mut n.comments),
    ("storage-clauses", |n| &mut n.storage_clauses),
    ("plsql-trigger", |n| &mut n.plsql_trigger),
    ("quoted-numbers", |n| &mut n.quoted_numbers),
    ("numeric-forms", |n| &mut n.numeric_forms),
    ("multiline-strings", |n| &mut n.multiline_strings),
    ("q-quote", |n| &mut n.q_quote),
    ("embedded-delimiters", |n| &mut n.embedded_delimiters),
    ("nextval", |n| &mut n.nextval),
    ("insert-all", |n| &mut n.insert_all),
    ("mixed-case", |n| &mut n.mixed_case),
    ("no-column-list", |n| &mut n.no_column_list),
    ("wrapped-statements", |n| &mut n.wrapped_statements),
];

impl Noise {
    /// What a plain SQL Developer export has.
    pub fn sqldev() -> Self {
        Self {
            rem_header: true,
            set_define_off: true,
            storage_clauses: true,
            quoted_numbers: true,
            ..Self::default()
        }
    }

    pub fn all() -> Self {
        let mut noise = Self::default();
        for (_, switch) in NOISE_SWITCHES {
            *switch(&mut noise) = true;
        }
        noise
    }

    /// A comma-separated list of presets (`sqldev`, `all`, `none`) and switch names, added together.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut noise = Self::default();
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part {
                "none" => {}
                "sqldev" => noise = noise.union(Self::sqldev()),
                "all" => noise = Self::all(),
                name => {
                    let (_, switch) =
                        NOISE_SWITCHES
                            .iter()
                            .find(|(n, _)| *n == name)
                            .ok_or_else(|| {
                                format!("unknown noise switch `{name}`; known: {}", Self::names())
                            })?;
                    *switch(&mut noise) = true;
                }
            }
        }
        Ok(noise)
    }

    pub fn names() -> String {
        NOISE_SWITCHES
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn union(mut self, other: Self) -> Self {
        for (_, switch) in NOISE_SWITCHES {
            let mut other = other;
            if *switch(&mut other) {
                *switch(&mut self) = true;
            }
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Options {
    pub seed: u64,
    /// Number of employees; the other tables scale from it.
    pub rows: u64,
    pub layout: Layout,
    pub constraints: Placement,
    /// Write the main schema's data sections in random order instead of parents first.
    pub shuffle_order: bool,
    /// Extra planted violations per million rows, on top of one of each kind.
    pub violations_ppm: u32,
    /// Plant a missing parent on the deferred foreign key; a deferred violation rolls back a whole transaction.
    pub deferred_violations: bool,
    pub noise: Noise,
    pub line_ending: LineEnding,
    pub encoding: Encoding,
    /// Tables (`APP.PROJECTS`) whose data isn't written.
    pub omit_data: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            seed: 1,
            rows: 100,
            layout: Layout::default(),
            constraints: Placement::default(),
            shuffle_order: false,
            violations_ppm: 1000,
            deferred_violations: true,
            noise: Noise::sqldev(),
            line_ending: LineEnding::default(),
            encoding: Encoding::default(),
            omit_data: Vec::new(),
        }
    }
}

pub const MIN_ROWS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub generator: String,
    pub options: Options,
    /// The SQL files in load order; `load.sql` and `order.txt` list the same order.
    pub files: Vec<OutputFile>,
    pub findings: Vec<Finding>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} exists and is not an empty folder")]
    OutputNotEmpty(PathBuf),
    #[error("--rows must be at least {MIN_ROWS}, got {0}")]
    TooFewRows(u64),
    #[error("unknown table `{0}` in --omit-data")]
    UnknownTable(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub const MANIFEST_FILE: &str = "expected.json";
pub const LOAD_SCRIPT: &str = "load.sql";
pub const ORDER_FILE: &str = "order.txt";

/// Writes an export into `root`, which must not exist or be empty, and returns its manifest (also written there).
pub fn generate(options: &Options, root: &Path) -> Result<Manifest, Error> {
    if options.rows < MIN_ROWS {
        return Err(Error::TooFewRows(options.rows));
    }
    let omitted = options
        .omit_data
        .iter()
        .map(|name| {
            schema::TableId::from_qualified(name).ok_or_else(|| Error::UnknownTable(name.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if root.exists() && fs::read_dir(root)?.next().is_some() {
        return Err(Error::OutputNotEmpty(root.to_owned()));
    }
    fs::create_dir_all(root)?;

    let (files, findings) = data::write_export(options, root, &omitted)?;

    let eol = options.line_ending.as_str();
    let lines = |prefix: &str| {
        files.iter().fold(String::new(), |mut text, f| {
            text.push_str(prefix);
            text.push_str(&f.path);
            text.push_str(eol);
            text
        })
    };
    let load = format!("SET DEFINE OFF{eol}{}", lines("@@"));
    let order = lines("");
    fs::write(root.join(LOAD_SCRIPT), load)?;
    fs::write(root.join(ORDER_FILE), order)?;

    let manifest = Manifest {
        generator: format!("sqlca-gen {}", env!("CARGO_PKG_VERSION")),
        options: options.clone(),
        files,
        findings,
    };
    let mut json = serde_json::to_string_pretty(&manifest)?;
    json.push('\n');
    fs::write(root.join(MANIFEST_FILE), json)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_specs_add_up() {
        assert_eq!(Noise::parse("none").unwrap(), Noise::default());
        let n = Noise::parse("sqldev, q-quote").unwrap();
        assert!(n.rem_header && n.quoted_numbers && n.q_quote && !n.insert_all);
        assert_eq!(Noise::parse("all").unwrap(), Noise::all());
        assert!(
            Noise::parse("nope")
                .unwrap_err()
                .contains("unknown noise switch `nope`")
        );
    }
}
