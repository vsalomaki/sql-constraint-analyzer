//! Oracle syntax for the generator: DDL as SQL Developer exports it, literals, `INSERT` and `INSERT ALL`, SQL\*Plus
//! headers and the `@@` load script.

macro_rules! put {
    ($out:expr, $($arg:tt)*) => {
        $out.push_str(&format!($($arg)*))
    };
}

use super::RULES;
use crate::synth::render::{Batch, DdlItem, DdlStyle, Edge, Insert, Sql};
use crate::synth::rng::Rng;
use crate::synth::schema::{
    APP_SCHEMA, AUDIT_SEQ, AUDIT_TRIGGER, Constraint, DefaultValue, EMPLOYEES_EMAIL_UIX, Form,
    Kind, NotNull, State, TableId, Ty, constraints_of,
};
use crate::synth::value::{Value, civil_from_days};
use crate::synth::{self, Noise};

pub(crate) struct Render {
    noise: Noise,
}

impl Render {
    pub(crate) fn new(noise: Noise) -> Self {
        Self { noise }
    }
}

/// The column and value lists of an insert, and what separates the statement's parts.
fn lists(insert: &Insert<'_>) -> (String, String, String) {
    let eol = insert.eol;
    let sep = if insert.wrap {
        format!(",{eol}    ")
    } else {
        ",".to_owned()
    };
    let brk = if insert.wrap {
        eol.to_owned()
    } else {
        " ".to_owned()
    };
    let columns = insert
        .columns
        .map(|c| format!(" ({})", c.join(&sep)))
        .unwrap_or_default();
    (columns, insert.values.join(&sep), brk)
}

impl synth::Render for Render {
    fn unsupported_noise(&self) -> Vec<&'static str> {
        // Every switch is an Oracle or SQL Developer quirk.
        Vec::new()
    }

    fn ddl(&self, item: DdlItem, style: &DdlStyle<'_>) -> Sql {
        let writer = DdlWriter {
            noise: self.noise,
            style: *style,
        };
        let (text, statements) = match item {
            DdlItem::CreateTable(table) => (
                writer.create_table(table),
                create_table_statements(table, style.keys_in_table),
            ),
            DdlItem::LateKeys(table) => (writer.late_keys(table), key_statements(table, |_| true)),
            DdlItem::ForeignKeys(table) => (
                writer.foreign_keys(table),
                count(constraints_of(table).filter(|c| c.is_foreign())),
            ),
            DdlItem::Sequence => (writer.sequence(), 1),
            // The PL/SQL block ended by `/`, then `ALTER TRIGGER … ENABLE`.
            DdlItem::Trigger => (writer.trigger(), 2),
        };
        Sql { text, statements }
    }

    fn literal(&self, value: &Value, rng: &mut Rng) -> String {
        match value {
            Value::Null => "null".into(),
            Value::Int(n) => self.number(&n.to_string(), rng),
            Value::Decimal(s) => self.number(s, rng),
            Value::Text(s) | Value::Char(s) => self.string(s, rng),
            Value::Date(days) => {
                let (y, m, d) = civil_from_days(*days);
                format!(
                    "to_date('{d:02}-{}-{:02}','DD-MON-RR')",
                    month(m),
                    y.rem_euclid(100)
                )
            }
            Value::Timestamp(secs) => {
                let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
                let s = secs.rem_euclid(86_400);
                let (h24, min, sec) = (s / 3600, s % 3600 / 60, s % 60);
                let (h12, ampm) = match h24 {
                    0 => (12, "AM"),
                    1..=11 => (h24, "AM"),
                    12 => (12, "PM"),
                    _ => (h24 - 12, "PM"),
                };
                format!(
                    "to_timestamp('{d:02}-{}-{:02} {h12:02}.{min:02}.{sec:02}.000000000 {ampm}','DD-MON-RR HH.MI.SSXFF AM')",
                    month(m),
                    y.rem_euclid(100)
                )
            }
            Value::NextVal(schema, seq) => format!("{schema}.{seq}.NEXTVAL"),
        }
    }

    fn data_section(&self, table: TableId, edge: Edge, eol: &str) -> Sql {
        let name = table.qualified(&RULES);
        let noise = self.noise;
        // `REM` and `SET DEFINE OFF` are script commands; comments are no statements.
        let statements = match edge {
            Edge::Start => u64::from(noise.rem_header) + u64::from(noise.set_define_off),
            Edge::End => 0,
        };
        let text = match edge {
            Edge::Start => [
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
            .collect(),
            Edge::End if noise.comments => format!("-- end of {name}{eol}"),
            Edge::End => String::new(),
        };
        Sql { text, statements }
    }

    fn insert(&self, insert: &Insert<'_>) -> String {
        let (columns, values, brk) = lists(insert);
        let (keyword, values_keyword) = match insert.case {
            1 => ("insert into", "values"),
            2 => ("INSERT INTO", "VALUES"),
            _ => ("Insert into", "values"),
        };
        format!(
            "{keyword} {}{columns}{brk}{values_keyword} ({values});{}",
            insert.table, insert.eol
        )
    }

    fn batch(&self, part: Batch<'_>, eol: &str) -> String {
        match part {
            Batch::Start => format!("INSERT ALL{eol}"),
            Batch::Row(insert) => {
                let (columns, values, brk) = lists(insert);
                format!(
                    "  INTO {}{columns}{brk}VALUES ({values}){eol}",
                    insert.table
                )
            }
            Batch::End => format!("SELECT 1 FROM DUAL;{eol}"),
        }
    }

    fn commit(&self, eol: &str) -> Sql {
        Sql {
            text: format!("{}{eol}", super::COMMIT),
            statements: 1,
        }
    }

    fn load_script(&self, paths: &[&str], eol: &str) -> String {
        paths
            .iter()
            .fold(format!("SET DEFINE OFF{eol}"), |mut script, path| {
                script.push_str("@@");
                script.push_str(path);
                script.push_str(eol);
                script
            })
    }
}

fn count<T>(items: impl Iterator<Item = T>) -> u64 {
    u64::try_from(items.count()).unwrap_or(u64::MAX)
}

/// Statements in a `CREATE TABLE` item: the table, each `MODIFY … NOT NULL` and `ADD` column, and, when keys are in
/// the table DDL, its `ALTER TABLE … ADD CONSTRAINT` keys and unique index.
fn create_table_statements(table: TableId, keys_in_table: bool) -> u64 {
    let columns = table.columns();
    let separate = count(
        columns
            .iter()
            .filter(|c| c.not_null == NotNull::AfterTable || c.added_later),
    );
    let keys = if keys_in_table {
        key_statements(table, |c| c.form == Form::Alter)
    } else {
        0
    };
    1 + separate + keys
}

/// `ALTER TABLE … ADD` statements for the keys `which` selects, plus the unique index.
fn key_statements(table: TableId, which: impl Fn(&Constraint) -> bool) -> u64 {
    let keys = count(constraints_of(table).filter(|c| !c.is_foreign() && which(c)));
    keys + u64::from(table == EMPLOYEES_EMAIL_UIX.table)
}

/// Writes DDL the way SQL Developer exports it.
#[derive(Debug, Clone, Copy)]
struct DdlWriter<'a> {
    noise: Noise,
    style: DdlStyle<'a>,
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn qt(table: TableId) -> String {
    format!("{}.{}", q(table.schema()), q(table.name()))
}

fn ty_sql(ty: Ty) -> String {
    match ty {
        Ty::Decimal(p, s) => format!("NUMBER({p},{s})"),
        Ty::Varchar(n) => format!("VARCHAR2({n} BYTE)"),
        Ty::Char(n) => format!("CHAR({n} BYTE)"),
        Ty::Date => "DATE".into(),
        Ty::Timestamp => "TIMESTAMP (6)".into(),
    }
}

fn col_list(columns: &[&str]) -> String {
    columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(",")
}

const USING_INDEX: &str = "USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS ";
const TABLE_STORAGE: [&str; 6] = [
    " SEGMENT CREATION IMMEDIATE ",
    "  PCTFREE 10 PCTUSED 40 INITRANS 1 MAXTRANS 255 ",
    " NOCOMPRESS LOGGING",
    "  STORAGE(INITIAL 65536 NEXT 1048576 MINEXTENTS 1 MAXEXTENTS 2147483645",
    "  PCTINCREASE 0 FREELISTS 1 FREELIST GROUPS 1",
    "  BUFFER_POOL DEFAULT FLASH_CACHE DEFAULT CELL_FLASH_CACHE DEFAULT)",
];

impl DdlWriter<'_> {
    fn banner(&self, out: &mut String, what: &str) {
        if self.noise.comments {
            let eol = self.style.eol;
            let rule = "--------------------------------------------------------";
            put!(out, "{rule}{eol}--  DDL for {what}{eol}{rule}{eol}{eol}");
        }
    }

    fn key_clause(&self, c: &Constraint) -> String {
        let eol = self.style.eol;
        let kind = if matches!(c.kind, Kind::Primary) {
            "PRIMARY KEY"
        } else {
            "UNIQUE"
        };
        if self.noise.storage_clauses {
            format!(
                "CONSTRAINT {} {kind} ({}){eol}  {USING_INDEX}{eol}  TABLESPACE \"USERS\"  ENABLE",
                q(c.name),
                col_list(c.columns)
            )
        } else {
            format!(
                "CONSTRAINT {} {kind} ({}) ENABLE",
                q(c.name),
                col_list(c.columns)
            )
        }
    }

    fn create_table(&self, table: TableId) -> String {
        let eol = self.style.eol;
        let mut out = String::new();
        self.banner(&mut out, &format!("Table {}", table.name()));
        let inline_pk =
            constraints_of(table).find(|c| self.style.keys_in_table && c.form == Form::Inline);
        let mut lines: Vec<String> = table
            .columns()
            .iter()
            .filter(|c| !c.added_later)
            .map(|c| {
                let mut line = format!("{} {}", q(c.name), ty_sql(c.ty));
                if let Some(default) = c.default {
                    line.push_str(if c.on_null {
                        " DEFAULT ON NULL "
                    } else {
                        " DEFAULT "
                    });
                    let value = match default {
                        DefaultValue::Text(text) => format!("'{}'", text.replace('\'', "''")),
                        DefaultValue::CurrentTimestamp => "SYSTIMESTAMP".to_owned(),
                    };
                    line.push_str(&value);
                }
                if c.not_null == NotNull::InColumn {
                    line.push_str(" NOT NULL ENABLE");
                }
                if let Some(pk) = inline_pk.filter(|pk| pk.columns == [c.name]) {
                    put!(line, " CONSTRAINT {} PRIMARY KEY", q(pk.name));
                }
                line
            })
            .collect();
        if self.style.keys_in_table {
            lines.extend(
                constraints_of(table)
                    .filter(|c| c.form == Form::OutOfLine && !c.is_foreign())
                    .map(|c| format!(" {}", self.key_clause(c))),
            );
        }
        put!(
            out,
            "  CREATE TABLE {} {eol}   (\t{}{eol}   )",
            qt(table),
            lines.join(&format!(", {eol}\t"))
        );
        if self.noise.storage_clauses {
            for line in TABLE_STORAGE {
                put!(out, "{line}{eol}");
            }
            put!(out, "  TABLESPACE \"USERS\" ;{eol}");
        } else {
            put!(out, " ;{eol}");
        }
        for c in table.columns() {
            if c.not_null == NotNull::AfterTable {
                put!(
                    out,
                    "{eol}  ALTER TABLE {} MODIFY ({} NOT NULL ENABLE);{eol}",
                    qt(table),
                    q(c.name)
                );
            }
            if c.added_later {
                put!(
                    out,
                    "{eol}  ALTER TABLE {} ADD ({} {});{eol}",
                    qt(table),
                    q(c.name),
                    ty_sql(c.ty)
                );
            }
        }
        if self.style.keys_in_table {
            out.push_str(&self.keys(table, |c| c.form == Form::Alter));
        }
        out
    }

    /// Primary and unique keys declared by `ALTER TABLE`, plus the unique index, for those `which` selects.
    fn keys(&self, table: TableId, which: impl Fn(&Constraint) -> bool) -> String {
        let eol = self.style.eol;
        let mut out = String::new();
        for c in constraints_of(table).filter(|c| !c.is_foreign() && which(c)) {
            put!(
                out,
                "{eol}  ALTER TABLE {} ADD {};{eol}",
                qt(table),
                self.key_clause(c)
            );
        }
        if table == EMPLOYEES_EMAIL_UIX.table {
            let ix = EMPLOYEES_EMAIL_UIX;
            self.banner(&mut out, &format!("Index {}", ix.name));
            put!(
                out,
                "  CREATE UNIQUE INDEX {}.{} ON {} ({}) ",
                q(ix.table.schema()),
                q(ix.name),
                qt(table),
                col_list(ix.columns)
            );
            if self.noise.storage_clauses {
                put!(
                    out,
                    "{eol}  PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS {eol}  TABLESPACE \"USERS\" "
                );
            }
            put!(out, ";{eol}");
        }
        out
    }

    /// Keys that the table DDL didn't declare: all of them when they come after the data.
    fn late_keys(&self, table: TableId) -> String {
        self.keys(table, |_| true)
    }

    fn foreign_keys(&self, table: TableId) -> String {
        let eol = self.style.eol;
        let mut out = String::new();
        let mut first = true;
        for c in constraints_of(table) {
            let Kind::Foreign {
                parent,
                parent_columns,
                deferred,
            } = c.kind
            else {
                continue;
            };
            if first {
                self.banner(
                    &mut out,
                    &format!("Ref Constraints for Table {}", table.name()),
                );
                first = false;
            }
            let columns = parent_columns
                .map(|cols| format!(" ({})", col_list(cols)))
                .unwrap_or_default();
            let deferrable = if deferred {
                " DEFERRABLE INITIALLY DEFERRED"
            } else {
                ""
            };
            let state = match c.state {
                State::Enforced => "ENABLE",
                State::NotValidated => "ENABLE NOVALIDATE",
                State::Disabled => "DISABLE",
            };
            put!(
                out,
                "  ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}){eol}\t  REFERENCES {}{columns}{deferrable} {state};{eol}",
                qt(table),
                q(c.name),
                col_list(c.columns),
                qt(parent),
            );
        }
        out
    }

    fn sequence(&self) -> String {
        let eol = self.style.eol;
        let mut out = String::new();
        self.banner(&mut out, &format!("Sequence {AUDIT_SEQ}"));
        put!(
            out,
            "   CREATE SEQUENCE  {}.{}  MINVALUE 1 MAXVALUE 9999999999999999999999999999 INCREMENT BY 1 START WITH 1 CACHE 20 NOORDER  NOCYCLE ;{eol}",
            q(APP_SCHEMA),
            q(AUDIT_SEQ)
        );
        out
    }

    /// A harmless trigger: its body has a `;`, and it ends with `/` on its own line.
    fn trigger(&self) -> String {
        let eol = self.style.eol;
        let mut out = String::new();
        self.banner(&mut out, &format!("Trigger {AUDIT_TRIGGER}"));
        let trigger = format!("{}.{}", q(APP_SCHEMA), q(AUDIT_TRIGGER));
        put!(
            out,
            "  CREATE OR REPLACE EDITIONABLE TRIGGER {trigger} {eol}  BEFORE INSERT ON {} FOR EACH ROW{eol}BEGIN{eol}  NULL;{eol}END;{eol}/{eol}ALTER TRIGGER {trigger} ENABLE;{eol}",
            qt(TableId::AuditLog)
        );
        out
    }
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

impl Render {
    fn number(&self, digits: &str, rng: &mut Rng) -> String {
        if self.noise.numeric_forms {
            match rng.below(5) {
                0 => digits.to_owned(),
                2 if !digits.contains('.') => format!("{digits}.0"),
                3 if !digits.contains('.') => format!("{digits}E0"),
                4 if !digits.contains('.') && !digits.starts_with('-') => format!("'0{digits}'"),
                _ => format!("'{digits}'"),
            }
        } else if self.noise.quoted_numbers {
            format!("'{digits}'")
        } else {
            digits.to_owned()
        }
    }

    fn string(&self, s: &str, rng: &mut Rng) -> String {
        if self.noise.q_quote && s.contains('\'') && !s.contains("]'") && rng.below(2) == 0 {
            format!("q'[{s}]'")
        } else {
            format!("'{}'", s.replace('\'', "''"))
        }
    }
}

fn month(m: u32) -> &'static str {
    MONTHS
        .get(usize::try_from(m.saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or("JAN")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::Render as _;
    use crate::synth::value::days_from_civil;

    /// DDL without storage clauses or comments.
    fn plain(item: DdlItem, keys_in_table: bool) -> String {
        let style = DdlStyle {
            eol: "\n",
            keys_in_table,
        };
        Render::new(Noise::default()).ddl(item, &style).text
    }

    #[test]
    fn employees_ddl_has_its_defaults_and_out_of_line_key() {
        let ddl = plain(DdlItem::CreateTable(TableId::Employees), true);
        assert!(
            ddl.contains("\"STATUS\" VARCHAR2(10 BYTE) DEFAULT 'ACTIVE' NOT NULL ENABLE"),
            "{ddl}"
        );
        assert!(ddl.contains("DEFAULT ON NULL SYSTIMESTAMP"), "{ddl}");
        assert!(
            ddl.contains("CONSTRAINT \"EMPLOYEES_PK\" PRIMARY KEY (\"EMP_ID\") ENABLE"),
            "{ddl}"
        );
        assert!(ddl.contains("ADD (\"PHONE\" VARCHAR2(20 BYTE));"), "{ddl}");
        assert!(
            ddl.contains("MODIFY (\"LAST_NAME\" NOT NULL ENABLE);"),
            "{ddl}"
        );
        assert!(
            ddl.contains("CREATE UNIQUE INDEX \"APP\".\"EMPLOYEES_EMAIL_UIX\""),
            "{ddl}"
        );
        assert!(!ddl.contains("\"PHONE\" VARCHAR2(20 BYTE),"), "{ddl}");
    }

    #[test]
    fn foreign_keys_carry_their_states() {
        let ddl = plain(DdlItem::ForeignKeys(TableId::Departments), true);
        assert!(ddl.contains("REFERENCES \"APP\".\"EMPLOYEES\" (\"EMP_ID\") DEFERRABLE INITIALLY DEFERRED ENABLE;"), "{ddl}");
        let ddl = plain(DdlItem::ForeignKeys(TableId::Employees), true);
        assert!(
            ddl.contains(
                "FOREIGN KEY (\"DEPT_ID\")\n\t  REFERENCES \"APP\".\"DEPARTMENTS\" ENABLE;"
            ),
            "{ddl}"
        );
        assert!(
            plain(DdlItem::ForeignKeys(TableId::Projects), true).contains("ENABLE NOVALIDATE;")
        );
        assert!(plain(DdlItem::ForeignKeys(TableId::LegacyNotes), true).contains(" DISABLE;"));
    }

    #[test]
    fn keys_move_out_of_the_table_ddl_when_they_come_after_the_data() {
        let ddl = plain(DdlItem::CreateTable(TableId::Countries), false);
        assert!(!ddl.contains("PRIMARY KEY"), "{ddl}");
        assert!(
            plain(DdlItem::LateKeys(TableId::Countries), false)
                .contains("ADD CONSTRAINT \"COUNTRIES_PK\" PRIMARY KEY")
        );
        assert!(
            plain(DdlItem::CreateTable(TableId::Countries), true)
                .contains("\"COUNTRY_CODE\" CHAR(3 BYTE) CONSTRAINT \"COUNTRIES_PK\" PRIMARY KEY")
        );
    }

    #[test]
    fn literals_look_like_sql_developer_writes_them() {
        let mut rng = Rng::new(1, 1);
        let sqldev = Render::new(Noise {
            quoted_numbers: true,
            ..Noise::default()
        });
        assert_eq!(sqldev.literal(&Value::Int(100), &mut rng), "'100'");
        assert_eq!(
            sqldev.literal(&Value::text("O'Brien"), &mut rng),
            "'O''Brien'"
        );
        assert_eq!(
            sqldev.literal(&Value::Date(days_from_civil(2003, 6, 17)), &mut rng),
            "to_date('17-JUN-03','DD-MON-RR')"
        );
        let ts = days_from_civil(2005, 9, 21) * 86_400 + 13 * 3600 + 2 * 60 + 3;
        assert_eq!(
            sqldev.literal(&Value::Timestamp(ts), &mut rng),
            "to_timestamp('21-SEP-05 01.02.03.000000000 PM','DD-MON-RR HH.MI.SSXFF AM')"
        );
        assert_eq!(
            Render::new(Noise::default()).literal(&Value::Null, &mut rng),
            "null"
        );
    }
    #[test]
    fn every_numeric_form_means_the_same_number() {
        let style = Render::new(Noise {
            numeric_forms: true,
            ..Noise::default()
        });
        let mut rng = Rng::new(9, 9);
        let forms: std::collections::BTreeSet<String> = (0..200)
            .map(|_| style.literal(&Value::Int(10), &mut rng))
            .collect();
        assert_eq!(
            forms.into_iter().collect::<Vec<_>>(),
            ["'010'", "'10'", "10", "10.0", "10E0"]
        );
        let negative: std::collections::BTreeSet<String> = (0..200)
            .map(|_| style.literal(&Value::Int(-3), &mut rng))
            .collect();
        assert_eq!(
            negative.into_iter().collect::<Vec<_>>(),
            ["'-3'", "-3", "-3.0", "-3E0"]
        );
    }
}
