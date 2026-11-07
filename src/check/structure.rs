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
    pub values: Vec<Option<&'a Value<'a>>>,
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

/// Matches `row`, in the file at `file`, to its table and columns. Without a column list the values follow the
/// table's column order, without invisible columns, as Oracle and the SQL standard fill them.
pub fn resolve_row<'a>(
    catalog: &Catalog,
    row: &'a InsertRow<'a>,
    file: usize,
) -> Result<Row<'a>, StructureError> {
    let id = match catalog.resolve(&row.table, file) {
        Lookup::Found(id) => id,
        Lookup::Missing => return Err(StructureError::UnknownTable),
        Lookup::Ambiguous(_) => return Err(StructureError::AmbiguousTable),
    };
    let Some(table) = catalog.table(id) else {
        return Err(StructureError::UnknownTable);
    };
    let mut values = vec![None; table.columns.len()];
    if let Some(names) = &row.columns {
        // Exports list the columns in the table's order, so the column at the same place is tried first.
        let found: Vec<Option<usize>> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let in_place = table.columns.get(i).is_some_and(|c| c.name == n.value);
                if in_place {
                    Some(i)
                } else {
                    table.column(&n.value)
                }
            })
            .collect();
        let unknown: Vec<String> = names
            .iter()
            .zip(&found)
            .filter(|(_, position)| position.is_none())
            .map(|(n, _)| n.value.to_string())
            .collect();
        if !unknown.is_empty() {
            return Err(StructureError::UnknownColumns(unknown));
        }
        count_matches(names.len(), row)?;
        for (position, value) in found.into_iter().flatten().zip(&row.values) {
            if let Some(slot) = values.get_mut(position) {
                *slot = Some(value);
            }
        }
    } else {
        count_matches(table.columns.iter().filter(|c| !c.invisible).count(), row)?;
        let slots = values
            .iter_mut()
            .zip(&table.columns)
            .filter(|(_, c)| !c.invisible);
        for ((slot, _), value) in slots.zip(&row.values) {
            *slot = Some(value);
        }
    }
    Ok(Row { table: id, values })
}

fn count_matches(columns: usize, row: &InsertRow) -> Result<(), StructureError> {
    if columns == row.values.len() {
        Ok(())
    } else {
        Err(StructureError::ValueCount {
            columns,
            values: row.values.len(),
        })
    }
}

/// The table of `row`, in the file at `file`, as findings show it: the catalog's name for it; else the name in the
/// schema it would be in, or as written when several schemas have it.
pub(super) fn table_name(catalog: &Catalog, row: &InsertRow, file: usize) -> String {
    match catalog.resolve(&row.table, file) {
        Lookup::Found(id) => catalog.table(id).map_or_else(
            || catalog.display_name(&row.table, Some(file)),
            |t| catalog.display(&t.name),
        ),
        Lookup::Missing => catalog.display_name(&row.table, Some(file)),
        Lookup::Ambiguous(_) => catalog.display_name(&row.table, None),
    }
}

impl StructureError {
    /// The finding for `row` in `file`, the file at `index` in the run.
    pub fn finding(&self, catalog: &Catalog, row: &InsertRow, file: &str, index: usize) -> Finding {
        let (kind, columns) = match self {
            Self::UnknownTable => (FindingKind::UnknownTable, Vec::new()),
            Self::AmbiguousTable => (FindingKind::AmbiguousTable, Vec::new()),
            Self::UnknownColumns(names) => (FindingKind::UnknownColumn, names.clone()),
            Self::ValueCount { .. } => (FindingKind::ValueCountMismatch, Vec::new()),
        };
        Finding {
            kind,
            constraint: None,
            table: table_name(catalog, row, index),
            columns,
            file: Some(file.to_owned()),
            line: Some(row.line),
            key: Vec::new(),
            first: None,
            unchecked: Vec::new(),
            unchecked_more: false,
            count: None,
            assumed: false,
        }
    }
}
