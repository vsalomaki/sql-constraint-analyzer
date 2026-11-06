//! The tables an export defines, with their ordered columns and constraints, built from its DDL statements.
//!
//! Exports often run DDL out of order (a version-sorted glob reads `ref_constraints/` before `tables/`), so the
//! builder collects the statements first and applies them in two steps: every `CREATE TABLE`, then every other
//! statement in the order given. Names compare by their resolved text, so `emp`, `EMP` and `"EMP"` are one table.

use std::collections::HashMap;

use crate::dialect::Rules;
use crate::statement::{
    AlterAction, ColumnDef, ColumnDefault, ConstraintDef, ConstraintKind, ConstraintState,
    DataType, Ident, IndexColumn, Name, Statement,
};

/// Where a statement is in the input: the file's position in the run, and its offset and line in that file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin {
    pub file: usize,
    pub offset: u64,
    pub line: u64,
}

/// Something the catalog couldn't apply, or applies differently from what the statement says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub origin: Origin,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableId(usize);

/// A table's name; `schema` is `None` for a table created without one when no default schema was given.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableName {
    pub schema: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
    pub default: Option<ColumnDefault>,
    pub default_on_null: bool,
    pub identity: bool,
    pub invisible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    /// `None` for an unnamed one (Oracle would name it `SYS_C…`).
    pub name: Option<String>,
    pub kind: Kind,
    pub state: ConstraintState,
    pub declared: Origin,
    /// Where it last became enabled: its declaration, or the `ENABLE` that turned it on. Rows before it are judged
    /// only when the constraint validates the rows already there.
    pub enabled_at: Origin,
}

/// A constraint, with columns as positions in the table's column list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    PrimaryKey {
        columns: Vec<usize>,
    },
    Unique {
        columns: Vec<usize>,
        /// Written by `CREATE UNIQUE INDEX`, so its name is the index's.
        index: bool,
    },
    ForeignKey {
        /// With a parent key, in that key's column order.
        columns: Vec<usize>,
        parent: Parent,
    },
    NotNull {
        column: usize,
    },
    Check {
        condition: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parent {
    /// The parent table's primary key or unique constraint, by its position in that table's constraints.
    Key { table: TableId, constraint: usize },
    /// The parent table isn't in the input, so the foreign key can't be verified; its name as written.
    Missing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub name: TableName,
    pub columns: Vec<Column>,
    pub constraints: Vec<Constraint>,
    pub created: Origin,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// The constraint that forbids NULL in the column at `position` for a row at `offset` in the file at `file` in
    /// the run: a NOT NULL, else a primary key, that judges the row.
    pub fn forbids_null(&self, position: usize, file: usize, offset: u64) -> Option<&Constraint> {
        let judging = |c: &&Constraint| c.judges(file, offset);
        let not_null = self
            .constraints
            .iter()
            .filter(judging)
            .find(|c| matches!(c.kind, Kind::NotNull { column } if column == position));
        not_null.or_else(|| {
            self.constraints.iter().filter(judging).find(
                |c| matches!(&c.kind, Kind::PrimaryKey { columns } if columns.contains(&position)),
            )
        })
    }
}

impl Constraint {
    /// Whether the constraint judges a row at `offset` in the file at `file` in the run: enabled, and either enabled
    /// before the row or validating the rows that were there when it was enabled.
    pub fn judges(&self, file: usize, offset: u64) -> bool {
        self.state.enabled
            && ((file, offset) > (self.enabled_at.file, self.enabled_at.offset)
                || self.state.validated)
    }
}

/// How a name in a statement resolves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(TableId),
    Missing,
    /// Unqualified, no default schema, and tables of that name in several schemas: never guessed.
    Ambiguous(Vec<TableId>),
}

#[derive(Debug, Clone)]
pub struct Catalog {
    rules: &'static Rules,
    default_schema: Option<String>,
    tables: Vec<Table>,
    by_name: HashMap<String, Vec<TableId>>,
}

impl Catalog {
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// Whether an enabled constraint is checked only at commit, so a commit before the rows it needs rolls back.
    pub fn defers_checks(&self) -> bool {
        self.tables
            .iter()
            .flat_map(|t| &t.constraints)
            .any(|c| c.state.enabled && c.state.initially_deferred)
    }

    pub fn table(&self, id: TableId) -> Option<&Table> {
        self.tables.get(id.0)
    }

    /// A qualified name matches its schema only. An unqualified one matches the default schema when one is given,
    /// else a table of that name in any schema.
    pub fn resolve(&self, name: &Name<'_>) -> Lookup {
        let candidates = self
            .by_name
            .get(&*name.name.value)
            .map_or(&[][..], Vec::as_slice);
        let schema = name
            .schema
            .as_ref()
            .map(|s| &*s.value)
            .or(self.default_schema.as_deref());
        let mut found = candidates.iter().copied().filter(|&id| {
            schema.is_none() || self.table(id).and_then(|t| t.name.schema.as_deref()) == schema
        });
        match (found.next(), found.next()) {
            (None, _) => Lookup::Missing,
            (Some(id), None) => Lookup::Found(id),
            (Some(first), Some(second)) => {
                Lookup::Ambiguous([first, second].into_iter().chain(found).collect())
            }
        }
    }

    /// A table's name as the dialect displays it.
    pub fn display(&self, name: &TableName) -> String {
        display(self.rules, name)
    }

    /// A name from a statement as the dialect displays it, in the default schema when it has none.
    pub fn display_name(&self, name: &Name<'_>) -> String {
        display(
            self.rules,
            &TableName {
                schema: name
                    .schema
                    .as_ref()
                    .map(|s| s.value.to_string())
                    .or_else(|| self.default_schema.clone()),
                name: name.name.value.to_string(),
            },
        )
    }

    fn add_table(&mut self, table: Table) {
        let id = TableId(self.tables.len());
        self.by_name
            .entry(table.name.name.clone())
            .or_default()
            .push(id);
        self.tables.push(table);
    }
}

/// Collects DDL statements in input order; `finish` builds the catalog from them.
#[derive(Debug)]
pub struct CatalogBuilder {
    rules: &'static Rules,
    default_schema: Option<String>,
    statements: Vec<(Statement<'static>, Origin)>,
}

impl CatalogBuilder {
    /// `default_schema` as the dialect stores it (already folded).
    pub fn new(rules: &'static Rules, default_schema: Option<String>) -> Self {
        Self {
            rules,
            default_schema,
            statements: Vec::new(),
        }
    }

    /// Keeps a DDL statement; inserts and skipped statements aren't the catalog's.
    pub fn add(&mut self, statement: Statement<'static>, origin: Origin) {
        if matches!(
            statement,
            Statement::CreateTable { .. }
                | Statement::AlterTable { .. }
                | Statement::CreateIndex { .. }
        ) {
            self.statements.push((statement, origin));
        }
    }

    pub fn finish(self) -> (Catalog, Vec<Issue>) {
        let mut build = Build {
            catalog: Catalog {
                rules: self.rules,
                default_schema: self.default_schema,
                tables: Vec::new(),
                by_name: HashMap::new(),
            },
            drafts: Vec::new(),
            issues: Vec::new(),
        };
        let (creates, changes): (Vec<_>, Vec<_>) = self
            .statements
            .into_iter()
            .partition(|(s, _)| matches!(s, Statement::CreateTable { .. }));
        for (statement, origin) in creates {
            if let Statement::CreateTable {
                name,
                columns,
                constraints,
            } = statement
            {
                build.create(&name, columns, constraints, origin);
            }
        }
        for (statement, origin) in changes {
            match statement {
                Statement::AlterTable { table, actions } => build.alter(&table, actions, origin),
                Statement::CreateIndex {
                    name,
                    table,
                    unique: true,
                    columns,
                } => build.unique_index(&name, &table, columns, origin),
                _ => {}
            }
        }
        build.resolve();
        build.issues.sort_by_key(|i| i.origin);
        (build.catalog, build.issues)
    }
}

/// A table's constraints and unique indexes as written, columns still by name, until every table is known.
#[derive(Default)]
struct Draft {
    constraints: Vec<Written>,
    indexes: Vec<(String, Vec<Ident<'static>>, Origin)>,
}

/// A constraint as written, with where it was declared and where it last became enabled.
struct Written {
    def: ConstraintDef,
    declared: Origin,
    enabled_at: Origin,
}

impl Written {
    fn new(def: ConstraintDef, origin: Origin) -> Self {
        Self {
            def,
            declared: origin,
            enabled_at: origin,
        }
    }
}

struct Build {
    catalog: Catalog,
    /// One per table, at the table's position.
    drafts: Vec<Draft>,
    issues: Vec<Issue>,
}

fn display(rules: &Rules, name: &TableName) -> String {
    match &name.schema {
        Some(schema) => rules.qualified(schema, &name.name),
        None => rules.display_ident(&name.name).into_owned(),
    }
}

fn display_name(rules: &Rules, name: &Name<'_>) -> String {
    display(
        rules,
        &TableName {
            schema: name.schema.as_ref().map(|s| s.value.to_string()),
            name: name.name.value.to_string(),
        },
    )
}

fn label(def: &ConstraintDef) -> String {
    def.name.as_ref().map_or_else(
        || "an unnamed constraint".to_owned(),
        |n| format!("constraint {}", n.value),
    )
}

fn same_columns(a: &[usize], b: &[usize]) -> bool {
    let sorted = |c: &[usize]| {
        let mut c = c.to_vec();
        c.sort_unstable();
        c
    };
    sorted(a) == sorted(b)
}

/// The columns of a key a foreign key can reference: a primary key, or a unique constraint (not a unique index).
fn referable(kind: &Kind) -> Option<(&[usize], bool)> {
    match kind {
        Kind::PrimaryKey { columns } => Some((columns, true)),
        Kind::Unique {
            columns,
            index: false,
        } => Some((columns, false)),
        _ => None,
    }
}

fn add_columns(
    rules: &Rules,
    table: &mut Table,
    draft: &mut Draft,
    columns: Vec<ColumnDef>,
    origin: Origin,
    issues: &mut Vec<Issue>,
) {
    for column in columns {
        if table.column(&column.name.value).is_some() {
            issues.push(Issue {
                origin,
                message: format!(
                    "column {} of {} is defined twice; the first is kept",
                    column.name.value,
                    display(rules, &table.name)
                ),
            });
            continue;
        }
        draft.constraints.extend(
            column
                .constraints
                .into_iter()
                .map(|c| Written::new(c, origin)),
        );
        table.columns.push(Column {
            name: column.name.value.into_owned(),
            data_type: column.data_type,
            default: column.default,
            default_on_null: column.default_on_null,
            identity: column.identity,
            invisible: column.invisible,
        });
    }
}

/// The key of `parent` that `child` references, with the child's columns put in that key's order.
fn parent_key(
    rules: &Rules,
    id: TableId,
    parent: &Table,
    names: Option<&[Ident<'_>]>,
    child: &[usize],
) -> Result<Kind, String> {
    let shown = display(rules, &parent.name);
    let referenced = match names {
        None => None,
        Some(names) => Some(
            names
                .iter()
                .map(|n| {
                    parent.column(&n.value).ok_or_else(|| {
                        format!(
                            "references column {} of {shown}, which the table doesn't have",
                            n.value
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
    };
    let key = parent
        .constraints
        .iter()
        .enumerate()
        .find_map(|(position, c)| {
            let (columns, primary) = referable(&c.kind)?;
            let fits = match &referenced {
                None => primary,
                Some(r) => same_columns(columns, r),
            };
            fits.then_some((position, columns))
        });
    let Some((position, key_columns)) = key else {
        let wanted = if referenced.is_some() {
            "primary key or unique constraint on those columns"
        } else {
            "primary key"
        };
        return Err(format!("references {shown}, which has no {wanted}"));
    };
    let referenced = referenced.unwrap_or_else(|| key_columns.to_vec());
    if referenced.len() != child.len() {
        return Err(format!(
            "has {} columns but the key it references in {shown} has {}",
            child.len(),
            referenced.len()
        ));
    }
    let columns = key_columns
        .iter()
        .map(|k| {
            referenced
                .iter()
                .position(|r| r == k)
                .and_then(|i| child.get(i).copied())
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| format!("doesn't match the key it references in {shown}"))?;
    Ok(Kind::ForeignKey {
        columns,
        parent: Parent::Key {
            table: id,
            constraint: position,
        },
    })
}

impl Build {
    fn issue(&mut self, origin: Origin, message: String) {
        self.issues.push(Issue { origin, message });
    }

    fn create(
        &mut self,
        name: &Name<'_>,
        columns: Vec<ColumnDef>,
        constraints: Vec<ConstraintDef>,
        origin: Origin,
    ) {
        let rules = self.catalog.rules;
        let table_name = TableName {
            schema: name
                .schema
                .as_ref()
                .map(|s| s.value.to_string())
                .or_else(|| self.catalog.default_schema.clone()),
            name: name.name.value.to_string(),
        };
        if self.catalog.tables.iter().any(|t| t.name == table_name) {
            let shown = display(rules, &table_name);
            self.issue(
                origin,
                format!("table {shown} is created again; the first definition is kept"),
            );
            return;
        }
        let mut draft = Draft::default();
        let mut table = Table {
            name: table_name,
            columns: Vec::new(),
            constraints: Vec::new(),
            created: origin,
        };
        add_columns(
            rules,
            &mut table,
            &mut draft,
            columns,
            origin,
            &mut self.issues,
        );
        draft
            .constraints
            .extend(constraints.into_iter().map(|c| Written::new(c, origin)));
        self.catalog.add_table(table);
        self.drafts.push(draft);
    }

    /// The position of the table a statement changes, or an issue saying why there is none.
    fn target(&mut self, name: &Name<'_>, origin: Origin, what: &str) -> Option<usize> {
        let shown = display_name(self.catalog.rules, name);
        match self.catalog.resolve(name) {
            Lookup::Found(id) => return Some(id.0),
            Lookup::Missing => self.issue(
                origin,
                format!("{what} names table {shown}, which no CREATE TABLE in the input defines"),
            ),
            Lookup::Ambiguous(ids) => {
                let schemas = self.schemas(&ids);
                self.issue(
                    origin,
                    format!(
                        "{what} names table {shown}, which exists in several schemas ({schemas}); give a default schema"
                    ),
                );
            }
        }
        None
    }

    fn schemas(&self, ids: &[TableId]) -> String {
        let rules = self.catalog.rules;
        ids.iter()
            .filter_map(|&id| self.catalog.table(id))
            .map(|t| {
                t.name.schema.as_deref().map_or_else(
                    || "no schema".to_owned(),
                    |s| rules.display_ident(s).into_owned(),
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn alter(&mut self, name: &Name<'_>, actions: Vec<AlterAction>, origin: Origin) {
        let Some(index) = self.target(name, origin, "ALTER TABLE") else {
            return;
        };
        let Self {
            catalog,
            drafts,
            issues,
        } = self;
        let rules = catalog.rules;
        let (Some(table), Some(draft)) = (catalog.tables.get_mut(index), drafts.get_mut(index))
        else {
            return;
        };
        let shown = display(rules, &table.name);
        for action in actions {
            match action {
                AlterAction::AddConstraint(constraint) => {
                    draft.constraints.push(Written::new(constraint, origin));
                }
                AlterAction::AddColumns(columns) => {
                    add_columns(rules, table, draft, columns, origin, issues);
                }
                AlterAction::ModifyColumns(items) => {
                    for (_, constraints) in items {
                        draft
                            .constraints
                            .extend(constraints.into_iter().map(|c| Written::new(c, origin)));
                    }
                }
                AlterAction::ChangeConstraint { name, change } => {
                    let found = draft
                        .constraints
                        .iter_mut()
                        .find(|d| d.def.name.as_ref().is_some_and(|n| n.value == name.value));
                    if let Some(declared) = found {
                        if change.enabled == Some(true) && !declared.def.state.enabled {
                            declared.enabled_at = origin;
                        }
                        declared.def.state.apply(change);
                    } else {
                        issues.push(Issue {
                            origin,
                            message: format!(
                                "constraint {} of {shown} is changed, but no statement adds it",
                                name.value
                            ),
                        });
                    }
                }
                AlterAction::Other(text) => issues.push(Issue {
                    origin,
                    message: format!("ALTER TABLE {shown} {text}: not followed"),
                }),
            }
        }
    }

    fn unique_index(
        &mut self,
        name: &Name<'_>,
        table: &Name<'_>,
        columns: Vec<IndexColumn>,
        origin: Origin,
    ) {
        let Some(index) = self.target(table, origin, "CREATE UNIQUE INDEX") else {
            return;
        };
        let mut plain = Vec::new();
        for column in columns {
            match column {
                IndexColumn::Column(c) => plain.push(c),
                IndexColumn::Expression(_) => {
                    self.issue(
                        origin,
                        format!(
                            "unique index {} has an expression, so it is not checked",
                            name.name.value
                        ),
                    );
                    return;
                }
            }
        }
        if let Some(draft) = self.drafts.get_mut(index) {
            draft
                .indexes
                .push((name.name.value.to_string(), plain, origin));
        }
    }

    /// Columns to positions, unique indexes to keys, then foreign keys to their parents' keys.
    fn resolve(&mut self) {
        let drafts = std::mem::take(&mut self.drafts);
        let mut foreign = Vec::new();
        for (index, draft) in drafts.into_iter().enumerate() {
            for declared in draft.constraints {
                if matches!(declared.def.kind, ConstraintKind::ForeignKey { .. }) {
                    foreign.push((index, declared));
                } else if let Some(constraint) = self.local(index, declared) {
                    self.push_constraint(index, constraint);
                }
            }
            for (name, columns, origin) in draft.indexes {
                let label = format!("unique index {name}");
                let Some(columns) = self.positions(index, &columns, origin, &label) else {
                    continue;
                };
                if !self.covered_by_key(index, &columns) {
                    self.push_constraint(
                        index,
                        Constraint {
                            name: Some(name),
                            kind: Kind::Unique {
                                columns,
                                index: true,
                            },
                            state: ConstraintState::default(),
                            declared: origin,
                            enabled_at: origin,
                        },
                    );
                }
            }
        }
        for (index, declared) in foreign {
            if let Some(constraint) = self.foreign_key(index, declared) {
                self.push_constraint(index, constraint);
            }
        }
    }

    fn push_constraint(&mut self, index: usize, constraint: Constraint) {
        if let Some(table) = self.catalog.tables.get_mut(index) {
            table.constraints.push(constraint);
        }
    }

    /// A primary key or unique constraint on the same columns that judges every row already makes an index's key
    /// unique; one added with NOVALIDATE doesn't judge the rows the index did.
    fn covered_by_key(&self, index: usize, columns: &[usize]) -> bool {
        self.catalog.tables.get(index).is_some_and(|t| {
            t.constraints.iter().any(|c| {
                c.state.enabled
                    && c.state.validated
                    && referable(&c.kind).is_some_and(|(key, _)| same_columns(key, columns))
            })
        })
    }

    /// A constraint of one table, its columns resolved; `None` with an issue when one is missing.
    fn local(&mut self, index: usize, written: Written) -> Option<Constraint> {
        let Written {
            def,
            declared: origin,
            enabled_at,
        } = written;
        let label = label(&def);
        let kind = match def.kind {
            ConstraintKind::PrimaryKey { columns } => Kind::PrimaryKey {
                columns: self.positions(index, &columns, origin, &label)?,
            },
            ConstraintKind::Unique { columns } => Kind::Unique {
                columns: self.positions(index, &columns, origin, &label)?,
                index: false,
            },
            ConstraintKind::NotNull { column } => Kind::NotNull {
                column: *self.positions(index, &[column], origin, &label)?.first()?,
            },
            ConstraintKind::Check { condition } => Kind::Check { condition },
            ConstraintKind::ForeignKey { .. } => return None,
        };
        Some(Constraint {
            name: def.name.map(|n| n.value.into_owned()),
            kind,
            state: def.state,
            declared: origin,
            enabled_at,
        })
    }

    fn positions(
        &mut self,
        index: usize,
        columns: &[Ident<'_>],
        origin: Origin,
        label: &str,
    ) -> Option<Vec<usize>> {
        let table = self.catalog.tables.get(index)?;
        let missing = columns.iter().find(|c| table.column(&c.value).is_none());
        if let Some(column) = missing {
            let message = format!(
                "{label} names column {} of {}, which the table doesn't have",
                column.value,
                display(self.catalog.rules, &table.name)
            );
            self.issue(origin, message);
            return None;
        }
        columns.iter().map(|c| table.column(&c.value)).collect()
    }

    fn foreign_key(&mut self, index: usize, written: Written) -> Option<Constraint> {
        let Written {
            def,
            declared: origin,
            enabled_at,
        } = written;
        let label = label(&def);
        let ConstraintKind::ForeignKey {
            columns,
            parent,
            parent_columns,
        } = def.kind
        else {
            return None;
        };
        let child = self.positions(index, &columns, origin, &label)?;
        let rules = self.catalog.rules;
        let kind = match self.catalog.resolve(&parent) {
            Lookup::Found(id) => {
                let table = self.catalog.table(id)?;
                match parent_key(rules, id, table, parent_columns.as_deref(), &child) {
                    Ok(kind) => kind,
                    Err(message) => {
                        self.issue(origin, format!("{label} {message}"));
                        return None;
                    }
                }
            }
            Lookup::Missing => Kind::ForeignKey {
                columns: child,
                parent: Parent::Missing(display_name(rules, &parent)),
            },
            Lookup::Ambiguous(ids) => {
                let schemas = self.schemas(&ids);
                let shown = display_name(rules, &parent);
                self.issue(
                    origin,
                    format!(
                        "{label} references {shown}, which exists in several schemas ({schemas}); give a default schema"
                    ),
                );
                return None;
            }
        };
        Some(Constraint {
            name: def.name.map(|n| n.value.into_owned()),
            kind,
            state: def.state,
            declared: origin,
            enabled_at,
        })
    }
}
