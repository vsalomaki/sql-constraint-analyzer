//! Test-data generator: writes exports with planted constraint violations, and a manifest listing the findings an
//! analyzer must report for them. The model, rows and plants here are neutral; the chosen dialect writes the syntax.
//!
//! Every planted violation is an extra row with keys outside the normal rows' range, so no normal row references a
//! bad one. The database rejects a bad row and then its children, while the analyzer judges each row on its own, so
//! the two agree on the findings by construction.
//!
//! With `cascades`, trees of rows hang from bad rows: a row with NULL in a NOT NULL column, its children, and theirs,
//! down a manager chain of `manager_chain` employees. The analyzer finds nothing in the children, since their parent
//! is in the export, but the database rejects them and `fix` removes them with their root, a level per round. Bad
//! rows also get duplicates: the database rejects the bad row and accepts the next row of its key, which `fix` keeps
//! once the bad row is gone. The manifest lists the cascade rows, and the rows `fix` removes or sets to NULL under
//! `--on-duplicate keep-first`.

mod data;
mod out;
pub(crate) mod render;
pub(crate) mod rng;
pub(crate) mod schema;
pub(crate) mod value;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dialect::DialectId;
use crate::finding::{Finding, Location};
pub use out::{Encoding, LineEnding, OutputFile};
pub(crate) use render::Render;

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
    #[serde(default)]
    pub dialect: DialectId,
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
    /// Tables whose rows get no commit after them.
    #[serde(default)]
    pub omit_commit: Vec<String>,
    /// The most rows a data file holds before the rows go on in `<name>_2.sql`, `<name>_3.sql`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_per_file: Option<u64>,
    /// Hang rows from bad rows: children the database rejects with their parent, and `fix` removes with it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cascades: bool,
    /// With `cascades`, the employees each tree's employee manages in a chain, each the manager of the next.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub manager_chain: u64,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dialect: DialectId::default(),
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
            omit_commit: Vec::new(),
            rows_per_file: None,
            cascades: false,
            manager_chain: 0,
        }
    }
}

pub const MIN_ROWS: u64 = 10;

/// The longest manager chain: `fix` removes one of its rows per round, and stops after `--max-rounds`, 30 unless
/// given.
pub const MAX_MANAGER_CHAIN: u64 = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub generator: String,
    pub options: Options,
    /// The SQL files in load order; `load.sql` and `order.txt` list the same order.
    pub files: Vec<OutputFile>,
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cascades: Vec<Cascade>,
    pub fixes: Fixes,
}

/// A row whose parent is a bad row or another cascade row. It has no finding of its own, but the database rejects
/// it once its parent is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cascade {
    pub file: String,
    pub line: u64,
    /// The foreign key that points at the parent.
    pub constraint: String,
    pub parent: Location,
}

/// What `sqlca fix` does to the rows under `--on-duplicate keep-first`. Under keep-last a duplicate's first row goes
/// instead, a normal row that other rows depend on, and with it every row that depends on it; that isn't predicted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fixes {
    /// The rows the default policy removes.
    pub removed: Vec<Location>,
    /// The rows `--on-fk null` removes.
    pub removed_on_fk_null: Vec<Location>,
    /// The rows `--on-fk null` keeps with these foreign key columns set to NULL.
    pub nulled_on_fk_null: Vec<Nulled>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Nulled {
    pub file: String,
    pub line: u64,
    pub columns: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} exists and is not an empty folder")]
    OutputNotEmpty(PathBuf),
    #[error("--rows must be at least {MIN_ROWS}, got {0}")]
    TooFewRows(u64),
    #[error("unknown table `{0}`; give one such as APP.PROJECTS")]
    UnknownTable(String),
    #[error("{dialect:?} has no syntax for the noise switches {switches:?}")]
    UnsupportedNoise {
        dialect: DialectId,
        switches: Vec<&'static str>,
    },
    #[error("--rows-per-file needs a layout with separate data files, not one file per schema")]
    SplitSingleFile,
    #[error("--cascades needs every table's data, so not --omit-data")]
    CascadesWithoutData,
    #[error("--manager-chain must be at most {MAX_MANAGER_CHAIN}, got {0}")]
    ChainTooLong(u64),
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
    // A schema's one file holds the DDL after its data too, which would then run before the later parts.
    if options.layout == Layout::Single && options.rows_per_file.is_some() {
        return Err(Error::SplitSingleFile);
    }
    // A cascade row whose parent table is left out would have no finding the fix acts on, and stay.
    if options.cascades && !options.omit_data.is_empty() {
        return Err(Error::CascadesWithoutData);
    }
    if options.manager_chain > MAX_MANAGER_CHAIN {
        return Err(Error::ChainTooLong(options.manager_chain));
    }
    let render = options.dialect.render(options.noise);
    let switches = render.unsupported_noise();
    if !switches.is_empty() {
        return Err(Error::UnsupportedNoise {
            dialect: options.dialect,
            switches,
        });
    }
    let tables = |names: &[String]| {
        names
            .iter()
            .map(|name| {
                schema::TableId::from_qualified(name)
                    .ok_or_else(|| Error::UnknownTable(name.clone()))
            })
            .collect::<Result<Vec<_>, _>>()
    };
    let omitted = tables(&options.omit_data)?;
    let uncommitted = tables(&options.omit_commit)?;
    if root.exists() && fs::read_dir(root)?.next().is_some() {
        return Err(Error::OutputNotEmpty(root.to_owned()));
    }
    fs::create_dir_all(root)?;

    let data::Written {
        files,
        findings,
        cascades,
        fixes,
    } = data::write_export(options, render.as_ref(), root, &omitted, &uncommitted)?;

    let eol = options.line_ending.as_str();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    fs::write(root.join(LOAD_SCRIPT), render.load_script(&paths, eol))?;
    let order = paths.iter().fold(String::new(), |mut text, path| {
        text.push_str(path);
        text.push_str(eol);
        text
    });
    fs::write(root.join(ORDER_FILE), order)?;

    let manifest = Manifest {
        generator: format!("sqlca-gen {}", env!("CARGO_PKG_VERSION")),
        options: options.clone(),
        files,
        findings,
        cascades,
        fixes,
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

    #[test]
    fn every_noise_field_has_a_switch_named_as_its_key() {
        let all = serde_json::to_value(Noise::all()).unwrap();
        let fields = all.as_object().unwrap();
        assert!(fields.values().all(|v| *v == true), "{all}");
        for (name, switch) in NOISE_SWITCHES {
            let mut noise = Noise::default();
            *switch(&mut noise) = true;
            let value = serde_json::to_value(noise).unwrap();
            let on: Vec<&str> = value
                .as_object()
                .unwrap()
                .iter()
                .filter(|(_, v)| **v == true)
                .map(|(k, _)| k.as_str())
                .collect();
            assert_eq!(on, [name]);
        }
    }
}
