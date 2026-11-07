//! The rows of each table, the planted findings, and the order everything is written in.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use super::out::{Out, OutputFile};
use super::render::{Batch, DdlItem, DdlStyle, Edge, Insert, Render};
use super::rng::Rng;
use super::schema::{
    self, APP_SCHEMA, APP_TABLES, AUDIT_SEQ, Constraint, Kind, REF_SCHEMA, REF_TABLES, State,
    TableId,
};
use super::value::Value;
use super::{Cascade, Fixes, Layout, Nulled, Options, Placement};
use crate::dialect::{Fold, Rules, UniqueNulls};
use crate::finding::{Finding, FindingKind, Location};
use crate::value::days_from_civil;

#[derive(Debug, Clone, Copy)]
enum Section {
    Table,
    Sequence,
    Trigger,
    RefConstraints,
    Constraints,
    Data,
}

fn file_path(layout: Layout, schema: &str, section: Section, object: &str) -> String {
    let dir = schema.to_ascii_lowercase();
    let kind = match section {
        Section::Table => "tables",
        Section::Sequence => "sequences",
        Section::Trigger => "triggers",
        Section::RefConstraints => "ref_constraints",
        Section::Constraints => "constraints",
        Section::Data => "data",
    };
    match layout {
        Layout::Single => format!("{dir}/export.sql"),
        Layout::Separate => format!("{dir}/{kind}/{object}.sql"),
        Layout::Type => format!("{dir}/{kind}.sql"),
    }
}

/// Part `part` of a data file: the file itself, then `<name>_2.sql`, `<name>_3.sql`, …
fn part_path(path: &str, part: u64) -> String {
    match path.rsplit_once('.') {
        Some((stem, extension)) if part > 1 => format!("{stem}_{part}.{extension}"),
        _ => path.to_owned(),
    }
}

/// What an export holds, as the manifest lists it.
pub(crate) struct Written {
    /// In load order.
    pub(crate) files: Vec<OutputFile>,
    pub(crate) findings: Vec<Finding>,
    pub(crate) cascades: Vec<Cascade>,
    pub(crate) fixes: Fixes,
}

/// Writes the whole export.
pub(crate) fn write_export(
    options: &Options,
    render: &dyn Render,
    root: &Path,
    omitted: &[TableId],
    uncommitted: &[TableId],
) -> io::Result<Written> {
    let mut w = Writer::new(options, render, root, omitted, uncommitted);
    let sizes = Sizes::new(options.rows, options.manager_chain);
    // One tree of each shape, and one more per extra plant the violation rate gives the employees.
    if options.cascades {
        w.trees = 2 + sizes.employees * u64::from(options.violations_ppm) / 1_000_000;
    }
    let mut order_rng = Rng::new(options.seed, 0);
    for (schema, tables) in [(REF_SCHEMA, &REF_TABLES[..]), (APP_SCHEMA, &APP_TABLES[..])] {
        w.schema_ddl(schema, tables)?;
        let mut data_order = tables.to_vec();
        if options.shuffle_order && schema == APP_SCHEMA {
            order_rng.shuffle(&mut data_order);
        }
        for table in data_order {
            w.begin_table(table)?;
            match table {
                TableId::Countries => countries(&mut w)?,
                TableId::Departments => departments(&mut w, &sizes)?,
                TableId::Employees => employees(&mut w, &sizes)?,
                TableId::Projects => projects(&mut w, &sizes)?,
                TableId::Assignments => assignments(&mut w, &sizes)?,
                TableId::Timesheets => timesheets(&mut w, &sizes)?,
                TableId::LegacyNotes => legacy_notes(&mut w, &sizes)?,
                TableId::AuditLog => audit_log(&mut w, &sizes)?,
            }
            w.end_table()?;
        }
        if options.constraints == Placement::AfterData {
            for &table in tables {
                w.ddl(
                    schema,
                    Section::Constraints,
                    table.name(),
                    DdlItem::LateKeys(table),
                )?;
            }
            for &table in tables {
                w.ddl(
                    schema,
                    Section::RefConstraints,
                    table.name(),
                    DdlItem::ForeignKeys(table),
                )?;
            }
        }
    }
    w.finish()
}

struct Sizes {
    employees: u64,
    departments: u64,
    projects: u64,
    notes: u64,
    audit: u64,
    /// The employees under each tree's employee in its manager chain.
    chain: u64,
}

impl Sizes {
    fn new(rows: u64, chain: u64) -> Self {
        Self {
            employees: rows,
            departments: (rows / 20).max(5),
            projects: (rows / 10).max(5),
            notes: (rows / 50).max(3),
            audit: (rows / 10).max(3),
            chain,
        }
    }

    // Extra rows take keys above every normal row's key, so a planted row never shares a key by accident.

    fn extra_dept_id(&self, k: u64) -> u64 {
        dept_id(self.departments) + k
    }

    fn extra_emp_id(&self, k: u64) -> u64 {
        emp_id(self.employees) + k
    }

    /// Normal timesheet ids run up to three per assignment, two assignments per employee.
    fn extra_ts_id(&self, k: u64) -> u64 {
        6 * self.employees + k
    }

    fn extra_log_id(&self, k: u64) -> u64 {
        self.audit + k
    }

    // A cascade tree's keys come after every extra's: a table has at most one extra per normal row, besides one of
    // each kind.

    fn cascade_dept_id(&self, tree: u64) -> u64 {
        self.extra_dept_id(self.departments + EXTRA_KINDS + tree)
    }

    /// A tree's employee `i`: 0 is the tree's own, then its manager chain, then the duplicates of bad rows.
    fn tree_emp_id(&self, tree: u64, i: u64) -> u64 {
        let per_tree = 1 + self.chain + SHIFTED_ROWS;
        self.extra_emp_id(self.employees + EXTRA_KINDS + tree * per_tree + i)
    }
}

/// More than any table's kinds of plant.
const EXTRA_KINDS: u64 = 16;

/// The employees `shifted_duplicates` writes for one tree.
const SHIFTED_ROWS: u64 = 6;

/// Days after 2000-01-01 where the extra timesheets' work dates start, and how many follow before Oracle's last date.
const EXTRA_DAYS: u64 = 9000;
const EXTRA_DAY_SPAN: u64 = 2_900_000;

/// The work date of extra timesheet `k`, including cascade ones. A timesheet whose foreign key `--on-fk null` sets to
/// NULL keeps only its work date in the day key, so these dates differ while a table has fewer extras than the span.
fn extra_work_date(k: u64) -> Value {
    let days = EXTRA_DAYS + k % EXTRA_DAY_SPAN;
    Value::Date(days_from_civil(2000, 1, 1) + i64::try_from(days).unwrap_or(0))
}

/// A foreign key value no row has: every key in the export is positive.
fn missing_id(k: u64) -> Value {
    Value::Int(-i64::try_from(k).unwrap_or(i64::MAX))
}

#[derive(Debug, Clone, Copy)]
enum Target {
    /// The parent table's normal row with this index.
    Row(u64),
    /// No parent row has this key.
    Missing,
    /// A bad row or a cascade row: in the export, so the analyzer finds no missing parent, but the database rejects
    /// it.
    Bad,
}

/// A cascade row: its tree, its table's stream, and which of the tree's rows in that table it is.
type Node = (u64, u64, u64);

fn node(tree: u64, table: TableId, n: u64) -> Node {
    (tree, table.stream(), n)
}

/// Each cascade row's place, and unless it is the root the foreign key to its parent and the parent.
type CascadeRows = BTreeMap<Node, (Location, Option<(&'static Constraint, Node)>)>;

struct Row {
    cells: Vec<(&'static str, Value)>,
    /// `false`: no column list, so `cells` hold every column in catalog order.
    column_list: bool,
    /// A table that doesn't exist, for the unknown-table plant.
    table_name: Option<&'static str>,
    drop_last_value: bool,
}

impl Row {
    fn new(cells: Vec<(&'static str, Value)>) -> Self {
        Self {
            cells,
            column_list: true,
            table_name: None,
            drop_last_value: false,
        }
    }

    fn get(&self, column: &str) -> Option<&Value> {
        self.cells
            .iter()
            .find(|(c, _)| *c == column)
            .map(|(_, v)| v)
    }

    fn key(&self, columns: &[&str], rules: &Rules) -> Vec<Option<String>> {
        columns
            .iter()
            .map(|c| self.get(c).and_then(|v| v.canonical(rules)))
            .collect()
    }
}

/// A non-FK finding a row is planted to produce.
struct Plant {
    kind: FindingKind,
    constraint: Option<String>,
    table: Option<String>,
    columns: Vec<String>,
    key: Vec<Option<String>>,
    /// Fill `key` from the row's values of `columns` when it is written.
    key_from_row: bool,
    first: Option<Location>,
}

impl Plant {
    fn new(kind: FindingKind, constraint: Option<String>, columns: &[&str]) -> Self {
        Self {
            kind,
            constraint,
            table: None,
            columns: columns.iter().map(|c| (*c).to_owned()).collect(),
            key: Vec::new(),
            key_from_row: false,
            first: None,
        }
    }

    /// A duplicate of `columns`; its key is read from the row when the row is written.
    fn duplicate(constraint: String, columns: &[&str], first: Option<&Location>) -> Self {
        Self {
            key_from_row: true,
            first: first.cloned(),
            ..Self::new(FindingKind::DuplicateKey, Some(constraint), columns)
        }
    }

    fn not_null(column: &'static str, constraint: Option<String>) -> Self {
        Self::new(FindingKind::NotNull, constraint, &[column])
    }
}

/// The rows a file has had since its last commit: the first one's place and table, and how many.
struct OpenRows {
    at: Location,
    table: String,
    rows: u64,
}

struct Writer<'o> {
    options: &'o Options,
    omitted: &'o [TableId],
    /// Tables whose rows get no commit after them.
    uncommitted: &'o [TableId],
    /// By file path.
    open: BTreeMap<String, OpenRows>,
    render: &'o dyn Render,
    rules: &'static Rules,
    out: Out,
    eol: &'static str,
    findings: Vec<Finding>,
    empty_parents: Vec<(&'static Constraint, u64)>,
    loaded: Vec<TableId>,
    table: TableId,
    /// The current table's data file, as the layout names it, and the part of it rows go to now.
    base_path: String,
    data_path: String,
    /// By data file as the layout names it: its current part, from 1, and the rows that part has.
    parts: BTreeMap<String, (u64, u64)>,
    /// Decides the data; kept apart from `style_rng` so the noise options don't change the rows.
    rng: Rng,
    style_rng: Rng,
    last_index: u64,
    extras: u64,
    group_left: u64,
    /// The cascade trees in the export, and the next one whose row the current table writes.
    trees: u64,
    next_tree: u64,
    cascade_rows: CascadeRows,
    fixes: Fixes,
}

impl<'o> Writer<'o> {
    fn new(
        options: &'o Options,
        render: &'o dyn Render,
        root: &Path,
        omitted: &'o [TableId],
        uncommitted: &'o [TableId],
    ) -> Self {
        Self {
            options,
            omitted,
            uncommitted,
            open: BTreeMap::new(),
            render,
            rules: options.dialect.rules(),
            out: Out::new(root, options.encoding),
            eol: options.line_ending.as_str(),
            findings: Vec::new(),
            empty_parents: Vec::new(),
            loaded: Vec::new(),
            table: TableId::Countries,
            base_path: String::new(),
            data_path: String::new(),
            parts: BTreeMap::new(),
            rng: Rng::new(options.seed, 1),
            style_rng: Rng::new(options.seed, 2),
            last_index: 0,
            extras: 0,
            group_left: 0,
            trees: 0,
            next_tree: 0,
            cascade_rows: BTreeMap::new(),
            fixes: Fixes::default(),
        }
    }

    fn style(&self) -> DdlStyle<'static> {
        DdlStyle {
            eol: self.eol,
            keys_in_table: self.options.constraints == Placement::BeforeData,
        }
    }

    fn ddl(
        &mut self,
        schema: &str,
        section: Section,
        object: &str,
        item: DdlItem,
    ) -> io::Result<()> {
        let sql = self.render.ddl(item, &self.style());
        if sql.text.is_empty() {
            return Ok(());
        }
        let path = file_path(self.options.layout, schema, section, object);
        let text = format!("{}{}", sql.text, self.eol);
        self.out.write(&path, &text, sql.statements)?;
        // DDL commits the rows before it.
        self.open.remove(&path);
        Ok(())
    }

    /// Tables, the sequence and trigger, and (before the data) the foreign keys of one schema.
    fn schema_ddl(&mut self, schema: &str, tables: &[TableId]) -> io::Result<()> {
        if schema == APP_SCHEMA && self.options.noise.nextval {
            self.ddl(schema, Section::Sequence, AUDIT_SEQ, DdlItem::Sequence)?;
        }
        for &table in tables {
            self.ddl(
                schema,
                Section::Table,
                table.name(),
                DdlItem::CreateTable(table),
            )?;
        }
        if schema == APP_SCHEMA && self.options.noise.plsql_trigger {
            self.ddl(
                schema,
                Section::Trigger,
                schema::AUDIT_TRIGGER,
                DdlItem::Trigger,
            )?;
        }
        if self.options.constraints == Placement::BeforeData {
            for &table in tables {
                self.ddl(
                    schema,
                    Section::RefConstraints,
                    table.name(),
                    DdlItem::ForeignKeys(table),
                )?;
            }
        }
        Ok(())
    }

    fn begin_table(&mut self, table: TableId) -> io::Result<()> {
        self.table = table;
        self.base_path = file_path(
            self.options.layout,
            table.schema(),
            Section::Data,
            table.name(),
        );
        let part = self.parts.get(&self.base_path).map_or(1, |(part, _)| *part);
        self.data_path = part_path(&self.base_path, part);
        self.rng = Rng::new(self.options.seed, 100 + table.stream());
        self.style_rng = Rng::new(self.options.seed, 200 + table.stream());
        self.last_index = 0;
        self.extras = 0;
        self.group_left = 0;
        self.next_tree = 0;
        if self.omitted.contains(&table) {
            return Ok(());
        }
        let header = self.render.data_section(table, Edge::Start, self.eol);
        if !header.text.is_empty() {
            self.write_data(&header.text, header.statements)?;
        }
        Ok(())
    }

    fn end_table(&mut self) -> io::Result<()> {
        if !self.omitted.contains(&self.table) {
            self.end_part(true)?;
        }
        self.loaded.push(self.table);
        Ok(())
    }

    /// Ends the current table's rows in the current file, all of them with `done`: the commit, unless there is to be
    /// none, then the footer.
    fn end_part(&mut self, done: bool) -> io::Result<()> {
        self.close_group()?;
        if !self.uncommitted.contains(&self.table) && !self.commit_rolls_back(done) {
            let commit = self.render.commit(self.eol);
            self.write_data(&commit.text, commit.statements)?;
            self.open.remove(&self.data_path);
        }
        let footer = self.render.data_section(self.table, Edge::End, self.eol);
        if !footer.text.is_empty() {
            self.write_data(&footer.text, footer.statements)?;
        }
        Ok(())
    }

    /// Before a statement: once the current part has its rows, ends it and goes on in the next.
    fn next_part_if_full(&mut self) -> io::Result<()> {
        let Some(limit) = self.options.rows_per_file else {
            return Ok(());
        };
        let (part, rows) = self.parts.get(&self.base_path).copied().unwrap_or((1, 0));
        if rows < limit {
            return Ok(());
        }
        self.end_part(false)?;
        self.out.close(&self.data_path)?;
        self.parts.insert(self.base_path.clone(), (part + 1, 0));
        self.data_path = part_path(&self.base_path, part + 1);
        let header = self.render.data_section(self.table, Edge::Start, self.eol);
        if !header.text.is_empty() {
            self.write_data(&header.text, header.statements)?;
        }
        Ok(())
    }

    /// Whether a commit after the current table's rows, all of them with `done`, would roll back: a deferred foreign
    /// key, in place before the data and checked at commit, of this table or one written before it, whose parent's
    /// rows aren't all written yet.
    fn commit_rolls_back(&self, done: bool) -> bool {
        let written =
            |parent: TableId| self.loaded.contains(&parent) || (done && parent == self.table);
        !self.after_data()
            && self.loaded.iter().chain([&self.table]).any(|&table| {
                schema::constraints_of(table).any(|c| {
                    self.deferred(c)
                        && matches!(c.kind, Kind::Foreign { parent, .. } if !written(parent))
                })
            })
    }

    /// Whether `fk` is checked at commit rather than per row.
    fn deferred(&self, fk: &Constraint) -> bool {
        self.rules.deferrable_foreign_keys
            && matches!(fk.kind, Kind::Foreign { deferred: true, .. })
    }

    fn write_data(&mut self, text: &str, statements: u64) -> io::Result<Location> {
        self.out.write(&self.data_path, text, statements)
    }

    fn close_group(&mut self) -> io::Result<()> {
        if self.group_left > 0 {
            self.group_left = 0;
            let text = self.render.batch(Batch::End, self.eol);
            // An `INSERT ALL` is one statement, however many rows it has.
            self.write_data(&text, 1)?;
        }
        Ok(())
    }

    fn next_extra(&mut self) -> u64 {
        self.extras += 1;
        self.extras
    }

    fn after_data(&self) -> bool {
        self.options.constraints == Placement::AfterData
    }

    /// Whether the end-state check applies: a not-validated constraint added after the data never checks the rows
    /// already there.
    fn checks_end_state(&self, fk: &Constraint) -> bool {
        match fk.state {
            State::Enforced => true,
            State::NotValidated => !self.after_data(),
            State::Disabled => false,
        }
    }

    fn checks_load_order(&self, fk: &Constraint) -> bool {
        !self.after_data() && fk.state != State::Disabled && !self.deferred(fk)
    }

    fn parent_loaded(&self, parent: TableId, index: u64, current: u64) -> bool {
        if parent == self.table {
            index <= current
        } else {
            self.loaded.contains(&parent)
        }
    }

    /// A normal row: valid in the end state and, unless the data is shuffled, in load order too. Children reference
    /// it by index, so a finding here would leave the database rejecting rows the manifest calls valid.
    fn normal(
        &mut self,
        index: u64,
        row: &Row,
        refs: &[(&'static Constraint, Target)],
    ) -> io::Result<Option<Location>> {
        self.last_index = index;
        let before = self.findings.len();
        let at = self.emit(row, index, refs, Vec::new(), true)?;
        if !self.options.shuffle_order
            && let Some(finding) = self.findings.get(before)
        {
            return Err(io::Error::other(format!(
                "generator bug: normal row {index} of {} has a finding: {finding:?}",
                self.table.qualified(self.rules)
            )));
        }
        Ok(at)
    }

    /// An extra row carrying planted findings; its keys are outside the normal range.
    fn extra(
        &mut self,
        row: &Row,
        refs: &[(&'static Constraint, Target)],
        plants: Vec<Plant>,
    ) -> io::Result<Option<Location>> {
        self.emit(row, self.last_index, refs, plants, true)
    }

    /// A row whose key duplicates a bad row's, the key's first: the database rejects the bad row, then accepts this
    /// one, and the fix keeps it once the bad row is gone. Under `--on-fk null` it goes when `first_stays` there.
    fn second_holder(&mut self, row: &Row, duplicate: Plant, first_stays: bool) -> io::Result<()> {
        if let Some(at) = self.emit(row, self.last_index, &[], vec![duplicate], false)?
            && first_stays
        {
            self.fixes.removed_on_fk_null.push(at);
        }
        Ok(())
    }

    /// The cascade trees whose rows of the current table come after its normal row `index` of `n`: the trees spread
    /// evenly over each table.
    fn trees_at(&mut self, index: u64, n: u64) -> Vec<u64> {
        let mut trees = Vec::new();
        while self.next_tree < self.trees && (self.next_tree + 1) * n / (self.trees + 1) == index {
            trees.push(self.next_tree);
            self.next_tree += 1;
        }
        trees
    }

    /// A cascade tree's row in the current table: its root, with `plants`, or a row whose foreign key points at
    /// `parent`, besides its other `refs`.
    fn cascade(
        &mut self,
        at: Node,
        parent: Option<(&'static Constraint, Node)>,
        row: &Row,
        refs: &[(&'static Constraint, Target)],
        plants: Vec<Plant>,
    ) -> io::Result<Option<Location>> {
        let mut all = refs.to_vec();
        all.extend(parent.map(|(fk, _)| (fk, Target::Bad)));
        let written = self.emit(row, self.last_index, &all, plants, true)?;
        if let Some(location) = &written {
            self.cascade_rows.insert(at, (location.clone(), parent));
        }
        Ok(written)
    }

    /// Writes a row and adds its findings; with `record`, also what the fix does to it, as its findings decide.
    fn emit(
        &mut self,
        row: &Row,
        current: u64,
        refs: &[(&'static Constraint, Target)],
        plants: Vec<Plant>,
        record: bool,
    ) -> io::Result<Option<Location>> {
        if self.omitted.contains(&self.table) {
            return Ok(None);
        }
        let mut fk_findings = Vec::new();
        for &(fk, target) in refs {
            let key = row.key(fk.columns, self.rules);
            if key.iter().any(Option::is_none) {
                continue;
            }
            let Kind::Foreign { parent, .. } = fk.kind else {
                continue;
            };
            if self.omitted.contains(&parent) {
                if self.checks_end_state(fk) {
                    self.count_empty_parent(fk);
                }
                continue;
            }
            let kind = match target {
                Target::Missing if self.checks_end_state(fk) => FindingKind::FkMissingParent,
                Target::Row(index)
                    if self.checks_load_order(fk)
                        && !self.parent_loaded(parent, index, current) =>
                {
                    FindingKind::FkLoadOrder
                }
                // A bad parent in the same table, a manager in its chain, is written before its child.
                Target::Bad
                    if self.checks_load_order(fk)
                        && parent != self.table
                        && !self.loaded.contains(&parent) =>
                {
                    FindingKind::FkLoadOrder
                }
                _ => continue,
            };
            fk_findings.push(Plant {
                key,
                ..Plant::new(kind, Some(fk.qualified(self.rules)), fk.columns)
            });
        }
        let has_nextval = row
            .cells
            .iter()
            .any(|(_, v)| matches!(v, Value::NextVal(..)));
        let groupable = self.options.noise.insert_all
            && plants.is_empty()
            && fk_findings.is_empty()
            && row.column_list
            && row.table_name.is_none()
            && !row.drop_last_value
            && !has_nextval
            && !refs.iter().any(|(_, target)| matches!(target, Target::Bad));
        let at = self.write_row(row, groupable)?;
        let before = self.findings.len();
        for plant in plants.into_iter().chain(fk_findings) {
            let key = if plant.key_from_row {
                let columns: Vec<&str> = plant.columns.iter().map(String::as_str).collect();
                row.key(&columns, self.rules)
            } else {
                plant.key
            };
            self.findings.push(Finding {
                kind: plant.kind,
                constraint: plant.constraint,
                table: plant
                    .table
                    .unwrap_or_else(|| self.table.qualified(self.rules)),
                columns: plant.columns,
                file: Some(at.file.clone()),
                line: Some(at.line),
                key,
                first: plant.first,
                count: None,
                assumed: false,
            });
        }
        if record && let Some(found) = self.findings.get(before..) {
            record_fix(&mut self.fixes, self.table, &at, found);
        }
        Ok(Some(at))
    }

    fn count_empty_parent(&mut self, fk: &'static Constraint) {
        if let Some((_, n)) = self
            .empty_parents
            .iter_mut()
            .find(|(c, _)| c.name == fk.name)
        {
            *n += 1;
        } else {
            self.empty_parents.push((fk, 1));
        }
    }

    fn write_row(&mut self, row: &Row, groupable: bool) -> io::Result<Location> {
        let eol = self.eol;
        let rules = self.rules;
        let case = if self.options.noise.mixed_case {
            self.style_rng.below(3)
        } else {
            0
        };
        let named = match row.table_name {
            Some(name) => rules.qualified(self.table.schema(), name),
            None => self.table.qualified(rules),
        };
        let table = cased(rules, &named, case);
        let wrap = self.options.noise.wrapped_statements && self.style_rng.below(2) == 0;
        let columns: Option<Vec<String>> = row.column_list.then(|| {
            row.cells
                .iter()
                .map(|(c, _)| cased(rules, c, case))
                .collect()
        });
        let mut values: Vec<String> = row
            .cells
            .iter()
            .map(|(_, v)| self.render.literal(v, &mut self.style_rng))
            .collect();
        if row.drop_last_value {
            values.pop();
        }
        let insert = Insert {
            table: &table,
            columns: columns.as_deref(),
            values: &values,
            case,
            wrap,
            eol,
        };
        if self.group_left == 0 {
            self.next_part_if_full()?;
        }
        self.parts.entry(self.base_path.clone()).or_insert((1, 0)).1 += 1;
        let at = if groupable {
            if self.group_left == 0 {
                self.group_left = 2 + self.style_rng.below(3);
                self.write_data(&self.render.batch(Batch::Start, eol), 0)?;
            }
            let at = self.write_data(&self.render.batch(Batch::Row(&insert), eol), 0)?;
            self.group_left -= 1;
            if self.group_left == 0 {
                self.write_data(&self.render.batch(Batch::End, eol), 1)?;
            }
            at
        } else {
            self.close_group()?;
            self.write_data(&self.render.insert(&insert), 1)?
        };
        // The reference schema's files are checked as reference files, which need no commit.
        if self.table.schema() != REF_SCHEMA {
            self.open
                .entry(self.data_path.clone())
                .or_insert_with(|| OpenRows {
                    at: at.clone(),
                    table: named,
                    rows: 0,
                })
                .rows += 1;
        }
        Ok(at)
    }

    fn finish(mut self) -> io::Result<Written> {
        for (_, open) in std::mem::take(&mut self.open) {
            self.findings.push(Finding {
                kind: FindingKind::Uncommitted,
                constraint: None,
                table: open.table,
                columns: Vec::new(),
                file: Some(open.at.file),
                line: Some(open.at.line),
                key: Vec::new(),
                first: None,
                count: Some(open.rows),
                assumed: false,
            });
        }
        for (fk, count) in std::mem::take(&mut self.empty_parents) {
            self.findings.push(Finding {
                kind: FindingKind::ParentTableEmpty,
                constraint: Some(fk.qualified(self.rules)),
                table: fk.table.qualified(self.rules),
                columns: fk.columns.iter().map(|c| (*c).to_owned()).collect(),
                file: None,
                line: None,
                key: Vec::new(),
                first: None,
                count: Some(count),
                assumed: false,
            });
        }
        let cascades = self.cascade_fixes();
        let files = self.out.finish()?;
        Ok(Written {
            files,
            findings: self.findings,
            cascades,
            fixes: self.fixes,
        })
    }

    /// The cascade rows below the roots, and what the fix does to them. A root's own finding decides its fix.
    fn cascade_fixes(&mut self) -> Vec<Cascade> {
        let rows = std::mem::take(&mut self.cascade_rows);
        let mut cascades = Vec::new();
        for (&node, (at, link)) in &rows {
            let Some((fk, parent)) = link else {
                continue;
            };
            let Some((parent, _)) = rows.get(parent) else {
                continue;
            };
            cascades.push(Cascade {
                file: at.file.clone(),
                line: at.line,
                constraint: fk.qualified(self.rules),
                parent: parent.clone(),
            });
            self.fixes.removed.push(at.clone());
            match fate_on_fk_null(&rows, node) {
                Fate::Removed => self.fixes.removed_on_fk_null.push(at.clone()),
                Fate::Nulled => self.fixes.nulled_on_fk_null.push(Nulled {
                    file: at.file.clone(),
                    line: at.line,
                    columns: sorted(fk.columns.iter().map(|c| (*c).to_owned())),
                }),
                Fate::Kept => {}
            }
        }
        cascades
    }

    /// Kinds planted at normal row `index` of `n`: each kind once at a fixed position, plus one at random.
    fn plants_at<K: Copy + PartialEq>(&mut self, kinds: &[K], index: u64, n: u64) -> Vec<K> {
        let len = u64::try_from(kinds.len()).unwrap_or(0);
        let mut at: Vec<K> = (0..len)
            .zip(kinds)
            .filter(|(k, _)| (k + 1) * n / (len + 1) == index)
            .map(|(_, kind)| *kind)
            .collect();
        if self.rng.chance_ppm(self.options.violations_ppm)
            && let Some(kind) = self.rng.pick(kinds)
            && !at.contains(kind)
        {
            at.push(*kind);
        }
        at
    }
}

/// What `--on-fk null` does to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Removed,
    Nulled,
    Kept,
}

fn sorted(columns: impl Iterator<Item = String>) -> Vec<String> {
    let mut columns: Vec<String> = columns.collect();
    columns.sort();
    columns.dedup();
    columns
}

/// Adds what the default policy and `--on-fk null` do to a row with `findings`, under keep-first: a row with a
/// finding the fix acts on goes, unless every such finding is a missing parent whose columns can be NULL.
fn record_fix(fixes: &mut Fixes, table: TableId, at: &Location, findings: &[Finding]) {
    let acted: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                FindingKind::FkMissingParent | FindingKind::DuplicateKey | FindingKind::NotNull
            )
        })
        .collect();
    if acted.is_empty() {
        return;
    }
    fixes.removed.push(at.clone());
    let columns = sorted(acted.iter().flat_map(|f| f.columns.iter().cloned()));
    if acted.iter().all(|f| f.kind == FindingKind::FkMissingParent)
        && columns.iter().all(|c| schema::can_be_null(table, c))
    {
        fixes.nulled_on_fk_null.push(Nulled {
            file: at.file.clone(),
            line: at.line,
            columns,
        });
    } else {
        fixes.removed_on_fk_null.push(at.clone());
    }
}

/// A cascade row's fate under `--on-fk null`: the root goes; a row whose parent goes loses its foreign key to NULL
/// when it can, else goes too; a row whose parent stays stays.
fn fate_on_fk_null(rows: &CascadeRows, at: Node) -> Fate {
    let Some((_, Some((fk, parent)))) = rows.get(&at) else {
        return Fate::Removed;
    };
    match fate_on_fk_null(rows, *parent) {
        Fate::Removed if fk.columns.iter().all(|c| schema::can_be_null(fk.table, c)) => {
            Fate::Nulled
        }
        Fate::Removed => Fate::Removed,
        Fate::Nulled | Fate::Kept => Fate::Kept,
    }
}

/// An identifier written in one of three cases: as the dialect stores it, in the other case, or quoted. A name
/// that needs quotes stays as it is.
fn cased(rules: &Rules, name: &str, case: u64) -> String {
    let q = rules.ident_quote;
    name.split('.')
        .map(|part| {
            let shown = rules.display_ident(part);
            if part.starts_with(q) {
                part.to_owned()
            } else if shown != part {
                // A name that needs quotes under the dialect's folding, such as `EMP_ID` where names fold to lower case.
                shown.into_owned()
            } else {
                match (case, rules.fold) {
                    (1, Fold::Upper) => part.to_ascii_lowercase(),
                    (1, Fold::Lower) => part.to_ascii_uppercase(),
                    (2, _) => format!("{q}{part}{q}"),
                    _ => part.to_owned(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

const FIRST_NAMES: [&str; 10] = [
    "Matti", "Liisa", "Jussi", "Aino", "Pekka", "Sanna", "Jörg", "Céline", "José", "Åsa",
];
const LAST_NAMES: [&str; 10] = [
    "Mäkinen", "Virtanen", "Öhman", "Åström", "O'Brien", "Korhonen", "Müller", "D'Angelo",
    "Nieminen", "Lehtonen",
];
const COUNTRIES: [(&str, &str); 10] = [
    ("FI", "Finland"),
    ("SE", "Sweden"),
    ("NO", "Norway"),
    ("DK", "Denmark"),
    ("EE", "Estonia"),
    ("DE", "Germany"),
    ("FR", "France"),
    ("ES", "Spain"),
    ("GB", "United Kingdom"),
    ("US", "United States"),
];
const DEPT_NAMES: [&str; 6] = [
    "R&D",
    "Myynti & markkinointi",
    "Hallinto",
    "Logistiikka",
    "Tuotekehitys",
    "Asiakaspalvelu",
];
const HOURS: [&str; 5] = ["7.5", "8", "4.25", "6", "7.75"];

fn pick_str<const N: usize>(items: &[&'static str; N], i: u64) -> &'static str {
    let n = u64::try_from(N).unwrap_or(1);
    usize::try_from(i % n)
        .ok()
        .and_then(|i| items.get(i))
        .copied()
        .unwrap_or("")
}

fn int(n: u64) -> Value {
    Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

fn dept_id(d: u64) -> u64 {
    10 * (d + 1)
}
fn dept_name(d: u64) -> String {
    format!("{} {d}", pick_str(&DEPT_NAMES, d))
}
fn emp_id(e: u64) -> u64 {
    1000 + e
}
fn email(e: u64) -> String {
    format!("user{e}@example.com")
}
fn project_code(p: u64) -> String {
    format!("P{p:05}")
}

fn day(rng: &mut Rng, from_year: i64, span_days: u64) -> Value {
    Value::Date(days_from_civil(from_year, 1, 1) + i64::try_from(rng.below(span_days)).unwrap_or(0))
}

fn timestamp(rng: &mut Rng) -> Value {
    let days = days_from_civil(2000, 1, 1) + i64::try_from(rng.below(9000)).unwrap_or(0);
    Value::Timestamp(days * 86_400 + i64::try_from(rng.below(86_400)).unwrap_or(0))
}

fn countries(w: &mut Writer<'_>) -> io::Result<()> {
    for (i, (code, name)) in (0..).zip(COUNTRIES) {
        let row = Row::new(vec![
            ("COUNTRY_CODE", Value::Char(code.into())),
            ("NAME", Value::text(name)),
        ]);
        w.normal(i, &row, &[])?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum DeptPlant {
    MissingCountry,
    MissingManager,
    DuplicateName,
    DuplicateId,
    NullId,
}

fn departments(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    let mut kinds = vec![
        DeptPlant::MissingCountry,
        DeptPlant::DuplicateName,
        DeptPlant::DuplicateId,
        DeptPlant::NullId,
    ];
    if w.options.deferred_violations && w.deferred(&schema::DEPARTMENTS_MANAGER_FK) {
        kinds.push(DeptPlant::MissingManager);
    }
    let refs = |manager: Target, country: Target| {
        [
            (&schema::DEPARTMENTS_MANAGER_FK, manager),
            (&schema::DEPARTMENTS_COUNTRY_FK, country),
        ]
    };
    for d in 0..s.departments {
        let plants = w.plants_at(&kinds, d, s.departments);
        let manager = (d * 7) % s.employees;
        let country = d % 10;
        let code = pick_str(&COUNTRIES.map(|c| c.0), country);
        let row = Row::new(vec![
            ("DEPT_ID", int(dept_id(d))),
            ("NAME", Value::text(dept_name(d))),
            (
                "MANAGER_ID",
                if d % 4 == 3 {
                    Value::Null
                } else {
                    int(emp_id(manager))
                },
            ),
            (
                "COUNTRY_CODE",
                match d % 5 {
                    4 => Value::Null,
                    1 => Value::Char(format!("{code} ")),
                    _ => Value::Char(code.into()),
                },
            ),
        ]);
        let first = w.normal(d, &row, &refs(Target::Row(manager), Target::Row(country)))?;
        for plant in plants {
            let k = w.next_extra();
            let mut cells = vec![
                ("DEPT_ID", int(s.extra_dept_id(k))),
                ("NAME", Value::text(format!("Extra dept {k}"))),
                ("MANAGER_ID", Value::Null),
                ("COUNTRY_CODE", Value::Null),
            ];
            match plant {
                DeptPlant::MissingCountry => {
                    set(&mut cells, "COUNTRY_CODE", Value::Char("XX".into()));
                }
                DeptPlant::MissingManager => set(&mut cells, "MANAGER_ID", missing_id(k)),
                DeptPlant::DuplicateName => set(&mut cells, "NAME", Value::text(dept_name(d))),
                DeptPlant::DuplicateId => set(&mut cells, "DEPT_ID", int(dept_id(d))),
                DeptPlant::NullId => set(&mut cells, "DEPT_ID", Value::Null),
            }
            let row = Row::new(cells);
            let found = match plant {
                DeptPlant::MissingCountry | DeptPlant::MissingManager => Vec::new(),
                DeptPlant::DuplicateName => {
                    vec![Plant::duplicate(
                        schema::DEPARTMENTS_NAME_UK.qualified(w.rules),
                        &["NAME"],
                        first.as_ref(),
                    )]
                }
                DeptPlant::DuplicateId => {
                    vec![Plant::duplicate(
                        schema::DEPARTMENTS_PK.qualified(w.rules),
                        &["DEPT_ID"],
                        first.as_ref(),
                    )]
                }
                DeptPlant::NullId => vec![Plant::not_null(
                    "DEPT_ID",
                    Some(schema::DEPARTMENTS_PK.qualified(w.rules)),
                )],
            };
            // An extra's FK columns are NULL unless planted with a value no parent has.
            w.extra(&row, &refs(Target::Missing, Target::Missing), found)?;
        }
        // An odd tree's root is a department with no name.
        for tree in w.trees_at(d, s.departments) {
            if tree % 2 == 1 {
                let row = Row::new(vec![
                    ("DEPT_ID", int(s.cascade_dept_id(tree))),
                    ("NAME", Value::Null),
                    ("MANAGER_ID", Value::Null),
                    ("COUNTRY_CODE", Value::Null),
                ]);
                let at = node(tree, TableId::Departments, 0);
                w.cascade(at, None, &row, &[], vec![Plant::not_null("NAME", None)])?;
            }
        }
    }
    Ok(())
}

fn set(cells: &mut [(&'static str, Value)], column: &str, value: Value) {
    if let Some((_, v)) = cells.iter_mut().find(|(c, _)| *c == column) {
        *v = value;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum EmpPlant {
    MissingDept,
    MissingManager,
    DuplicateId,
    DuplicateEmail,
    NullEmail,
    EmptyEmail,
    UnknownColumn,
    ValueCount,
    UnknownTable,
    /// An extra employee whose manager is written after it: valid in the end state, a load-order finding while the
    /// FK is enforced.
    ForwardManager,
}

const EMP_PLANTS: [EmpPlant; 10] = [
    EmpPlant::MissingDept,
    EmpPlant::MissingManager,
    EmpPlant::DuplicateId,
    EmpPlant::DuplicateEmail,
    EmpPlant::NullEmail,
    EmpPlant::EmptyEmail,
    EmpPlant::UnknownColumn,
    EmpPlant::ValueCount,
    EmpPlant::UnknownTable,
    EmpPlant::ForwardManager,
];

fn employee_row(w: &mut Writer<'_>, s: &Sizes, e: u64, manager: Option<u64>) -> (Row, u64) {
    let dept = e % s.departments;
    let first = pick_str(&FIRST_NAMES, e);
    let last = pick_str(&LAST_NAMES, e / 10);
    let hire = day(&mut w.rng, 1995, 11_000);
    let created = timestamp(&mut w.rng);
    let full_row = w.options.noise.no_column_list && e % 7 == 3;
    let mut cells = vec![
        ("EMP_ID", int(emp_id(e))),
        ("FIRST_NAME", Value::text(first)),
        ("LAST_NAME", Value::text(last)),
        ("EMAIL", Value::text(email(e))),
        (
            "DEPT_ID",
            if e % 9 == 8 {
                Value::Null
            } else {
                int(dept_id(dept))
            },
        ),
        (
            "MANAGER_ID",
            manager.map_or(Value::Null, |m| int(emp_id(m))),
        ),
        ("HIRE_DATE", hire),
        (
            "STATUS",
            Value::text(if e % 10 == 9 { "LEFT" } else { "ACTIVE" }),
        ),
        ("CREATED_AT", if e % 6 == 2 { Value::Null } else { created }),
        (
            "PHONE",
            if e.is_multiple_of(3) {
                Value::Null
            } else {
                Value::text(format!("+358 40 {:07}", e % 10_000_000))
            },
        ),
    ];
    if !full_row {
        if e % 4 == 1 {
            cells.retain(|(c, _)| *c != "STATUS");
        }
        if e.is_multiple_of(3) {
            cells.retain(|(c, _)| *c != "PHONE");
        }
    }
    (
        Row {
            column_list: !full_row,
            ..Row::new(cells)
        },
        dept,
    )
}

fn employees(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    // `''` violates NOT NULL only where the dialect stores it as NULL.
    let kinds: Vec<EmpPlant> = EMP_PLANTS
        .into_iter()
        .filter(|p| *p != EmpPlant::EmptyEmail || w.rules.empty_string_is_null)
        .collect();
    for e in 0..s.employees {
        let mut plants = w.plants_at(&kinds, e, s.employees);
        // A forward reference needs a later employee, so it is always planted at the middle one as well.
        if e == s.employees / 2 && !plants.contains(&EmpPlant::ForwardManager) {
            plants.push(EmpPlant::ForwardManager);
        }
        if e + 1 >= s.employees {
            plants.retain(|p| *p != EmpPlant::ForwardManager);
        }
        let manager = match e {
            0 => None,
            _ if e % 13 == 5 => Some(e),
            _ => Some(e / 3),
        };
        let (row, dept) = employee_row(w, s, e, manager);
        let refs = [
            (&schema::EMPLOYEES_DEPT_FK, Target::Row(dept)),
            (
                &schema::EMPLOYEES_MANAGER_FK,
                Target::Row(manager.unwrap_or(e)),
            ),
        ];
        let first = w.normal(e, &row, &refs)?;
        for plant in plants {
            employee_extra(w, s, e, plant, first.as_ref())?;
        }
        for tree in w.trees_at(e, s.employees) {
            let employee = cascade_employee(w, s, tree)?;
            shifted_duplicates(w, s, tree, employee.as_ref())?;
        }
    }
    Ok(())
}

/// A tree's employee `i` with `email`, the rest of the row as every such employee has it.
fn tree_employee(s: &Sizes, tree: u64, i: u64, email: Value, dept: Value, manager: Value) -> Row {
    Row::new(vec![
        ("EMP_ID", int(s.tree_emp_id(tree, i))),
        ("FIRST_NAME", Value::text("Cascade")),
        ("LAST_NAME", Value::text(format!("Row {tree}.{i}"))),
        ("EMAIL", email),
        ("DEPT_ID", dept),
        ("MANAGER_ID", manager),
        ("HIRE_DATE", Value::Date(days_from_civil(2015, 1, 1))),
        ("STATUS", Value::text("ACTIVE")),
    ])
}

/// A cascade tree's employee, an even tree's root with no email or in an odd tree a child of its department, then
/// the chain of employees it manages. Returns the odd tree's employee's place.
fn cascade_employee(w: &mut Writer<'_>, s: &Sizes, tree: u64) -> io::Result<Option<Location>> {
    let at = node(tree, TableId::Employees, 0);
    let employee = if tree.is_multiple_of(2) {
        let row = tree_employee(s, tree, 0, Value::Null, Value::Null, Value::Null);
        w.cascade(at, None, &row, &[], vec![Plant::not_null("EMAIL", None)])?;
        None
    } else {
        let email = Value::text(format!("cascade{tree}@example.com"));
        let row = tree_employee(s, tree, 0, email, int(s.cascade_dept_id(tree)), Value::Null);
        let dept = (
            &schema::EMPLOYEES_DEPT_FK,
            node(tree, TableId::Departments, 0),
        );
        w.cascade(at, Some(dept), &row, &[], Vec::new())?
    };
    for i in 1..=s.chain {
        let email = Value::text(format!("cascade{tree}.{i}@example.com"));
        let manager = int(s.tree_emp_id(tree, i - 1));
        let row = tree_employee(s, tree, i, email, Value::Null, manager);
        let parent = (
            &schema::EMPLOYEES_MANAGER_FK,
            node(tree, TableId::Employees, i - 1),
        );
        w.cascade(
            node(tree, TableId::Employees, i),
            Some(parent),
            &row,
            &[],
            Vec::new(),
        )?;
    }
    Ok(employee)
}

/// Duplicates whose key's first row is bad, so the database rejects it and accepts the next: after a row with a
/// missing department, which `--on-fk null` keeps instead; after a row with no last name, with a third row that
/// stays a duplicate; and in an odd tree after its employee, which goes a round after its department.
fn shifted_duplicates(
    w: &mut Writer<'_>,
    s: &Sizes,
    tree: u64,
    employee: Option<&Location>,
) -> io::Result<()> {
    let id = |i: u64| 1 + s.chain + i;
    let email = |group: u64| Value::text(format!("shift{tree}.{group}@example.com"));
    let duplicate = |first: Option<&Location>| {
        Plant::duplicate(
            schema::EMPLOYEES_EMAIL_UIX.qualified(w.rules),
            &["EMAIL"],
            first,
        )
    };
    let no_dept = tree_employee(s, tree, id(0), email(0), missing_id(tree + 1), Value::Null);
    let refs = [(&schema::EMPLOYEES_DEPT_FK, Target::Missing)];
    let first = w.extra(&no_dept, &refs, Vec::new())?;
    let second = tree_employee(s, tree, id(1), email(0), Value::Null, Value::Null);
    w.second_holder(&second, duplicate(first.as_ref()), true)?;

    let mut no_name = tree_employee(s, tree, id(2), email(1), Value::Null, Value::Null);
    set(&mut no_name.cells, "LAST_NAME", Value::Null);
    let first = w.extra(&no_name, &[], vec![Plant::not_null("LAST_NAME", None)])?;
    let second = tree_employee(s, tree, id(3), email(1), Value::Null, Value::Null);
    w.second_holder(&second, duplicate(first.as_ref()), false)?;
    let third = tree_employee(s, tree, id(4), email(1), Value::Null, Value::Null);
    w.extra(&third, &[], vec![duplicate(first.as_ref())])?;

    if let Some(employee) = employee {
        let email = Value::text(format!("cascade{tree}@example.com"));
        let second = tree_employee(s, tree, id(5), email, Value::Null, Value::Null);
        w.second_holder(&second, duplicate(Some(employee)), true)?;
    }
    Ok(())
}

fn employee_extra(
    w: &mut Writer<'_>,
    s: &Sizes,
    e: u64,
    plant: EmpPlant,
    first: Option<&Location>,
) -> io::Result<()> {
    let k = w.next_extra();
    let mut row = Row::new(vec![
        ("EMP_ID", int(s.extra_emp_id(k))),
        ("FIRST_NAME", Value::text("Extra")),
        ("LAST_NAME", Value::text(format!("Row {k}"))),
        ("EMAIL", Value::text(format!("extra{k}@example.com"))),
        ("DEPT_ID", Value::Null),
        ("MANAGER_ID", Value::Null),
        ("HIRE_DATE", day(&mut w.rng, 2010, 3000)),
        ("STATUS", Value::text("ACTIVE")),
    ]);
    let cells = &mut row.cells;
    let mut found = Vec::new();
    match plant {
        EmpPlant::MissingDept => set(cells, "DEPT_ID", missing_id(k)),
        EmpPlant::MissingManager => set(cells, "MANAGER_ID", missing_id(k)),
        EmpPlant::ForwardManager => set(cells, "MANAGER_ID", int(emp_id(e + 1))),
        EmpPlant::DuplicateId => set(cells, "EMP_ID", int(emp_id(e))),
        EmpPlant::DuplicateEmail => set(cells, "EMAIL", Value::text(email(e))),
        EmpPlant::NullEmail => set(cells, "EMAIL", Value::Null),
        EmpPlant::EmptyEmail => set(cells, "EMAIL", Value::text("")),
        EmpPlant::UnknownColumn => {
            if let Some(cell) = cells.iter_mut().find(|(c, _)| *c == "EMAIL") {
                cell.0 = "EMAIL_ADDR";
            }
            found.push(Plant::new(
                FindingKind::UnknownColumn,
                None,
                &["EMAIL_ADDR"],
            ));
        }
        EmpPlant::ValueCount => {
            row.drop_last_value = true;
            found.push(Plant::new(FindingKind::ValueCountMismatch, None, &[]));
        }
        EmpPlant::UnknownTable => {
            row.table_name = Some("EMPLOYEES_ARCHIVE");
            found.push(Plant {
                table: Some(w.rules.qualified(APP_SCHEMA, "EMPLOYEES_ARCHIVE")),
                ..Plant::new(FindingKind::UnknownTable, None, &[])
            });
        }
    }
    match plant {
        EmpPlant::DuplicateId => found.push(Plant::duplicate(
            schema::EMPLOYEES_PK.qualified(w.rules),
            &["EMP_ID"],
            first,
        )),
        EmpPlant::DuplicateEmail => {
            found.push(Plant::duplicate(
                schema::EMPLOYEES_EMAIL_UIX.qualified(w.rules),
                &["EMAIL"],
                first,
            ));
        }
        EmpPlant::NullEmail | EmpPlant::EmptyEmail => found.push(Plant::not_null("EMAIL", None)),
        _ => {}
    }
    // The forward reference is valid in the end state, and a load-order finding while the FK is enforced.
    let manager = if plant == EmpPlant::ForwardManager {
        Target::Row(e + 1)
    } else {
        Target::Missing
    };
    let refs = [
        (&schema::EMPLOYEES_DEPT_FK, Target::Missing),
        (&schema::EMPLOYEES_MANAGER_FK, manager),
    ];
    w.extra(&row, &refs, found).map(drop)
}

#[derive(Clone, Copy, PartialEq)]
enum ProjectPlant {
    MissingDept,
    DuplicateCode,
    DuplicateExternalRef,
}

fn projects(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    let kinds = [
        ProjectPlant::MissingDept,
        ProjectPlant::DuplicateCode,
        ProjectPlant::DuplicateExternalRef,
    ];
    for p in 0..s.projects {
        let plants = w.plants_at(&kinds, p, s.projects);
        let dept = p % s.departments;
        let name = match p % 4 {
            1 => format!("Customer's portal {p}"),
            2 => format!("Varasto € {p}"),
            _ => format!("Projekti {p}"),
        };
        let external = p.is_multiple_of(2) || plants.contains(&ProjectPlant::DuplicateExternalRef);
        let row = Row::new(vec![
            ("PROJECT_CODE", Value::text(project_code(p))),
            ("NAME", Value::text(name)),
            (
                "DEPT_NAME",
                if p % 6 == 5 {
                    Value::Null
                } else {
                    Value::text(dept_name(dept))
                },
            ),
            (
                "EXTERNAL_REF",
                if external {
                    Value::text(format!("EXT-{p}"))
                } else {
                    Value::Null
                },
            ),
            ("START_DATE", day(&mut w.rng, 2000, 9000)),
        ]);
        let first = w.normal(p, &row, &[(&schema::PROJECTS_DEPT_FK, Target::Row(dept))])?;
        for plant in plants {
            let k = w.next_extra();
            let mut cells = vec![
                ("PROJECT_CODE", Value::text(format!("X{k:05}"))),
                ("NAME", Value::text(format!("Extra project {k}"))),
                ("DEPT_NAME", Value::Null),
                ("EXTERNAL_REF", Value::Null),
            ];
            match plant {
                ProjectPlant::MissingDept => set(
                    &mut cells,
                    "DEPT_NAME",
                    Value::text(format!("Lakkautettu {k}")),
                ),
                ProjectPlant::DuplicateCode => {
                    set(&mut cells, "PROJECT_CODE", Value::text(project_code(p)));
                }
                ProjectPlant::DuplicateExternalRef => {
                    set(&mut cells, "EXTERNAL_REF", Value::text(format!("EXT-{p}")));
                }
            }
            let row = Row::new(cells);
            let found = match plant {
                ProjectPlant::MissingDept => Vec::new(),
                ProjectPlant::DuplicateCode => {
                    vec![Plant::duplicate(
                        schema::PROJECTS_PK.qualified(w.rules),
                        &["PROJECT_CODE"],
                        first.as_ref(),
                    )]
                }
                ProjectPlant::DuplicateExternalRef => {
                    vec![Plant::duplicate(
                        schema::PROJECTS_EXTREF_UK.qualified(w.rules),
                        &["EXTERNAL_REF"],
                        first.as_ref(),
                    )]
                }
            };
            w.extra(&row, &[(&schema::PROJECTS_DEPT_FK, Target::Missing)], found)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum AssignmentPlant {
    /// (employee, NULL) a second time: a duplicate where partially NULL unique keys conflict.
    DuplicateNullRole,
    DuplicateKey,
    MissingEmployee,
    MissingProject,
}

fn assignments(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    let kinds = [
        AssignmentPlant::DuplicateNullRole,
        AssignmentPlant::DuplicateKey,
        AssignmentPlant::MissingEmployee,
        AssignmentPlant::MissingProject,
    ];
    // A partially NULL unique key is a duplicate only where the dialect compares the non-NULL parts.
    let partial_conflict = w.rules.unique_nulls == UniqueNulls::PartialKeysConflict;
    let kinds: Vec<AssignmentPlant> = kinds
        .into_iter()
        .filter(|p| *p != AssignmentPlant::DuplicateNullRole || partial_conflict)
        .collect();
    let refs = |emp: Target, project: Target| {
        [
            (&schema::ASSIGNMENTS_EMP_FK, emp),
            (&schema::ASSIGNMENTS_PROJECT_FK, project),
        ]
    };
    for e in 0..s.employees {
        let plants = w.plants_at(&kinds, e, s.employees);
        let mut firsts = Vec::new();
        for j in 0..2 {
            let project = (e + j) % s.projects;
            let role = match j {
                0 if e.is_multiple_of(5) => Value::text("LEAD"),
                0 => Value::text("DEV"),
                _ if e.is_multiple_of(3)
                    || plants.contains(&AssignmentPlant::DuplicateNullRole) =>
                {
                    Value::Null
                }
                _ => Value::text("QA"),
            };
            let row = Row::new(vec![
                ("EMP_ID", int(emp_id(e))),
                ("PROJECT_CODE", Value::text(project_code(project))),
                ("ROLE", role),
                ("ASSIGNED_ON", day(&mut w.rng, 2000, 9000)),
            ]);
            let target = refs(Target::Row(e), Target::Row(project));
            firsts.push(w.normal(2 * e + j, &row, &target)?);
        }
        for plant in plants {
            let k = w.next_extra();
            let (emp, project, role) = match plant {
                AssignmentPlant::DuplicateNullRole => {
                    (Some(e), Some((e + 2) % s.projects), Value::Null)
                }
                AssignmentPlant::DuplicateKey => {
                    (Some(e), Some(e % s.projects), Value::text("OPS"))
                }
                AssignmentPlant::MissingEmployee => {
                    (None, Some(e % s.projects), Value::text("DEV"))
                }
                AssignmentPlant::MissingProject => (Some(e), None, Value::text(format!("R{k}"))),
            };
            let row = Row::new(vec![
                (
                    "EMP_ID",
                    emp.map_or_else(|| missing_id(k), |e| int(emp_id(e))),
                ),
                (
                    "PROJECT_CODE",
                    Value::text(project.map_or_else(|| format!("NOPE{k}"), project_code)),
                ),
                ("ROLE", role),
            ]);
            let first = |j: usize| firsts.get(j).and_then(Option::as_ref);
            let found = match plant {
                AssignmentPlant::DuplicateNullRole => {
                    vec![Plant::duplicate(
                        schema::ASSIGNMENTS_ROLE_UK.qualified(w.rules),
                        &["EMP_ID", "ROLE"],
                        first(1),
                    )]
                }
                AssignmentPlant::DuplicateKey => vec![Plant::duplicate(
                    schema::ASSIGNMENTS_PK.qualified(w.rules),
                    &["EMP_ID", "PROJECT_CODE"],
                    first(0),
                )],
                _ => Vec::new(),
            };
            let target = refs(
                emp.map_or(Target::Missing, Target::Row),
                project.map_or(Target::Missing, Target::Row),
            );
            w.extra(&row, &target, found)?;
        }
        for tree in w.trees_at(e, s.employees) {
            cascade_assignment(w, s, tree)?;
        }
    }
    Ok(())
}

/// A cascade tree's assignment, of its employee to the first project.
fn cascade_assignment(w: &mut Writer<'_>, s: &Sizes, tree: u64) -> io::Result<()> {
    let row = Row::new(vec![
        ("EMP_ID", int(s.tree_emp_id(tree, 0))),
        ("PROJECT_CODE", Value::text(project_code(0))),
        ("ROLE", Value::text("DEV")),
    ]);
    let employee = (
        &schema::ASSIGNMENTS_EMP_FK,
        node(tree, TableId::Employees, 0),
    );
    let project = [(&schema::ASSIGNMENTS_PROJECT_FK, Target::Row(0))];
    let at = node(tree, TableId::Assignments, 0);
    w.cascade(at, Some(employee), &row, &project, Vec::new())
        .map(drop)
}

#[derive(Clone, Copy, PartialEq)]
enum TimesheetPlant {
    MissingAssignment,
    NullHours,
    DuplicateId,
}

#[expect(
    clippy::many_single_char_names,
    reason = "e, j, a, k and s are the row-index names used across this file"
)]
fn timesheets(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    let kinds = [
        TimesheetPlant::MissingAssignment,
        TimesheetPlant::NullHours,
        TimesheetPlant::DuplicateId,
    ];
    let day0 = days_from_civil(2000, 1, 1);
    let mut ts = 0;
    let assignments = 2 * s.employees;
    for a in 0..assignments {
        let (e, j) = (a / 2, a % 2);
        let project = (e + j) % s.projects;
        let plants = w.plants_at(&kinds, a, assignments);
        let mut last = None;
        for _ in 0..2 + w.rng.below(2) {
            ts += 1;
            let row = Row::new(vec![
                ("TS_ID", int(ts)),
                ("EMP_ID", int(emp_id(e))),
                (
                    "PROJECT_CODE",
                    if ts % 11 == 7 {
                        Value::Null
                    } else {
                        Value::text(project_code(project))
                    },
                ),
                (
                    "WORK_DATE",
                    Value::Date(day0 + i64::try_from(ts % EXTRA_DAYS).unwrap_or(0)),
                ),
                ("HOURS", Value::Decimal(pick_str(&HOURS, ts).into())),
            ]);
            last = w.normal(
                a,
                &row,
                &[(&schema::TIMESHEETS_ASSIGNMENT_FK, Target::Row(a))],
            )?;
        }
        for plant in plants {
            let k = w.next_extra();
            let missing = plant == TimesheetPlant::MissingAssignment;
            let row = Row::new(vec![
                (
                    "TS_ID",
                    int(if plant == TimesheetPlant::DuplicateId {
                        ts
                    } else {
                        s.extra_ts_id(k)
                    }),
                ),
                ("EMP_ID", int(emp_id(e))),
                (
                    "PROJECT_CODE",
                    Value::text(project_code(if missing {
                        (e + 3) % s.projects
                    } else {
                        project
                    })),
                ),
                ("WORK_DATE", extra_work_date(k)),
                (
                    "HOURS",
                    if plant == TimesheetPlant::NullHours {
                        Value::Null
                    } else {
                        Value::Decimal("1".into())
                    },
                ),
            ]);
            let found = match plant {
                TimesheetPlant::MissingAssignment => Vec::new(),
                TimesheetPlant::NullHours => vec![Plant::not_null("HOURS", None)],
                TimesheetPlant::DuplicateId => {
                    vec![Plant::duplicate(
                        schema::TIMESHEETS_PK.qualified(w.rules),
                        &["TS_ID"],
                        last.as_ref(),
                    )]
                }
            };
            let target = if missing {
                Target::Missing
            } else {
                Target::Row(a)
            };
            w.extra(&row, &[(&schema::TIMESHEETS_ASSIGNMENT_FK, target)], found)?;
        }
        for tree in w.trees_at(a, assignments) {
            cascade_timesheet(w, s, tree)?;
        }
    }
    Ok(())
}

/// A cascade tree's timesheet, a child of its assignment.
fn cascade_timesheet(w: &mut Writer<'_>, s: &Sizes, tree: u64) -> io::Result<()> {
    let k = w.next_extra();
    let row = Row::new(vec![
        ("TS_ID", int(s.extra_ts_id(k))),
        ("EMP_ID", int(s.tree_emp_id(tree, 0))),
        ("PROJECT_CODE", Value::text(project_code(0))),
        ("WORK_DATE", extra_work_date(k)),
        ("HOURS", Value::Decimal("8".into())),
    ]);
    let assignment = (
        &schema::TIMESHEETS_ASSIGNMENT_FK,
        node(tree, TableId::Assignments, 0),
    );
    let at = node(tree, TableId::Timesheets, 0);
    w.cascade(at, Some(assignment), &row, &[], Vec::new())
        .map(drop)
}

fn legacy_notes(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    let multiline = w.options.noise.multiline_strings;
    for i in 0..s.notes {
        // Every third note points at an employee who doesn't exist; the foreign key is disabled.
        let (emp, target) = if i.is_multiple_of(3) {
            (missing_id(i + 1), Target::Missing)
        } else {
            (int(emp_id(i % s.employees)), Target::Row(i % s.employees))
        };
        let text = if multiline {
            format!("Rivi 1\nRivi 2: muistiinpano {i}")
        } else {
            format!("Muistiinpano {i}")
        };
        let row = Row::new(vec![
            ("NOTE_ID", int(i + 1)),
            ("EMP_ID", emp),
            ("NOTE_TEXT", Value::text(text)),
        ]);
        w.normal(i, &row, &[(&schema::LEGACY_NOTES_EMP_FK, target)])?;
    }
    Ok(())
}

fn audit_log(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    let nextval = w.options.noise.nextval;
    let delimiters = w.options.noise.embedded_delimiters;
    let log_id = |n: u64| {
        if nextval {
            Value::NextVal(APP_SCHEMA, AUDIT_SEQ)
        } else {
            int(n)
        }
    };
    for i in 0..s.audit {
        let plants = w.plants_at(&[()], i, s.audit);
        let emp = i % s.employees;
        let detail = if delimiters {
            "login; ok -- via SSO"
        } else {
            "login ok"
        };
        let row = Row::new(vec![
            ("LOG_ID", log_id(i + 1)),
            (
                "EMP_ID",
                if i % 7 == 6 {
                    Value::Null
                } else {
                    int(emp_id(emp))
                },
            ),
            ("LOGGED_AT", timestamp(&mut w.rng)),
            ("DETAIL", Value::text(detail)),
        ]);
        w.normal(i, &row, &[(&schema::AUDITLOG_EMP_FK, Target::Row(emp))])?;
        for () in plants {
            let k = w.next_extra();
            let row = Row::new(vec![
                ("LOG_ID", log_id(s.extra_log_id(k))),
                ("EMP_ID", missing_id(k)),
                ("LOGGED_AT", timestamp(&mut w.rng)),
                ("DETAIL", Value::text("orphan")),
            ]);
            w.extra(
                &row,
                &[(&schema::AUDITLOG_EMP_FK, Target::Missing)],
                Vec::new(),
            )?;
        }
        for tree in w.trees_at(i, s.audit) {
            let k = w.next_extra();
            let row = Row::new(vec![
                ("LOG_ID", log_id(s.extra_log_id(k))),
                ("EMP_ID", int(s.tree_emp_id(tree, 0))),
                (
                    "LOGGED_AT",
                    Value::Timestamp(days_from_civil(2015, 1, 1) * 86_400),
                ),
                ("DETAIL", Value::text("cascade")),
            ]);
            let employee = (&schema::AUDITLOG_EMP_FK, node(tree, TableId::Employees, 0));
            let at = node(tree, TableId::AuditLog, 0);
            w.cascade(at, Some(employee), &row, &[], Vec::new())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_timesheets_get_dates_of_their_own_up_to_oracles_last_day() {
        let day = |k| match extra_work_date(k) {
            Value::Date(day) => day,
            other => panic!("{other:?}"),
        };
        let first = days_from_civil(2000, 1, 1) + i64::try_from(EXTRA_DAYS).unwrap();
        assert_eq!(day(0), first);
        // A million cascade trees apart, as with `--rows 1000000 --violations 1`.
        assert_ne!(day(1), day(1_000_001));
        assert_eq!(
            day(EXTRA_DAY_SPAN - 1),
            first + i64::try_from(EXTRA_DAY_SPAN).unwrap() - 1
        );
        assert!(day(EXTRA_DAY_SPAN - 1) <= days_from_civil(9999, 12, 31));
    }

    #[test]
    fn extra_keys_stay_above_every_normal_key_at_any_size() {
        for rows in [super::super::MIN_ROWS, 1_000, 1_802_000, 20_000_000] {
            let s = Sizes::new(rows, super::super::MAX_MANAGER_CHAIN);
            assert!(s.extra_dept_id(1) > dept_id(s.departments - 1), "{rows}");
            assert!(s.extra_emp_id(1) > emp_id(s.employees - 1), "{rows}");
            assert!(s.extra_ts_id(1) > 3 * 2 * s.employees, "{rows}");
            assert!(s.extra_log_id(1) > s.audit, "{rows}");
            // Every extra fits NUMBER(9), even with an extra for every employee.
            assert!(s.extra_dept_id(s.employees) < 1_000_000_000, "{rows}");
            assert!(s.extra_emp_id(s.employees) < 1_000_000_000, "{rows}");
            // Cascade keys come after every extra's, and fit with a tree for every employee.
            let extras = |normal: u64| normal + EXTRA_KINDS - 1;
            assert!(
                s.cascade_dept_id(0) > s.extra_dept_id(extras(s.departments)),
                "{rows}"
            );
            assert!(
                s.tree_emp_id(0, 0) > s.extra_emp_id(extras(s.employees)),
                "{rows}"
            );
            // A tree's employees end before the next tree's begin.
            let last = s.chain + SHIFTED_ROWS;
            assert_eq!(s.tree_emp_id(0, last) + 1, s.tree_emp_id(1, 0), "{rows}");
            assert!(s.cascade_dept_id(s.employees + 2) < 1_000_000_000, "{rows}");
            assert!(
                s.tree_emp_id(s.employees + 2, last) < 1_000_000_000,
                "{rows}"
            );
        }
        assert_eq!(
            missing_id(4)
                .canonical(crate::dialect::DialectId::default().rules())
                .as_deref(),
            Some("-4")
        );
    }
}

#[cfg(test)]
mod casing_tests {
    use super::*;
    use crate::dialect::DialectId;

    #[test]
    fn identifiers_are_cased_and_quoted_by_the_dialect_rules() {
        let oracle = DialectId::Oracle.rules();
        assert_eq!(cased(oracle, "EMP_ID", 0), "EMP_ID");
        assert_eq!(cased(oracle, "EMP_ID", 1), "emp_id");
        assert_eq!(cased(oracle, "EMP_ID", 2), "\"EMP_ID\"");
        assert_eq!(cased(oracle, "APP.\"AuditLog\"", 1), "app.\"AuditLog\"");
        let lower = Rules {
            fold: Fold::Lower,
            ..*oracle
        };
        assert_eq!(cased(&lower, "EMP_ID", 0), "\"EMP_ID\"");
        assert_eq!(cased(&lower, "emp_id", 1), "EMP_ID");
    }
}
