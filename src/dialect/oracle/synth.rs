//! Oracle syntax for the generator: DDL as SQL Developer exports it, literals, `INSERT` and `INSERT ALL`, SQL\*Plus
//! headers and the `@@` load script.

macro_rules! put {
    ($out:expr, $($arg:tt)*) => {
        $out.push_str(&format!($($arg)*))
    };
}

use super::RULES;
use crate::dialect::Rules;
use crate::synth::render::{Batch, DdlItem, DdlStyle, Edge, Insert};
use crate::synth::rng::Rng;
use crate::synth::schema::{
    APP_SCHEMA, AUDIT_SEQ, AUDIT_TRIGGER, Constraint, DefaultValue, EMPLOYEES_EMAIL_UIX, Form,
    Kind, NotNull, State, TableId, Ty, constraints_of,
};
use crate::synth::value::{Value, civil_from_days};
use crate::synth::{self, Noise};

pub(crate) struct Render {
    noise: Noise,
    literals: LiteralStyle,
}

impl Render {
    pub(crate) fn new(noise: Noise) -> Self {
        let literals = LiteralStyle {
            quoted_numbers: noise.quoted_numbers,
            numeric_forms: noise.numeric_forms,
            q_quote: noise.q_quote,
        };
        Self { noise, literals }
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
    fn rules(&self) -> &'static Rules {
        &RULES
    }

    fn unsupported_noise(&self) -> Vec<&'static str> {
        // Every switch is an Oracle or SQL Developer quirk.
        Vec::new()
    }

    fn ddl(&self, item: DdlItem, style: &DdlStyle<'_>) -> String {
        let writer = DdlWriter::from(style);
        match item {
            DdlItem::CreateTable(table) => writer.create_table(table),
            DdlItem::LateKeys(table) => writer.late_keys(table),
            DdlItem::ForeignKeys(table) => writer.foreign_keys(table),
            DdlItem::Sequence => writer.sequence(),
            DdlItem::Trigger => writer.trigger(),
        }
    }

    fn literal(&self, value: &Value, rng: &mut Rng) -> String {
        self.literals.render(value, rng)
    }

    fn data_section(&self, table: TableId, edge: Edge, eol: &str) -> String {
        let name = table.qualified(&RULES);
        let noise = self.noise;
        match edge {
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
        }
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

/// Writes DDL the way SQL Developer exports it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DdlWriter<'a> {
    eol: &'a str,
    storage: bool,
    comments: bool,
    keys_in_table: bool,
}

impl<'a> From<&DdlStyle<'a>> for DdlWriter<'a> {
    fn from(style: &DdlStyle<'a>) -> Self {
        Self {
            eol: style.eol,
            storage: style.storage,
            comments: style.comments,
            keys_in_table: style.keys_in_table,
        }
    }
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
        if self.comments {
            let eol = self.eol;
            let rule = "--------------------------------------------------------";
            put!(out, "{rule}{eol}--  DDL for {what}{eol}{rule}{eol}{eol}");
        }
    }

    fn key_clause(&self, c: &Constraint) -> String {
        let eol = self.eol;
        let kind = if matches!(c.kind, Kind::Primary) {
            "PRIMARY KEY"
        } else {
            "UNIQUE"
        };
        if self.storage {
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

    pub(crate) fn create_table(&self, table: TableId) -> String {
        let eol = self.eol;
        let mut out = String::new();
        self.banner(&mut out, &format!("Table {}", table.name()));
        let inline_pk =
            constraints_of(table).find(|c| self.keys_in_table && c.form == Form::Inline);
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
        if self.keys_in_table {
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
        if self.storage {
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
        if self.keys_in_table {
            out.push_str(&self.keys(table, |c| c.form == Form::Alter));
        }
        out
    }

    /// Primary and unique keys declared by `ALTER TABLE`, plus the unique index, for those `which` selects.
    pub(crate) fn keys(&self, table: TableId, which: impl Fn(&Constraint) -> bool) -> String {
        let eol = self.eol;
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
            if self.storage {
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
    pub(crate) fn late_keys(&self, table: TableId) -> String {
        self.keys(table, |_| true)
    }

    pub(crate) fn foreign_keys(&self, table: TableId) -> String {
        let eol = self.eol;
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

    pub(crate) fn sequence(&self) -> String {
        let eol = self.eol;
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
    pub(crate) fn trigger(&self) -> String {
        let eol = self.eol;
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

/// Literal styles the noise options switch on.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LiteralStyle {
    pub(crate) quoted_numbers: bool,
    pub(crate) numeric_forms: bool,
    pub(crate) q_quote: bool,
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

impl LiteralStyle {
    pub(crate) fn render(self, value: &Value, rng: &mut Rng) -> String {
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

    fn number(self, digits: &str, rng: &mut Rng) -> String {
        if self.numeric_forms {
            match rng.below(5) {
                0 => digits.to_owned(),
                2 if !digits.contains('.') => format!("{digits}.0"),
                3 if !digits.contains('.') => format!("{digits}E0"),
                4 if !digits.contains('.') && !digits.starts_with('-') => format!("'0{digits}'"),
                _ => format!("'{digits}'"),
            }
        } else if self.quoted_numbers {
            format!("'{digits}'")
        } else {
            digits.to_owned()
        }
    }

    fn string(self, s: &str, rng: &mut Rng) -> String {
        if self.q_quote && s.contains('\'') && !s.contains("]'") && rng.below(2) == 0 {
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
    use crate::synth::value::days_from_civil;

    const PLAIN: DdlWriter<'static> = DdlWriter {
        eol: "\n",
        storage: false,
        comments: false,
        keys_in_table: true,
    };

    #[test]
    fn employees_ddl_has_its_defaults_and_out_of_line_key() {
        let ddl = PLAIN.create_table(TableId::Employees);
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
        let ddl = PLAIN.foreign_keys(TableId::Departments);
        assert!(ddl.contains("REFERENCES \"APP\".\"EMPLOYEES\" (\"EMP_ID\") DEFERRABLE INITIALLY DEFERRED ENABLE;"), "{ddl}");
        let ddl = PLAIN.foreign_keys(TableId::Employees);
        assert!(
            ddl.contains(
                "FOREIGN KEY (\"DEPT_ID\")\n\t  REFERENCES \"APP\".\"DEPARTMENTS\" ENABLE;"
            ),
            "{ddl}"
        );
        assert!(
            PLAIN
                .foreign_keys(TableId::Projects)
                .contains("ENABLE NOVALIDATE;")
        );
        assert!(
            PLAIN
                .foreign_keys(TableId::LegacyNotes)
                .contains(" DISABLE;")
        );
    }

    #[test]
    fn keys_move_out_of_the_table_ddl_when_they_come_after_the_data() {
        let late = DdlWriter {
            keys_in_table: false,
            ..PLAIN
        };
        let ddl = late.create_table(TableId::Countries);
        assert!(!ddl.contains("PRIMARY KEY"), "{ddl}");
        assert!(
            late.late_keys(TableId::Countries)
                .contains("ADD CONSTRAINT \"COUNTRIES_PK\" PRIMARY KEY")
        );
        assert!(
            PLAIN
                .create_table(TableId::Countries)
                .contains("\"COUNTRY_CODE\" CHAR(3 BYTE) CONSTRAINT \"COUNTRIES_PK\" PRIMARY KEY")
        );
    }

    #[test]
    fn literals_look_like_sql_developer_writes_them() {
        let mut rng = Rng::new(1, 1);
        let sqldev = LiteralStyle {
            quoted_numbers: true,
            ..LiteralStyle::default()
        };
        assert_eq!(sqldev.render(&Value::Int(100), &mut rng), "'100'");
        assert_eq!(
            sqldev.render(&Value::text("O'Brien"), &mut rng),
            "'O''Brien'"
        );
        assert_eq!(
            sqldev.render(&Value::Date(days_from_civil(2003, 6, 17)), &mut rng),
            "to_date('17-JUN-03','DD-MON-RR')"
        );
        let ts = days_from_civil(2005, 9, 21) * 86_400 + 13 * 3600 + 2 * 60 + 3;
        assert_eq!(
            sqldev.render(&Value::Timestamp(ts), &mut rng),
            "to_timestamp('21-SEP-05 01.02.03.000000000 PM','DD-MON-RR HH.MI.SSXFF AM')"
        );
        assert_eq!(
            LiteralStyle::default().render(&Value::Null, &mut rng),
            "null"
        );
    }
    #[test]
    fn every_numeric_form_means_the_same_number() {
        let style = LiteralStyle {
            numeric_forms: true,
            ..LiteralStyle::default()
        };
        let mut rng = Rng::new(9, 9);
        let forms: std::collections::BTreeSet<String> = (0..200)
            .map(|_| style.render(&Value::Int(10), &mut rng))
            .collect();
        assert_eq!(
            forms.into_iter().collect::<Vec<_>>(),
            ["'010'", "'10'", "10", "10.0", "10E0"]
        );
        let negative: std::collections::BTreeSet<String> = (0..200)
            .map(|_| style.render(&Value::Int(-3), &mut rng))
            .collect();
        assert_eq!(
            negative.into_iter().collect::<Vec<_>>(),
            ["'-3'", "-3", "-3.0", "-3E0"]
        );
    }
}
