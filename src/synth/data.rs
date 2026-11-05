//! The rows of each table, the planted findings, and the order everything is written in.

use std::io;
use std::path::Path;

use super::out::{Out, OutputFile, Role};
use super::rng::Rng;
use super::schema::{
    self, APP_SCHEMA, APP_TABLES, AUDIT_SEQ, Constraint, DdlStyle, Kind, REF_SCHEMA, REF_TABLES,
    State, TableId,
};
use super::value::{LiteralStyle, Value, days_from_civil};
use super::{Layout, Options, Placement};
use crate::finding::{Finding, FindingKind, Location, display_ident, qualified_name};

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

/// Writes the whole export; returns the files in load order and the findings they must produce.
pub(crate) fn write_export(
    options: &Options,
    root: &Path,
    omitted: &[TableId],
) -> io::Result<(Vec<OutputFile>, Vec<Finding>)> {
    let mut w = Writer::new(options, root, omitted);
    let sizes = Sizes::from_rows(options.rows);
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
                let text = w.style().late_keys(table);
                w.ddl(schema, Section::Constraints, table.name(), &text)?;
            }
            for &table in tables {
                let text = w.style().foreign_keys(table);
                w.ddl(schema, Section::RefConstraints, table.name(), &text)?;
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
}

impl Sizes {
    fn from_rows(rows: u64) -> Self {
        Self {
            employees: rows,
            departments: (rows / 20).max(5),
            projects: (rows / 10).max(5),
            notes: (rows / 50).max(3),
            audit: (rows / 10).max(3),
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
}

/// A foreign key value no row has: every key in the export is positive.
fn missing_id(k: u64) -> Value {
    Value::Int(-i64::try_from(k).unwrap_or(i64::MAX))
}

#[derive(Debug, Clone, Copy)]
enum Target {
    /// The parent is the normal row with this index.
    Row(TableId, u64),
    /// No parent row has this key.
    Missing,
}

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

    fn key(&self, columns: &[&str]) -> Vec<Option<String>> {
        columns
            .iter()
            .map(|c| self.get(c).and_then(Value::canonical))
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
            first: None,
        }
    }

    fn duplicate(
        constraint: String,
        columns: &[&str],
        row: &Row,
        first: Option<&Location>,
    ) -> Self {
        Self {
            key: row.key(columns),
            first: first.cloned(),
            ..Self::new(FindingKind::DuplicateKey, Some(constraint), columns)
        }
    }

    fn not_null(column: &'static str, constraint: Option<String>) -> Self {
        Self::new(FindingKind::NotNull, constraint, &[column])
    }
}

struct Writer<'o> {
    options: &'o Options,
    omitted: &'o [TableId],
    out: Out,
    eol: &'static str,
    literals: LiteralStyle,
    findings: Vec<Finding>,
    empty_parents: Vec<(&'static Constraint, u64)>,
    loaded: Vec<TableId>,
    table: TableId,
    data_path: String,
    /// Decides the data; kept apart from `style_rng` so the noise options don't change the rows.
    rng: Rng,
    style_rng: Rng,
    last_index: u64,
    extras: u64,
    group_left: u64,
}

impl<'o> Writer<'o> {
    fn new(options: &'o Options, root: &Path, omitted: &'o [TableId]) -> Self {
        let noise = options.noise;
        Self {
            options,
            omitted,
            out: Out::new(root, options.encoding),
            eol: options.line_ending.as_str(),
            literals: LiteralStyle {
                quoted_numbers: noise.quoted_numbers,
                numeric_forms: noise.numeric_forms,
                q_quote: noise.q_quote,
            },
            findings: Vec::new(),
            empty_parents: Vec::new(),
            loaded: Vec::new(),
            table: TableId::Countries,
            data_path: String::new(),
            rng: Rng::new(options.seed, 1),
            style_rng: Rng::new(options.seed, 2),
            last_index: 0,
            extras: 0,
            group_left: 0,
        }
    }

    fn style(&self) -> DdlStyle<'static> {
        DdlStyle {
            eol: self.eol,
            storage: self.options.noise.storage_clauses,
            comments: self.options.noise.comments,
            keys_in_table: self.options.constraints == Placement::BeforeData,
        }
    }

    fn ddl(&mut self, schema: &str, section: Section, object: &str, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let path = file_path(self.options.layout, schema, section, object);
        self.out
            .write(&path, schema, Role::Ddl, &format!("{text}{}", self.eol))?;
        Ok(())
    }

    /// Tables, the sequence and trigger, and (before the data) the foreign keys of one schema.
    fn schema_ddl(&mut self, schema: &str, tables: &[TableId]) -> io::Result<()> {
        let style = self.style();
        if schema == APP_SCHEMA && self.options.noise.nextval {
            self.ddl(schema, Section::Sequence, AUDIT_SEQ, &style.sequence())?;
        }
        for &table in tables {
            self.ddl(
                schema,
                Section::Table,
                table.name(),
                &style.create_table(table),
            )?;
        }
        if schema == APP_SCHEMA && self.options.noise.plsql_trigger {
            self.ddl(
                schema,
                Section::Trigger,
                schema::AUDIT_TRIGGER,
                &style.trigger(),
            )?;
        }
        if self.options.constraints == Placement::BeforeData {
            for &table in tables {
                self.ddl(
                    schema,
                    Section::RefConstraints,
                    table.name(),
                    &style.foreign_keys(table),
                )?;
            }
        }
        Ok(())
    }

    fn begin_table(&mut self, table: TableId) -> io::Result<()> {
        self.table = table;
        self.data_path = file_path(
            self.options.layout,
            table.schema(),
            Section::Data,
            table.name(),
        );
        self.rng = Rng::new(self.options.seed, 100 + table.stream());
        self.style_rng = Rng::new(self.options.seed, 200 + table.stream());
        self.last_index = 0;
        self.extras = 0;
        self.group_left = 0;
        if self.omitted.contains(&table) {
            return Ok(());
        }
        let eol = self.eol;
        let noise = self.options.noise;
        let name = table.qualified();
        let header: String = [
            noise
                .rem_header
                .then(|| format!("REM INSERTING into {name}{eol}")),
            noise
                .set_define_off
                .then(|| format!("SET DEFINE OFF;{eol}")),
            noise
                .comments
                .then(|| format!("/* Rows of {name}; a ';' or -- in here is not SQL */{eol}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !header.is_empty() {
            self.write_data(&header)?;
        }
        Ok(())
    }

    fn end_table(&mut self) -> io::Result<()> {
        if !self.omitted.contains(&self.table) {
            self.close_group()?;
            if self.options.noise.comments {
                let text = format!("-- end of {}{}", self.table.qualified(), self.eol);
                self.write_data(&text)?;
            }
        }
        self.loaded.push(self.table);
        Ok(())
    }

    fn write_data(&mut self, text: &str) -> io::Result<Location> {
        self.out
            .write(&self.data_path, self.table.schema(), Role::Data, text)
    }

    fn close_group(&mut self) -> io::Result<()> {
        if self.group_left > 0 {
            self.group_left = 0;
            let text = format!("SELECT 1 FROM DUAL;{}", self.eol);
            self.write_data(&text)?;
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

    /// Whether the end-state check applies: Oracle never validates rows already there against an `ENABLE
    /// NOVALIDATE` constraint added after them.
    fn checks_end_state(&self, fk: &Constraint) -> bool {
        match fk.state {
            State::Enable => true,
            State::EnableNovalidate => !self.after_data(),
            State::Disable => false,
        }
    }

    fn checks_load_order(&self, fk: &Constraint) -> bool {
        let deferred = matches!(fk.kind, Kind::Foreign { deferred: true, .. });
        !self.after_data() && fk.state != State::Disable && !deferred
    }

    fn parent_loaded(&self, parent: TableId, index: u64, current: u64) -> bool {
        if parent == self.table {
            index <= current
        } else {
            self.loaded.contains(&parent)
        }
    }

    /// A normal row: valid in the end state and, unless the data is shuffled, in load order too. Children reference
    /// it by index, so a finding here would leave Oracle rejecting rows the manifest calls valid.
    fn normal(
        &mut self,
        index: u64,
        row: &Row,
        refs: &[(&'static Constraint, Target)],
    ) -> io::Result<Option<Location>> {
        self.last_index = index;
        let before = self.findings.len();
        let at = self.emit(row, index, refs, Vec::new())?;
        if !self.options.shuffle_order
            && let Some(finding) = self.findings.get(before)
        {
            return Err(io::Error::other(format!(
                "generator bug: normal row {index} of {} has a finding: {finding:?}",
                self.table.qualified()
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
    ) -> io::Result<()> {
        self.emit(row, self.last_index, refs, plants).map(drop)
    }

    fn emit(
        &mut self,
        row: &Row,
        current: u64,
        refs: &[(&'static Constraint, Target)],
        plants: Vec<Plant>,
    ) -> io::Result<Option<Location>> {
        if self.omitted.contains(&self.table) {
            return Ok(None);
        }
        let mut fk_findings = Vec::new();
        for &(fk, target) in refs {
            let key = row.key(fk.columns);
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
                Target::Row(table, index)
                    if self.checks_load_order(fk) && !self.parent_loaded(table, index, current) =>
                {
                    FindingKind::FkLoadOrder
                }
                _ => continue,
            };
            fk_findings.push(Plant {
                key,
                ..Plant::new(kind, Some(fk.qualified()), fk.columns)
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
            && !has_nextval;
        let at = self.write_row(row, groupable)?;
        for plant in plants.into_iter().chain(fk_findings) {
            self.findings.push(Finding {
                kind: plant.kind,
                constraint: plant.constraint,
                table: plant.table.unwrap_or_else(|| self.table.qualified()),
                columns: plant.columns,
                file: Some(at.file.clone()),
                line: Some(at.line),
                key: plant.key,
                first: plant.first,
                count: None,
            });
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
        let case = if self.options.noise.mixed_case {
            self.style_rng.below(3)
        } else {
            0
        };
        let table = match row.table_name {
            Some(name) => qualified_name(self.table.schema(), name),
            None => self.table.qualified(),
        };
        let table = cased(&table, case);
        let wrap = self.options.noise.wrapped_statements && self.style_rng.below(2) == 0;
        let sep = if wrap {
            format!(",{eol}    ")
        } else {
            ",".to_owned()
        };
        let brk = if wrap { eol } else { " " };
        let columns = if row.column_list {
            let names: Vec<String> = row.cells.iter().map(|(c, _)| cased(c, case)).collect();
            format!(" ({})", names.join(&sep))
        } else {
            String::new()
        };
        let mut values: Vec<String> = row
            .cells
            .iter()
            .map(|(_, v)| self.literals.render(v, &mut self.style_rng))
            .collect();
        if row.drop_last_value {
            values.pop();
        }
        let values = values.join(&sep);
        if groupable {
            if self.group_left == 0 {
                self.group_left = 2 + self.style_rng.below(3);
                self.write_data(&format!("INSERT ALL{eol}"))?;
            }
            let at = self.write_data(&format!(
                "  INTO {table}{columns}{brk}VALUES ({values}){eol}"
            ))?;
            self.group_left -= 1;
            if self.group_left == 0 {
                self.write_data(&format!("SELECT 1 FROM DUAL;{eol}"))?;
            }
            Ok(at)
        } else {
            self.close_group()?;
            let (insert, keyword) = match case {
                1 => ("insert into", "values"),
                2 => ("INSERT INTO", "VALUES"),
                _ => ("Insert into", "values"),
            };
            self.write_data(&format!(
                "{insert} {table}{columns}{brk}{keyword} ({values});{eol}"
            ))
        }
    }

    fn finish(mut self) -> io::Result<(Vec<OutputFile>, Vec<Finding>)> {
        for (fk, count) in std::mem::take(&mut self.empty_parents) {
            self.findings.push(Finding {
                kind: FindingKind::ParentTableEmpty,
                constraint: Some(fk.qualified()),
                table: fk.table.qualified(),
                columns: fk.columns.iter().map(|c| (*c).to_owned()).collect(),
                file: None,
                line: None,
                key: Vec::new(),
                first: None,
                count: Some(count),
            });
        }
        let files = self.out.finish()?;
        Ok((files, self.findings))
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

/// An identifier written in one of three cases: as Oracle stores it, lower case, or quoted. A quoted
/// mixed-case name stays as it is.
fn cased(name: &str, case: u64) -> String {
    name.split('.')
        .map(|part| {
            if part.starts_with('"') || display_ident(part) != part {
                part.to_owned()
            } else {
                match case {
                    1 => part.to_ascii_lowercase(),
                    2 => format!("\"{part}\""),
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
    if w.options.deferred_violations {
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
        let first = w.normal(
            d,
            &row,
            &refs(
                Target::Row(TableId::Employees, manager),
                Target::Row(TableId::Countries, country),
            ),
        )?;
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
                        schema::DEPARTMENTS_NAME_UK.qualified(),
                        &["NAME"],
                        &row,
                        first.as_ref(),
                    )]
                }
                DeptPlant::DuplicateId => {
                    vec![Plant::duplicate(
                        schema::DEPARTMENTS_PK.qualified(),
                        &["DEPT_ID"],
                        &row,
                        first.as_ref(),
                    )]
                }
                DeptPlant::NullId => vec![Plant::not_null(
                    "DEPT_ID",
                    Some(schema::DEPARTMENTS_PK.qualified()),
                )],
            };
            // An extra's FK columns are NULL unless planted with a value no parent has.
            w.extra(&row, &refs(Target::Missing, Target::Missing), found)?;
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
    for e in 0..s.employees {
        let mut plants = w.plants_at(&EMP_PLANTS, e, s.employees);
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
            (
                &schema::EMPLOYEES_DEPT_FK,
                Target::Row(TableId::Departments, dept),
            ),
            (
                &schema::EMPLOYEES_MANAGER_FK,
                Target::Row(TableId::Employees, manager.unwrap_or(e)),
            ),
        ];
        let first = w.normal(e, &row, &refs)?;
        for plant in plants {
            employee_extra(w, s, e, plant, first.as_ref())?;
        }
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
    let mut cells = vec![
        ("EMP_ID", int(s.extra_emp_id(k))),
        ("FIRST_NAME", Value::text("Extra")),
        ("LAST_NAME", Value::text(format!("Row {k}"))),
        ("EMAIL", Value::text(format!("extra{k}@example.com"))),
        ("DEPT_ID", Value::Null),
        ("MANAGER_ID", Value::Null),
        ("HIRE_DATE", day(&mut w.rng, 2010, 3000)),
        ("STATUS", Value::text("ACTIVE")),
    ];
    let mut found = Vec::new();
    let mut row_shape = (true, None, false);
    match plant {
        EmpPlant::MissingDept => set(&mut cells, "DEPT_ID", missing_id(k)),
        EmpPlant::MissingManager => set(&mut cells, "MANAGER_ID", missing_id(k)),
        EmpPlant::ForwardManager => set(&mut cells, "MANAGER_ID", int(emp_id(e + 1))),
        EmpPlant::DuplicateId => set(&mut cells, "EMP_ID", int(emp_id(e))),
        EmpPlant::DuplicateEmail => set(&mut cells, "EMAIL", Value::text(email(e))),
        EmpPlant::NullEmail => set(&mut cells, "EMAIL", Value::Null),
        EmpPlant::EmptyEmail => set(&mut cells, "EMAIL", Value::text("")),
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
            row_shape.2 = true;
            found.push(Plant::new(FindingKind::ValueCountMismatch, None, &[]));
        }
        EmpPlant::UnknownTable => {
            row_shape.1 = Some("EMPLOYEES_ARCHIVE");
            found.push(Plant {
                table: Some(qualified_name(APP_SCHEMA, "EMPLOYEES_ARCHIVE")),
                ..Plant::new(FindingKind::UnknownTable, None, &[])
            });
        }
    }
    let row = Row {
        column_list: row_shape.0,
        table_name: row_shape.1,
        drop_last_value: row_shape.2,
        ..Row::new(cells)
    };
    match plant {
        EmpPlant::DuplicateId => found.push(Plant::duplicate(
            schema::EMPLOYEES_PK.qualified(),
            &["EMP_ID"],
            &row,
            first,
        )),
        EmpPlant::DuplicateEmail => {
            found.push(Plant::duplicate(
                schema::EMPLOYEES_EMAIL_UIX.qualified(),
                &["EMAIL"],
                &row,
                first,
            ));
        }
        EmpPlant::NullEmail | EmpPlant::EmptyEmail => found.push(Plant::not_null("EMAIL", None)),
        _ => {}
    }
    // The forward reference is valid in the end state, and a load-order finding while the FK is enforced.
    let manager = if plant == EmpPlant::ForwardManager {
        Target::Row(TableId::Employees, e + 1)
    } else {
        Target::Missing
    };
    let refs = [
        (&schema::EMPLOYEES_DEPT_FK, Target::Missing),
        (&schema::EMPLOYEES_MANAGER_FK, manager),
    ];
    w.extra(&row, &refs, found)
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
        let first = w.normal(
            p,
            &row,
            &[(
                &schema::PROJECTS_DEPT_FK,
                Target::Row(TableId::Departments, dept),
            )],
        )?;
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
                        schema::PROJECTS_PK.qualified(),
                        &["PROJECT_CODE"],
                        &row,
                        first.as_ref(),
                    )]
                }
                ProjectPlant::DuplicateExternalRef => {
                    vec![Plant::duplicate(
                        schema::PROJECTS_EXTREF_UK.qualified(),
                        &["EXTERNAL_REF"],
                        &row,
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
    /// (employee, NULL) a second time: a duplicate of the composite unique key in Oracle.
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
            let target = refs(
                Target::Row(TableId::Employees, e),
                Target::Row(TableId::Projects, project),
            );
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
                        schema::ASSIGNMENTS_ROLE_UK.qualified(),
                        &["EMP_ID", "ROLE"],
                        &row,
                        first(1),
                    )]
                }
                AssignmentPlant::DuplicateKey => vec![Plant::duplicate(
                    schema::ASSIGNMENTS_PK.qualified(),
                    &["EMP_ID", "PROJECT_CODE"],
                    &row,
                    first(0),
                )],
                _ => Vec::new(),
            };
            let target = refs(
                emp.map_or(Target::Missing, |e| Target::Row(TableId::Employees, e)),
                project.map_or(Target::Missing, |p| Target::Row(TableId::Projects, p)),
            );
            w.extra(&row, &target, found)?;
        }
    }
    Ok(())
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
                    Value::Date(day0 + i64::try_from(ts % 9000).unwrap_or(0)),
                ),
                ("HOURS", Value::Decimal(pick_str(&HOURS, ts).into())),
            ]);
            last = w.normal(
                a,
                &row,
                &[(
                    &schema::TIMESHEETS_ASSIGNMENT_FK,
                    Target::Row(TableId::Assignments, a),
                )],
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
                (
                    "WORK_DATE",
                    Value::Date(day0 + 9000 + i64::try_from(k % 500).unwrap_or(0)),
                ),
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
                        schema::TIMESHEETS_PK.qualified(),
                        &["TS_ID"],
                        &row,
                        last.as_ref(),
                    )]
                }
            };
            let target = if missing {
                Target::Missing
            } else {
                Target::Row(TableId::Assignments, a)
            };
            w.extra(&row, &[(&schema::TIMESHEETS_ASSIGNMENT_FK, target)], found)?;
        }
    }
    Ok(())
}

fn legacy_notes(w: &mut Writer<'_>, s: &Sizes) -> io::Result<()> {
    let multiline = w.options.noise.multiline_strings;
    for i in 0..s.notes {
        // Every third note points at an employee who doesn't exist; the foreign key is disabled.
        let (emp, target) = if i.is_multiple_of(3) {
            (missing_id(i + 1), Target::Missing)
        } else {
            (
                int(emp_id(i % s.employees)),
                Target::Row(TableId::Employees, i % s.employees),
            )
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
        w.normal(
            i,
            &row,
            &[(
                &schema::AUDITLOG_EMP_FK,
                Target::Row(TableId::Employees, emp),
            )],
        )?;
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_keys_stay_above_every_normal_key_at_any_size() {
        for rows in [super::super::MIN_ROWS, 1_000, 1_802_000, 20_000_000] {
            let s = Sizes::from_rows(rows);
            assert!(s.extra_dept_id(1) > dept_id(s.departments - 1), "{rows}");
            assert!(s.extra_emp_id(1) > emp_id(s.employees - 1), "{rows}");
            assert!(s.extra_ts_id(1) > 3 * 2 * s.employees, "{rows}");
            assert!(s.extra_log_id(1) > s.audit, "{rows}");
            // Every extra fits NUMBER(9), even with an extra for every employee.
            assert!(s.extra_dept_id(s.employees) < 1_000_000_000, "{rows}");
            assert!(s.extra_emp_id(s.employees) < 1_000_000_000, "{rows}");
        }
        assert_eq!(missing_id(4).canonical().as_deref(), Some("-4"));
    }
}
