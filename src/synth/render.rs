//! What a dialect writes for the generator: the syntax of the export. The data, the plants and the manifest stay
//! neutral; a dialect only turns them into text.

use super::rng::Rng;
use super::schema::{Constraint, TableId};
use super::value::Value;

/// How the DDL is written.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DdlStyle<'a> {
    pub(crate) eol: &'a str,
    /// Primary and unique keys go into the table DDL; otherwise they are added after the data.
    pub(crate) keys_in_table: bool,
    /// The export has the key enabled between a table's rows, and the index it needs.
    pub(crate) key_between_rows: bool,
    /// The disabled foreign keys are in the DDL; otherwise `load.sql` adds them after the data.
    pub(crate) disabled_keys_in_ddl: bool,
}

/// A line of `load.sql`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ScriptItem<'a> {
    /// Runs the file at this path, beside the script.
    Include(&'a str),
    Commit,
    /// Lets the schema `to` point foreign keys at `parent`.
    GrantReferences {
        parent: TableId,
        to: &'static str,
    },
    /// A foreign key added after the data, enabled without checking the rows there.
    ForeignKeyAfterData(&'static Constraint),
}

/// Text a dialect wrote, and how many statements (SQL, PL/SQL blocks and script commands) are in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sql {
    pub(crate) text: String,
    pub(crate) statements: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DdlItem {
    CreateTable(TableId),
    /// The keys and unique index of a table whose keys come after the data.
    LateKeys(TableId),
    ForeignKeys(TableId),
    /// The key enabled between a table's rows, written into its data when constraints come before the data.
    KeyBetweenRows(TableId),
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
    /// Noise switches the dialect has no syntax for.
    fn unsupported_noise(&self) -> Vec<&'static str>;
    fn ddl(&self, item: DdlItem, style: &DdlStyle<'_>) -> Sql;
    fn literal(&self, value: &Value, rng: &mut Rng) -> String;
    /// What goes before and after one table's rows.
    fn data_section(&self, table: TableId, edge: Edge, eol: &str) -> Sql;
    fn insert(&self, insert: &Insert<'_>) -> String;
    fn batch(&self, part: Batch<'_>, eol: &str) -> String;
    /// What ends a table's rows: a commit.
    fn commit(&self, eol: &str) -> Sql;
    /// A script that runs the files in order, with paths relative to it, and what else it says.
    fn load_script(&self, items: &[ScriptItem<'_>], eol: &str) -> String;
}
