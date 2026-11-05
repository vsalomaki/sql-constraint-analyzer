//! What a dialect writes for the generator: the syntax of the export. The data, the plants and the manifest stay
//! neutral; a dialect only turns them into text.

use super::rng::Rng;
use super::schema::TableId;
use super::value::Value;
use crate::dialect::Rules;

/// How the DDL is written.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DdlStyle<'a> {
    pub(crate) eol: &'a str,
    pub(crate) storage: bool,
    pub(crate) comments: bool,
    /// Primary and unique keys go into the table DDL; otherwise they are added after the data.
    pub(crate) keys_in_table: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DdlItem {
    CreateTable(TableId),
    /// The keys and unique index of a table whose keys come after the data.
    LateKeys(TableId),
    ForeignKeys(TableId),
    Sequence,
    Trigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    Start,
    End,
}

/// One row's insert, already in pieces: the dialect puts them together.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Insert<'a> {
    pub(crate) table: &'a str,
    /// `None`: no column list.
    pub(crate) columns: Option<&'a [String]>,
    pub(crate) values: &'a [String],
    /// Which keyword case to use, for the mixed-case noise; 0 is the dialect's usual.
    pub(crate) case: u64,
    /// Spread the column and value lists over several lines.
    pub(crate) wrap: bool,
    pub(crate) eol: &'a str,
}

/// Rows inserted by one statement.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Batch<'a> {
    Start,
    Row(&'a Insert<'a>),
    End,
}

pub(crate) trait Render {
    fn rules(&self) -> &'static Rules;
    /// Noise switches the dialect has no syntax for.
    fn unsupported_noise(&self) -> Vec<&'static str>;
    fn ddl(&self, item: DdlItem, style: &DdlStyle<'_>) -> String;
    fn literal(&self, value: &Value, rng: &mut Rng) -> String;
    /// What goes before and after one table's rows.
    fn data_section(&self, table: TableId, edge: Edge, eol: &str) -> String;
    fn insert(&self, insert: &Insert<'_>) -> String;
    fn batch(&self, part: Batch<'_>, eol: &str) -> String;
    /// A script that runs the files in order, with paths relative to it.
    fn load_script(&self, paths: &[&str], eol: &str) -> String;
}
