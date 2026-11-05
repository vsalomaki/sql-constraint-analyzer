//! Structure comes first: the row's table and columns must exist and its values must fit them, or the row gets that
//! one finding and no other check sees it.

use crate::catalog::{Catalog, Lookup, TableId};
use crate::finding::{Finding, FindingKind};
use crate::statement::{InsertRow, Value};

/// A row matched to its table: each of the table's columns with the value the row gives it.
#[derive(Debug, PartialEq, Eq)]
pub struct Row<'a> {
    pub table: TableId,
    /// By column position; `None` where the row leaves the column out, so its default applies.
    pub values: Vec<Option<&'a Value>>,
}

/// Why a row can't be matched to its table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureError {
    UnknownTable,
    /// Tables of that name in several schemas.
    AmbiguousTable,
    /// The column-list names the table doesn't have, in list order.
    UnknownColumns(Vec<String>),
    ValueCount {
        columns: usize,
        values: usize,
    },
}

/// Matches `row` to its table and columns. Without a column list the values follow the table's column order, without
/// invisible columns, as Oracle and the SQL standard fill them.
pub fn resolve_row<'a>(catalog: &Catalog, row: &'a InsertRow) -> Result<Row<'a>, StructureError> {
    let id = match catalog.resolve(&row.table) {
        Lookup::Found(id) => id,
        Lookup::Missing => return Err(StructureError::UnknownTable),
        Lookup::Ambiguous(_) => return Err(StructureError::AmbiguousTable),
    };
    let Some(table) = catalog.table(id) else {
        return Err(StructureError::UnknownTable);
    };
    let positions: Vec<usize> = match &row.columns {
        Some(names) => {
            let found: Vec<Option<usize>> = names.iter().map(|n| table.column(&n.value)).collect();
            let unknown: Vec<String> = names
                .iter()
                .zip(&found)
                .filter(|(_, position)| position.is_none())
                .map(|(n, _)| n.value.clone())
                .collect();
            if !unknown.is_empty() {
                return Err(StructureError::UnknownColumns(unknown));
            }
            found.into_iter().flatten().collect()
        }
        None => table
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.invisible)
            .map(|(position, _)| position)
            .collect(),
    };
    if positions.len() != row.values.len() {
        return Err(StructureError::ValueCount {
            columns: positions.len(),
            values: row.values.len(),
        });
    }
    let mut values = vec![None; table.columns.len()];
    for (position, value) in positions.into_iter().zip(&row.values) {
        if let Some(slot) = values.get_mut(position) {
            *slot = Some(value);
        }
    }
    Ok(Row { table: id, values })
}

impl StructureError {
    /// The finding for `row` in `file`.
    pub fn finding(&self, catalog: &Catalog, row: &InsertRow, file: &str) -> Finding {
        let table = match catalog.resolve(&row.table) {
            Lookup::Found(id) => catalog.table(id).map(|t| catalog.display(&t.name)),
            _ => None,
        };
        let (kind, columns) = match self {
            Self::UnknownTable => (FindingKind::UnknownTable, Vec::new()),
            Self::AmbiguousTable => (FindingKind::AmbiguousTable, Vec::new()),
            Self::UnknownColumns(names) => (FindingKind::UnknownColumn, names.clone()),
            Self::ValueCount { .. } => (FindingKind::ValueCountMismatch, Vec::new()),
        };
        Finding {
            kind,
            constraint: None,
            table: table.unwrap_or_else(|| catalog.display_name(&row.table)),
            columns,
            file: Some(file.to_owned()),
            line: Some(row.line),
            key: Vec::new(),
            first: None,
            count: None,
            assumed: false,
        }
    }
}
