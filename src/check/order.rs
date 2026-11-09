//! Data files in an order that loads every table's parent rows before the rows that reference them.

use std::collections::{BTreeMap, BTreeSet};

use super::{Observer, Options, Problem, Reading, Report, Schema, display, read, schema, total};
use crate::catalog::{self, Catalog, Kind, Lookup, Parent, TableId};
use crate::dialect::{Pass, Rules};
use crate::input::{InputError, InputFile, OrderSource, Role};
use crate::statement::Statement;

/// The inputs with their data files parents-first.
pub struct Ordered {
    pub files: Vec<InputFile>,
    /// A sentence for each data file still listed before a file holding rows its foreign keys point at: the files
    /// wait on each other, or table DDL between them or rows of one table in both keep them in order.
    pub unmet: Vec<String>,
    /// Files that couldn't be read to the end: the order is incomplete.
    pub unreadable: Vec<Problem>,
    /// Statements that couldn't be read, whose tables and keys the order can't use.
    pub errors: Vec<Problem>,
    /// DDL the catalog couldn't apply as written.
    pub catalog: Vec<Problem>,
}

/// Reorders the data files so that each comes after the data files holding rows of the tables its rows reference.
/// Schema and reference files come first, as given.
///
/// A data file with table DDL in it keeps its place, and no file moves across it: where a constraint is declared
/// decides which rows it judges. Only the files between two of them are reordered, and those that don't depend on
/// each other keep the order given. Files inserting into one table keep their order too, since the row that comes
/// first is the one a duplicate key keeps. A cycle is broken at the earliest file given in it.
///
/// Reads every data file. A foreign key orders files when it is enabled, isn't checked only at commit, and points at
/// another table. Every key still pointing at a later file is named in `unmet`.
pub fn dependency_order(
    files: &[InputFile],
    options: &Options,
    observer: &mut dyn Observer,
) -> Result<Ordered, InputError> {
    let schema = schema(files, options, observer)?;
    let catalog = schema.catalog();
    let rules = options.dialect.rules();
    let (tables, rows) = inserted(files, options, &schema, observer)?;
    let mut holders: BTreeMap<TableId, Vec<usize>> = BTreeMap::new();
    for (index, held) in tables.iter().enumerate() {
        for &table in held {
            holders.entry(table).or_default().push(index);
        }
    }
    let mut graph = Graph {
        needs: vec![BTreeMap::new(); files.len()],
        after: vec![BTreeSet::new(); files.len()],
    };
    for holding in holders.values() {
        for pair in holding.windows(2) {
            if let [earlier, later] = *pair
                && let Some(after) = graph.after.get_mut(later)
            {
                after.insert(earlier);
            }
        }
    }
    for (child, held) in tables.iter().enumerate() {
        for &table in held {
            for (key, parent) in ordering_keys(catalog, rules, table) {
                for &holder in holders.get(&parent).into_iter().flatten() {
                    if holder == child {
                        continue;
                    }
                    if let Some(need) = graph.needs.get_mut(child) {
                        need.entry(holder).or_default().insert(key.clone());
                    }
                }
            }
        }
    }

    let mut order = Vec::new();
    let mut run = Vec::new();
    for (index, file) in files.iter().enumerate() {
        if file.role != Role::Data {
            continue;
        }
        if schema.ddl.get(index).copied().unwrap_or(false) {
            order.extend(parents_first(&run, &graph));
            run.clear();
            order.push(index);
        } else {
            run.push(index);
        }
    }
    order.extend(parents_first(&run, &graph));

    let position: BTreeMap<usize, usize> = order.iter().enumerate().map(|(p, &i)| (i, p)).collect();
    let mut unmet = Vec::new();
    for &child in &order {
        for (parent, keys) in graph.needs.get(child).into_iter().flatten() {
            if position.get(parent) > position.get(&child) {
                unmet.push(format!(
                    "{} is listed before {}, which holds parent rows for {}: they wait on each other, or table DDL \
                     between them or rows of a table both hold keep them in order",
                    name(files, child),
                    name(files, *parent),
                    keys.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }

    let others = files.iter().filter(|f| f.role != Role::Data).cloned();
    let ordered = order
        .iter()
        .filter_map(|&i| files.get(i))
        .map(|f| InputFile {
            order: OrderSource::ParentsFirst,
            ..f.clone()
        });
    let report = &schema.report;
    Ok(Ordered {
        files: others.chain(ordered).collect(),
        unmet,
        unreadable: report
            .unreadable
            .iter()
            .chain(&rows.unreadable)
            .cloned()
            .collect(),
        errors: report.errors.iter().chain(&rows.errors).cloned().collect(),
        catalog: report.catalog.clone(),
    })
}

/// What each file waits on, by file.
struct Graph {
    /// The files holding its parents' rows, with the foreign keys that point there. A cycle may leave one unmet.
    needs: Vec<BTreeMap<usize, BTreeSet<String>>>,
    /// The file given before it that inserts into a table it inserts into: which of two rows of one key comes first
    /// decides which is the duplicate, so these keep their order, always.
    after: Vec<BTreeSet<usize>>,
}

impl Graph {
    fn waits_on(&self, file: usize) -> impl Iterator<Item = &usize> {
        let needs = self.needs.get(file).into_iter().flat_map(BTreeMap::keys);
        needs.chain(self.after.get(file).into_iter().flatten())
    }
}

/// The files of `run`, given in order, with each after the files of `run` it waits on. With none ready, a cycle of
/// foreign keys holds the rest: it is broken at the earliest file whose waiting parents all wait on it and whose
/// earlier files of the same tables are placed. The earliest file waiting is always one of those, as `after` points
/// only back in the order given.
fn parents_first(run: &[usize], graph: &Graph) -> Vec<usize> {
    let mut pending: BTreeSet<usize> = run.iter().copied().collect();
    let mut order = Vec::with_capacity(run.len());
    while !pending.is_empty() {
        let waits_on = |file: usize| graph.waits_on(file).filter(|p| pending.contains(p));
        let ready = pending
            .iter()
            .copied()
            .find(|&i| waits_on(i).next().is_none());
        let in_turn = |i: &usize| {
            let after = graph.after.get(*i).into_iter().flatten();
            after.copied().all(|p| !pending.contains(&p))
        };
        let in_cycle = |&i: &usize| waits_on(i).all(|&p| depends_on(graph, &pending, p, i));
        let next = ready
            .or_else(|| pending.iter().copied().filter(in_turn).find(in_cycle))
            .or_else(|| pending.first().copied());
        let Some(next) = next else {
            break;
        };
        pending.remove(&next);
        order.push(next);
    }
    order
}

/// Whether `file` waits, through the `pending` files, on `on`.
fn depends_on(graph: &Graph, pending: &BTreeSet<usize>, file: usize, on: usize) -> bool {
    let mut seen = BTreeSet::new();
    let mut next = vec![file];
    while let Some(current) = next.pop() {
        if current == on {
            return true;
        }
        if seen.insert(current) {
            next.extend(graph.waits_on(current).filter(|p| pending.contains(p)));
        }
    }
    false
}

/// By file: the tables a data file's rows go into, with what reading them reported. Reference files are read too, as
/// the check reads them, so that a problem in one is reported; their rows order nothing.
fn inserted(
    files: &[InputFile],
    options: &Options,
    schema: &Schema,
    observer: &mut dyn Observer,
) -> Result<(Vec<BTreeSet<TableId>>, Report), InputError> {
    let names: Vec<String> = files.iter().map(|f| display(&f.path)).collect();
    let reading = Reading {
        files,
        names: &names,
        options,
    };
    let in_rows_pass = |f: &InputFile| f.role != Role::Schema;
    let (count, bytes) = total(files, in_rows_pass);
    observer.pass(Pass::Rows, count, bytes);
    let mut report = Report::default();
    let mut tables = vec![BTreeSet::new(); files.len()];
    for unit in schema.plan.units() {
        if !files.get(unit.file).is_some_and(in_rows_pass) {
            continue;
        }
        read(
            &reading,
            &schema.plan,
            unit,
            Pass::Rows,
            &mut report,
            observer,
            |statement, origin| {
                let Statement::Insert(rows) = statement else {
                    return;
                };
                if !files.get(origin.file).is_some_and(|f| f.role == Role::Data) {
                    return;
                }
                let Some(held) = tables.get_mut(origin.file) else {
                    return;
                };
                for row in &rows {
                    if let Lookup::Found(table) = schema.catalog.resolve(&row.table, origin.file) {
                        held.insert(table);
                    }
                }
            },
        )?;
    }
    Ok((tables, report))
}

/// The foreign keys of `table` that order its rows after their parents', by name, with the parent table.
fn ordering_keys(catalog: &Catalog, rules: &Rules, table: TableId) -> Vec<(String, TableId)> {
    let Some(child) = catalog.table(table) else {
        return Vec::new();
    };
    child
        .constraints
        .iter()
        .filter(|c| c.state.enabled && !c.state.initially_deferred)
        .filter_map(|c| {
            let Kind::ForeignKey {
                parent: Parent::Key { table: parent, .. },
                ..
            } = &c.kind
            else {
                return None;
            };
            let key = match (&c.name, &child.name.schema) {
                (Some(name), Some(schema)) => rules.qualified(schema, name),
                (Some(name), None) => rules.display_ident(name).into_owned(),
                (None, _) => format!("a foreign key of {}", catalog::display(rules, &child.name)),
            };
            (*parent != table).then_some((key, *parent))
        })
        .collect()
}

fn name(files: &[InputFile], index: usize) -> String {
    files
        .get(index)
        .map(|f| display(&f.path))
        .unwrap_or_default()
}
