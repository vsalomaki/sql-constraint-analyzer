//! The checks a matched row goes through: NOT NULL, primary and unique keys, then foreign keys, and what is left to
//! resolve once every row is in. Keys are compared by a 128-bit hash of their canonical values.

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

use xxhash_rust::xxh3::Xxh3;

use super::Warning;
use super::structure::Row;
use crate::catalog::{Catalog, Column, Constraint, Kind, Origin, Parent, Table, TableId};
use crate::dialect::{DialectId, Rules, UniqueNulls};
use crate::finding::{Finding, FindingKind, Location};
use crate::source::Encoding;
use crate::statement::{DataType, Expr, InsertRow, TypeFamily};
use crate::value::{self, Canonical, Literal};

/// Whether a value no constraint reads still counts: a string, or the default, in a date column may be text read as
/// ISO, which the run reports. Only a string or a default can be text.
fn may_be_date_text(column: &Column, expr: Option<&Expr<'_>>) -> bool {
    matches!(
        column.data_type.family,
        TypeFamily::Date | TypeFamily::Timestamp
    ) && (column.default_on_null
        || matches!(expr, None | Some(Expr::Default | Expr::String { .. })))
}

/// Where a row is: its file's position in the run, its offset there, and its line.
#[derive(Debug, Clone, Copy)]
pub(super) struct At {
    pub file: usize,
    pub offset: u64,
    pub line: u64,
}

impl At {
    /// Whether the row comes after `origin` in the order the files are run.
    fn after(self, origin: Origin) -> bool {
        (self.file, self.offset) > (origin.file, origin.offset)
    }
}

/// One constraint's keys, by hash, with each key's first row.
type KeyMap = FxMap<u128, First>;

/// A map hashed the way rustc's own maps are. Its keys are table and constraint positions, and key hashes that are
/// already xxh3, so a hash that resists crafted collisions would only cost time.
type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;
type FxSet<K> = HashSet<K, BuildHasherDefault<Fx>>;

#[derive(Default)]
struct Fx(u64);

impl Fx {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            for (to, from) in word.iter_mut().zip(chunk) {
                *to = *from;
            }
            self.add(u64::from_le_bytes(word));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.add(crate::source::to_u64(n));
    }

    fn write_u128(&mut self, n: u128) {
        let [low, high] =
            [n, n >> 64].map(|half| u64::try_from(half & u128::from(u64::MAX)).unwrap_or(0));
        self.add(low);
        self.add(high);
    }
}

/// Where a key was first seen, whether its value rests on an assumption, and whether a child row's foreign key found
/// it. Kept to 16 bytes, as the key maps hold one per key.
#[derive(Debug, Clone, Copy)]
struct First {
    line: u64,
    file: u32,
    assumed: bool,
    referenced: bool,
}

impl First {
    fn new(at: At, assumed: bool) -> Self {
        Self {
            line: at.line,
            file: u32::try_from(at.file).unwrap_or(u32::MAX),
            assumed,
            referenced: false,
        }
    }
}

/// What a check found out about the rows `fix` may cut, beside its findings.
#[derive(Debug, Default)]
pub(super) struct Holders {
    /// Rows with a finding, or a duplicated key's first row, that hold a key some child row found, by file and
    /// line: cutting one can leave a child without its parent.
    pub rows: BTreeSet<(String, u64)>,
    /// Tables with a key value the check couldn't know or assumed: cutting a row there can uncover a missing parent
    /// that no child found.
    pub uncertain: BTreeSet<String>,
}

/// A foreign key value with no parent row yet.
struct Pending {
    table: TableId,
    constraint: usize,
    parent: (TableId, usize),
    hash: u128,
    at: At,
    key: Vec<Option<String>>,
    /// A parent found later is a load-order finding: `--load-order`, an immediate foreign key, declared before the row.
    load_order: bool,
    assumed: bool,
}

/// Why a key map may not say exactly which keys exist.
#[derive(Debug, Clone, Copy, Default)]
struct Doubt {
    /// Some rows' keys couldn't be known, so a key missing from the map may still exist.
    unknown: bool,
    /// Some keys rest on an assumption, so whether a key matches does too.
    assumed: bool,
}

/// Rows of one constraint the checker couldn't decide, and a reason for one of them.
#[derive(Default)]
struct Undecided {
    count: u64,
    reason: String,
}

pub(super) struct Checker<'a> {
    catalog: &'a Catalog,
    dialect: DialectId,
    rules: &'static Rules,
    load_order: bool,
    /// File names by position, as findings show them.
    files: &'a [String],
    keys: FxMap<(TableId, usize), KeyMap>,
    /// What makes a key map less than certain, by key; lookups against it inherit the doubt.
    doubt: FxMap<(TableId, usize), Doubt>,
    rows: FxMap<TableId, u64>,
    /// Tables with rows `fix` removed: a parent among them had rows, so its children lack theirs.
    removed: FxSet<TableId>,
    pending: Vec<Pending>,
    findings: Vec<Finding>,
    unverifiable: FxMap<(TableId, usize), Undecided>,
    no_parent: FxMap<(TableId, usize), u64>,
    dates: FxMap<(TableId, usize), u64>,
    /// By table: whether some constraint reads each column.
    read: FxMap<TableId, Vec<bool>>,
}

impl<'a> Checker<'a> {
    pub(super) fn new(
        catalog: &'a Catalog,
        dialect: DialectId,
        load_order: bool,
        files: &'a [String],
    ) -> Self {
        Self {
            catalog,
            dialect,
            rules: dialect.rules(),
            load_order,
            files,
            keys: FxMap::default(),
            doubt: FxMap::default(),
            rows: FxMap::default(),
            removed: FxSet::default(),
            pending: Vec::new(),
            findings: Vec::new(),
            unverifiable: FxMap::default(),
            no_parent: FxMap::default(),
            dates: FxMap::default(),
            read: catalog
                .ids()
                .filter_map(|id| Some((id, catalog.table(id)?.read_columns())))
                .collect(),
        }
    }

    fn file(&self, index: usize) -> String {
        self.files.get(index).cloned().unwrap_or_default()
    }

    fn constraint_name(&self, table: &Table, constraint: &Constraint) -> Option<String> {
        let name = constraint.name.as_deref()?;
        Some(match &table.name.schema {
            Some(schema) => self.rules.qualified(schema, name),
            None => self.rules.display_ident(name).into_owned(),
        })
    }

    fn finding(
        &self,
        kind: FindingKind,
        table: &Table,
        constraint: &Constraint,
        columns: &[usize],
    ) -> Finding {
        Finding {
            kind,
            constraint: self.constraint_name(table, constraint),
            table: self.catalog.display(&table.name),
            columns: table.column_names(columns),
            file: None,
            line: None,
            key: Vec::new(),
            first: None,
            count: None,
            assumed: false,
        }
    }

    /// A structure finding, or nothing for a reference row.
    /// A row of `table` that `fix` removed.
    pub(super) fn removed(&mut self, table: TableId) {
        self.removed.insert(table);
    }

    pub(super) fn structure(&mut self, finding: Finding, reference: bool) {
        if !reference {
            self.findings.push(finding);
        }
    }

    /// Checks one statement's matched rows. All its keys go in before any of its foreign keys are looked up, since
    /// the database checks immediate constraints once the whole statement has run. A reference row only fills the
    /// key maps, so other rows can find their parents.
    pub(super) fn statement(
        &mut self,
        rows: &[(Row<'_>, &InsertRow<'_>, At)],
        encoding: Encoding,
        reference: bool,
    ) {
        let literals: Vec<Vec<Literal<'_>>> = rows
            .iter()
            .map(|(matched, row, at)| self.own_checks(matched, row, *at, encoding, reference))
            .collect();
        if reference {
            return;
        }
        let catalog = self.catalog;
        for ((matched, _, at), literals) in rows.iter().zip(&literals) {
            if let Some(table) = catalog.table(matched.table) {
                self.foreign_keys(matched.table, table, literals, *at);
            }
        }
    }

    /// A row's NOT NULL and key checks; returns its columns' values for the foreign key checks.
    fn own_checks<'v>(
        &mut self,
        matched: &Row<'v>,
        row: &InsertRow<'_>,
        at: At,
        encoding: Encoding,
        reference: bool,
    ) -> Vec<Literal<'v>> {
        let catalog = self.catalog;
        let Some(table) = catalog.table(matched.table) else {
            return Vec::new();
        };
        *self.rows.entry(matched.table).or_default() += 1;
        let read = self.read.get(&matched.table);
        let literals: Vec<Literal<'v>> = table
            .columns
            .iter()
            .zip(&matched.values)
            .enumerate()
            .map(|(position, (column, value))| {
                let expr = value.map(|v| &v.expr);
                let read = read.and_then(|r| r.get(position)).copied().unwrap_or(true);
                if read || may_be_date_text(column, expr) {
                    self.literal(column, expr, encoding)
                } else {
                    // No check reads it.
                    Literal::Unknown(String::new())
                }
            })
            .collect();
        if !reference {
            for (position, (column, literal)) in table.columns.iter().zip(&literals).enumerate() {
                if value::assumption(literal, column.data_type.family).is_some() {
                    *self.dates.entry((matched.table, position)).or_default() += 1;
                }
            }
            self.not_null(table, &literals, row, at);
        }
        self.keys_of(matched.table, table, &literals, row, at, reference);
        literals
    }

    /// The value a column gets: the row's, or the column's default where the row leaves it out, writes `DEFAULT`, or
    /// writes NULL into a `DEFAULT ON NULL` column.
    fn literal<'v>(
        &self,
        column: &Column,
        expr: Option<&Expr<'v>>,
        encoding: Encoding,
    ) -> Literal<'v> {
        let default = || match &column.default {
            Some(expr) => self.dialect.evaluate(expr, encoding),
            None if column.identity => {
                Literal::Unknown("an identity column's generated value".to_owned())
            }
            None => Literal::Null,
        };
        match expr {
            None | Some(Expr::Default) => default(),
            Some(expr) => match self.dialect.evaluate(expr, encoding) {
                literal if column.default_on_null && self.is_null(&literal) => default(),
                literal => literal,
            },
        }
    }

    fn is_null(&self, literal: &Literal) -> bool {
        match literal {
            Literal::Null => true,
            Literal::Text(text) => text.is_empty() && self.rules.empty_string_is_null,
            _ => false,
        }
    }

    fn applies(constraint: &Constraint, at: At) -> bool {
        constraint.judges(at.file, at.offset)
    }

    /// NULL in a NOT NULL column or a primary key column: one finding per column.
    fn not_null(&mut self, table: &Table, literals: &[Literal], row: &InsertRow, at: At) {
        for (column, literal) in literals.iter().enumerate() {
            if !self.is_null(literal) {
                continue;
            }
            if let Some(constraint) = table.forbids_null(column, at.file, at.offset) {
                let mut finding = self.finding(FindingKind::NotNull, table, constraint, &[column]);
                finding.file = Some(self.file(at.file));
                finding.line = Some(row.line);
                self.findings.push(finding);
            }
        }
    }

    /// Puts the row's primary and unique keys into their maps, reporting a key that is already there.
    fn keys_of(
        &mut self,
        id: TableId,
        table: &Table,
        literals: &[Literal],
        row: &InsertRow,
        at: At,
        reference: bool,
    ) {
        for (position, constraint) in table.constraints.iter().enumerate() {
            let (columns, primary) = match &constraint.kind {
                Kind::PrimaryKey { columns } => (columns, true),
                Kind::Unique { columns, .. } => (columns, false),
                _ => continue,
            };
            let typed = || {
                columns.iter().filter_map(|&c| {
                    Some((literals.get(c)?, table.columns.get(c)?.data_type.family))
                })
            };
            let parts: Vec<Canonical<'_>> = columns
                .iter()
                .filter_map(|&c| {
                    let column = table.columns.get(c)?;
                    Some(value::canonical(
                        literals.get(c)?,
                        &column.data_type,
                        self.rules,
                    ))
                })
                .collect();
            if parts.contains(&Canonical::Unknown) {
                self.doubt.entry((id, position)).or_default().unknown = true;
                if !reference && Self::applies(constraint, at) {
                    self.undecided((id, position), columns, literals);
                }
                continue;
            }
            let nulls = parts.iter().filter(|p| **p == Canonical::Null).count();
            let skip = if primary {
                nulls > 0
            } else {
                nulls == parts.len()
                    || (nulls > 0 && self.rules.unique_nulls == UniqueNulls::NeverConflict)
            };
            if skip {
                continue;
            }
            let hash = hash(&parts);
            let assumed =
                typed().any(|(literal, family)| value::assumption(literal, family).is_some());
            if assumed {
                self.doubt.entry((id, position)).or_default().assumed = true;
            }
            let first = match self.keys.entry((id, position)).or_default().entry(hash) {
                Entry::Occupied(first) => *first.get(),
                Entry::Vacant(slot) => {
                    slot.insert(First::new(at, assumed));
                    continue;
                }
            };
            if reference || !Self::applies(constraint, at) {
                continue;
            }
            let mut finding = self.finding(FindingKind::DuplicateKey, table, constraint, columns);
            finding.file = Some(self.file(at.file));
            finding.line = Some(row.line);
            finding.key = parts.iter().map(shown).collect();
            finding.first = Some(Location {
                file: self.file(usize::try_from(first.file).unwrap_or(usize::MAX)),
                line: first.line,
            });
            finding.assumed = assumed || first.assumed;
            self.findings.push(finding);
        }
    }

    fn undecided(&mut self, key: (TableId, usize), columns: &[usize], literals: &[Literal]) {
        self.count_unverifiable(key, || {
            columns
                .iter()
                .filter_map(|&c| literals.get(c))
                .find_map(|l| match l {
                    Literal::Unknown(reason) => Some(reason.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "a value its column's type can't hold".to_owned())
        });
    }

    fn count_unverifiable(&mut self, key: (TableId, usize), reason: impl FnOnce() -> String) {
        let entry = self.unverifiable.entry(key).or_default();
        entry.count += 1;
        if entry.reason.is_empty() {
            entry.reason = reason();
        }
    }

    /// Looks the row's foreign keys up in their parents' key maps; a miss waits for the end of the input.
    fn foreign_keys(&mut self, id: TableId, table: &Table, literals: &[Literal], at: At) {
        for (position, constraint) in table.constraints.iter().enumerate() {
            let Kind::ForeignKey { columns, parent } = &constraint.kind else {
                continue;
            };
            if !Self::applies(constraint, at) {
                continue;
            }
            let (parent_id, parent_key, parent_columns) = match parent {
                Parent::Key {
                    table,
                    constraint,
                    columns,
                } => (*table, *constraint, columns),
                Parent::Missing(_) => {
                    *self.no_parent.entry((id, position)).or_default() += 1;
                    continue;
                }
            };
            let Some(parent_table) = self.catalog.table(parent_id) else {
                continue;
            };
            // A child value is stored as its own column stores numbers, then compared as the parent's type compares.
            let parts: Vec<(Canonical<'_>, bool)> = columns
                .iter()
                .zip(
                    parent_columns
                        .iter()
                        .filter_map(|&c| parent_table.columns.get(c)),
                )
                .map(|(&child, parent)| {
                    let literal = literals.get(child).unwrap_or(&Literal::Null);
                    let compared = DataType {
                        decimal: table.columns.get(child).and_then(|c| c.data_type.decimal),
                        ..DataType::of(parent.data_type.family)
                    };
                    (
                        value::canonical(literal, &compared, self.rules),
                        value::assumption(literal, compared.family).is_some(),
                    )
                })
                .collect();
            if parts.iter().any(|(p, _)| *p == Canonical::Null) {
                continue;
            }
            if parts.iter().any(|(p, _)| *p == Canonical::Unknown) {
                self.undecided((id, position), columns, literals);
                continue;
            }
            let hash = hash(parts.iter().map(|(p, _)| p));
            let found = self.find_parent((parent_id, parent_key), hash);
            if !found {
                self.pending.push(Pending {
                    table: id,
                    constraint: position,
                    parent: (parent_id, parent_key),
                    hash,
                    at,
                    key: parts.iter().map(|(p, _)| shown(p)).collect(),
                    load_order: self.load_order
                        && at.after(constraint.enabled_at)
                        && !constraint.state.initially_deferred,
                    assumed: parts.iter().any(|(_, a)| *a),
                });
            }
        }
    }

    /// Whether the parent key map holds `hash`, marking the key as found by a child.
    fn find_parent(&mut self, parent: (TableId, usize), hash: u128) -> bool {
        match self
            .keys
            .get_mut(&parent)
            .and_then(|keys| keys.get_mut(&hash))
        {
            Some(first) => {
                first.referenced = true;
                true
            }
            None => false,
        }
    }

    /// Resolves what waited for the end of the input, and returns the findings and warnings, and which rows with a
    /// finding hold a key a child found.
    pub(super) fn finish(mut self) -> (Vec<Finding>, Vec<Warning>, Holders) {
        let mut empty: HashMap<(TableId, usize), u64> = HashMap::new();
        for pending in std::mem::take(&mut self.pending) {
            let found = self.find_parent(pending.parent, pending.hash);
            if found && !pending.load_order {
                continue;
            }
            let doubt = self.doubt.get(&pending.parent).copied().unwrap_or_default();
            if doubt.unknown {
                // The parent key may be one of the values the checker couldn't know, or have been one earlier.
                self.count_unverifiable((pending.table, pending.constraint), || {
                    "parent keys the checker can't know".to_owned()
                });
                continue;
            }
            let kind = if found {
                FindingKind::FkLoadOrder
            } else if self.rows.get(&pending.parent.0).copied().unwrap_or(0) == 0
                && !self.removed.contains(&pending.parent.0)
            {
                *empty
                    .entry((pending.table, pending.constraint))
                    .or_default() += 1;
                continue;
            } else {
                FindingKind::FkMissingParent
            };
            let Some((table, constraint, columns)) =
                self.foreign_key(pending.table, pending.constraint)
            else {
                continue;
            };
            let mut finding = self.finding(kind, table, constraint, columns);
            finding.file = Some(self.file(pending.at.file));
            finding.line = Some(pending.at.line);
            finding.key = pending.key;
            finding.assumed = pending.assumed || doubt.assumed;
            self.findings.push(finding);
        }
        for ((table, constraint), count) in empty {
            let Some((table, constraint, columns)) = self.foreign_key(table, constraint) else {
                continue;
            };
            let mut finding =
                self.finding(FindingKind::ParentTableEmpty, table, constraint, columns);
            finding.count = Some(count);
            self.findings.push(finding);
        }
        let warnings = self.warnings();
        let holders = self.holders();
        (self.findings, warnings, holders)
    }

    /// The rows with a finding, and duplicated keys' first rows, that hold a key a child found; one pass over the
    /// key maps, bounded by the findings.
    fn holders(&self) -> Holders {
        let index_of = |name: &str| self.files.iter().position(|f| f == name);
        let mut candidates: HashSet<(usize, u64)> = HashSet::new();
        for finding in &self.findings {
            if let (Some(file), Some(line)) =
                (finding.file.as_deref().and_then(index_of), finding.line)
            {
                candidates.insert((file, line));
            }
            if let Some(first) = &finding.first
                && let Some(file) = index_of(&first.file)
            {
                candidates.insert((file, first.line));
            }
        }
        let mut rows = BTreeSet::new();
        if !candidates.is_empty() {
            for first in self.keys.values().flat_map(FxMap::values) {
                let at = (
                    usize::try_from(first.file).unwrap_or(usize::MAX),
                    first.line,
                );
                if first.referenced && candidates.contains(&at) {
                    rows.insert((self.file(at.0), at.1));
                }
            }
        }
        let uncertain = self
            .doubt
            .iter()
            .filter(|(_, doubt)| doubt.unknown || doubt.assumed)
            .filter_map(|((table, _), _)| {
                Some(self.catalog.display(&self.catalog.table(*table)?.name))
            })
            .collect();
        Holders { rows, uncertain }
    }

    fn foreign_key(
        &self,
        table: TableId,
        constraint: usize,
    ) -> Option<(&'a Table, &'a Constraint, &'a [usize])> {
        let table = self.catalog.table(table)?;
        let constraint = table.constraints.get(constraint)?;
        let Kind::ForeignKey { columns, .. } = &constraint.kind else {
            return None;
        };
        Some((table, constraint, columns))
    }

    fn warnings(&self) -> Vec<Warning> {
        let catalog = self.catalog;
        let constraint = |(table, position): (TableId, usize)| {
            let table = catalog.table(table)?;
            let constraint = table.constraints.get(position)?;
            Some((
                catalog.display(&table.name),
                self.constraint_name(table, constraint),
                constraint,
            ))
        };
        let mut warnings = Vec::new();
        for (&key, undecided) in &self.unverifiable {
            if let Some((table, name, _)) = constraint(key) {
                warnings.push(Warning::Unverifiable {
                    table,
                    constraint: name,
                    count: undecided.count,
                    reason: undecided.reason.clone(),
                });
            }
        }
        for (&key, &count) in &self.no_parent {
            if let Some((
                table,
                name,
                Constraint {
                    kind:
                        Kind::ForeignKey {
                            parent: Parent::Missing(parent),
                            ..
                        },
                    ..
                },
            )) = constraint(key)
            {
                warnings.push(Warning::ParentNotInInput {
                    table,
                    constraint: name,
                    parent: parent.clone(),
                    count,
                });
            }
        }
        for (&(table, position), &count) in &self.dates {
            if let Some(t) = catalog.table(table)
                && let Some(column) = t.columns.get(position)
            {
                warnings.push(Warning::DateAssumed {
                    table: catalog.display(&t.name),
                    column: column.name.clone(),
                    count,
                });
            }
        }
        warnings.sort();
        warnings
    }
}

/// A key's canonical values as a finding shows them; `None` is NULL.
fn shown(part: &Canonical<'_>) -> Option<String> {
    match part {
        Canonical::Value(text) => Some(text.to_string()),
        Canonical::Null | Canonical::Unknown => None,
    }
}

/// The 128-bit hash of a key's canonical values, each one tagged and length-prefixed so no two keys run together.
fn hash<'p>(parts: impl IntoIterator<Item = &'p Canonical<'p>>) -> u128 {
    let mut hasher = Xxh3::new();
    for part in parts {
        match part {
            Canonical::Value(text) => {
                hasher.update(&[1]);
                hasher.update(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
                hasher.update(text.as_bytes());
            }
            Canonical::Null | Canonical::Unknown => hasher.update(&[0]),
        }
    }
    hasher.digest128()
}

#[cfg(test)]
mod size {
    #[test]
    fn a_key_maps_entry_stays_16_bytes() {
        assert_eq!(std::mem::size_of::<super::First>(), 16);
    }
}
