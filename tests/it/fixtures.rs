//! The generator's committed exports under `tests/fixtures/gen/oracle`, and what the tests read of their manifests.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sqlca::finding::{Finding, Location};
use sqlca::input::{InputFile, OrderSource, Role};
use sqlca::source::Encoding;

/// The parts of a manifest (`expected.json`) the tests read.
#[derive(Deserialize)]
pub(crate) struct Manifest {
    pub(crate) options: Options,
    pub(crate) files: Vec<OutputFile>,
    pub(crate) findings: Vec<Finding>,
    pub(crate) fixes: Fixes,
}

/// The rows `fix` removes or sets to NULL under keep-first.
#[derive(Deserialize)]
pub(crate) struct Fixes {
    pub(crate) removed: Vec<Location>,
    pub(crate) removed_on_fk_null: Vec<Location>,
    pub(crate) nulled_on_fk_null: Vec<Nulled>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub(crate) struct Nulled {
    pub(crate) file: String,
    pub(crate) line: u64,
    pub(crate) columns: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct Options {
    pub(crate) encoding: Encoding,
}

#[derive(Deserialize)]
pub(crate) struct OutputFile {
    pub(crate) path: String,
}

/// Every committed export's folder, by name.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture folder that can't be listed fails the test"
)]
pub(crate) fn generated() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gen/oracle");
    let mut exports: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    exports.sort();
    exports
}

/// The committed exports a manifest describes: the generator's, then Oracle's HR and CO sample schemas with the rows
/// `hr/hr_plants.sql` and `co/co_plants.sql` plant after them, whose manifest is written by hand.
pub(crate) fn with_manifests() -> Vec<PathBuf> {
    let mut exports = generated();
    exports.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/samples/oracle"));
    exports
}

#[expect(
    clippy::unwrap_used,
    reason = "a missing or malformed manifest fails the test"
)]
pub(crate) fn manifest(root: &Path) -> Manifest {
    serde_json::from_str(&fs::read_to_string(root.join("expected.json")).unwrap()).unwrap()
}

/// The export's files in `order.txt` order, `ref/` ones as reference files.
#[expect(
    clippy::unwrap_used,
    reason = "a fixture without order.txt or one of its files fails the test"
)]
pub(crate) fn inputs(root: &Path) -> Vec<InputFile> {
    fs::read_to_string(root.join("order.txt"))
        .unwrap()
        .lines()
        .map(|path| InputFile {
            path: root.join(path),
            role: if path.starts_with("ref/") {
                Role::Reference
            } else {
                Role::Data
            },
            bytes: fs::metadata(root.join(path)).unwrap().len(),
            order: OrderSource::Given,
            includes: Vec::new(),
        })
        .collect()
}
