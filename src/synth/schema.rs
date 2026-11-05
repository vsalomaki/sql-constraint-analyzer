//! The fixed schema the generator writes: tables, columns, constraints, and their DDL in SQL Developer's style.

macro_rules! put {
    ($out:expr, $($arg:tt)*) => {
        $out.push_str(&format!($($arg)*))
    };
}

use crate::finding::qualified_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TableId {
    Countries,
    Departments,
    Employees,
    Projects,
    Assignments,
    Timesheets,
    LegacyNotes,
    AuditLog,
}

pub(crate) const REF_SCHEMA: &str = "REF";
pub(crate) const APP_SCHEMA: &str = "APP";

/// Tables in dependency order (parents first) per schema.
pub(crate) const REF_TABLES: [TableId; 1] = [TableId::Countries];
pub(crate) const APP_TABLES: [TableId; 7] = [
    TableId::Departments,
    TableId::Employees,
    TableId::Projects,
    TableId::Assignments,
    TableId::Timesheets,
    TableId::LegacyNotes,
    TableId::AuditLog,
];

impl TableId {
    pub(crate) fn schema(self) -> &'static str {
        match self {
            Self::Countries => REF_SCHEMA,
            _ => APP_SCHEMA,
        }
    }

    /// The name as Oracle stores it: upper case, except the one quoted mixed-case table.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Countries => "COUNTRIES",
            Self::Departments => "DEPARTMENTS",
            Self::Employees => "EMPLOYEES",
            Self::Projects => "PROJECTS",
            Self::Assignments => "ASSIGNMENTS",
            Self::Timesheets => "TIMESHEETS",
            Self::LegacyNotes => "LEGACY_NOTES",
            Self::AuditLog => "AuditLog",
        }
    }

    pub(crate) fn qualified(self) -> String {
        qualified_name(self.schema(), self.name())
    }

    /// A stable number for this table's random stream.
    pub(crate) fn stream(self) -> u64 {
        match self {
            Self::Countries => 1,
            Self::Departments => 2,
            Self::Employees => 3,
            Self::Projects => 4,
            Self::Assignments => 5,
            Self::Timesheets => 6,
            Self::LegacyNotes => 7,
            Self::AuditLog => 8,
        }
    }

    pub(crate) fn columns(self) -> &'static [Column] {
        match self {
            Self::Countries => COUNTRIES,
            Self::Departments => DEPARTMENTS,
            Self::Employees => EMPLOYEES,
            Self::Projects => PROJECTS,
            Self::Assignments => ASSIGNMENTS,
            Self::Timesheets => TIMESHEETS,
            Self::LegacyNotes => LEGACY_NOTES,
            Self::AuditLog => AUDIT_LOG,
        }
    }

    pub(crate) fn from_qualified(name: &str) -> Option<Self> {
        REF_TABLES
            .iter()
            .chain(APP_TABLES.iter())
            .copied()
            .find(|t| {
                t.qualified().eq_ignore_ascii_case(name) || t.name().eq_ignore_ascii_case(name)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ty {
    Number(u8, u8),
    Varchar2(u16),
    Char(u16),
    Date,
    Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotNull {
    No,
    /// `NOT NULL ENABLE` on the column itself.
    Inline,
    /// `ALTER TABLE … MODIFY ("COL" NOT NULL ENABLE)` after the `CREATE TABLE`, as SQL Developer writes it.
    Modify,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Column {
    pub(crate) name: &'static str,
    pub(crate) ty: Ty,
    pub(crate) not_null: NotNull,
    pub(crate) default: Option<&'static str>,
    /// `DEFAULT ON NULL`: an explicit NULL takes the default.
    pub(crate) on_null: bool,
    /// Added by `ALTER TABLE … ADD` after the `CREATE TABLE`.
    pub(crate) added_later: bool,
}

const fn col(name: &'static str, ty: Ty) -> Column {
    Column {
        name,
        ty,
        not_null: NotNull::No,
        default: None,
        on_null: false,
        added_later: false,
    }
}

const fn nn(name: &'static str, ty: Ty) -> Column {
    Column {
        not_null: NotNull::Modify,
        ..col(name, ty)
    }
}

const COUNTRIES: &[Column] = &[
    col("COUNTRY_CODE", Ty::Char(3)),
    nn("NAME", Ty::Varchar2(60)),
];

const DEPARTMENTS: &[Column] = &[
    col("DEPT_ID", Ty::Number(9, 0)),
    nn("NAME", Ty::Varchar2(40)),
    col("MANAGER_ID", Ty::Number(9, 0)),
    col("COUNTRY_CODE", Ty::Char(3)),
];

const EMPLOYEES: &[Column] = &[
    col("EMP_ID", Ty::Number(9, 0)),
    col("FIRST_NAME", Ty::Varchar2(40)),
    nn("LAST_NAME", Ty::Varchar2(40)),
    Column {
        not_null: NotNull::Inline,
        ..col("EMAIL", Ty::Varchar2(80))
    },
    col("DEPT_ID", Ty::Number(9, 0)),
    col("MANAGER_ID", Ty::Number(9, 0)),
    nn("HIRE_DATE", Ty::Date),
    Column {
        not_null: NotNull::Inline,
        default: Some("'ACTIVE'"),
        ..col("STATUS", Ty::Varchar2(10))
    },
    Column {
        default: Some("SYSTIMESTAMP"),
        on_null: true,
        ..col("CREATED_AT", Ty::Timestamp)
    },
    Column {
        added_later: true,
        ..col("PHONE", Ty::Varchar2(20))
    },
];

const PROJECTS: &[Column] = &[
    col("PROJECT_CODE", Ty::Varchar2(10)),
    nn("NAME", Ty::Varchar2(80)),
    col("DEPT_NAME", Ty::Varchar2(40)),
    col("EXTERNAL_REF", Ty::Varchar2(20)),
    col("START_DATE", Ty::Date),
];

const ASSIGNMENTS: &[Column] = &[
    col("EMP_ID", Ty::Number(9, 0)),
    col("PROJECT_CODE", Ty::Varchar2(10)),
    col("ROLE", Ty::Varchar2(20)),
    col("ASSIGNED_ON", Ty::Date),
];

const TIMESHEETS: &[Column] = &[
    col("TS_ID", Ty::Number(12, 0)),
    col("EMP_ID", Ty::Number(9, 0)),
    col("PROJECT_CODE", Ty::Varchar2(10)),
    nn("WORK_DATE", Ty::Date),
    nn("HOURS", Ty::Number(4, 2)),
];

const LEGACY_NOTES: &[Column] = &[
    col("NOTE_ID", Ty::Number(9, 0)),
    col("EMP_ID", Ty::Number(9, 0)),
    col("NOTE_TEXT", Ty::Varchar2(400)),
];

const AUDIT_LOG: &[Column] = &[
    col("LOG_ID", Ty::Number(12, 0)),
    col("EMP_ID", Ty::Number(9, 0)),
    nn("LOGGED_AT", Ty::Timestamp),
    col("DETAIL", Ty::Varchar2(200)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Enable,
    /// Enforced for rows inserted after it; rows already there are not checked.
    EnableNovalidate,
    Disable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Primary,
    Unique,
    Foreign {
        parent: TableId,
        /// `None`: `REFERENCES T` without a column list, meaning the parent's primary key.
        parent_columns: Option<&'static [&'static str]>,
        deferred: bool,
    },
}

/// Where a constraint is declared when constraints come before the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
    /// On the column: `"C" CHAR(3) CONSTRAINT "PK" PRIMARY KEY`.
    Inline,
    /// A `CONSTRAINT` clause in the `CREATE TABLE`.
    OutOfLine,
    /// `ALTER TABLE … ADD CONSTRAINT`.
    Alter,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Constraint {
    pub(crate) name: &'static str,
    pub(crate) table: TableId,
    pub(crate) kind: Kind,
    pub(crate) columns: &'static [&'static str],
    pub(crate) state: State,
    pub(crate) form: Form,
}

impl Constraint {
    pub(crate) fn qualified(&self) -> String {
        qualified_name(self.table.schema(), self.name)
    }

    pub(crate) fn is_foreign(&self) -> bool {
        matches!(self.kind, Kind::Foreign { .. })
    }
}

const fn key(
    name: &'static str,
    table: TableId,
    kind: Kind,
    columns: &'static [&'static str],
    form: Form,
) -> Constraint {
    Constraint {
        name,
        table,
        kind,
        columns,
        state: State::Enable,
        form,
    }
}

const fn fk(
    name: &'static str,
    table: TableId,
    columns: &'static [&'static str],
    parent: TableId,
) -> Constraint {
    key(
        name,
        table,
        Kind::Foreign {
            parent,
            parent_columns: None,
            deferred: false,
        },
        columns,
        Form::Alter,
    )
}

pub(crate) const COUNTRIES_PK: Constraint = key(
    "COUNTRIES_PK",
    TableId::Countries,
    Kind::Primary,
    &["COUNTRY_CODE"],
    Form::Inline,
);
pub(crate) const DEPARTMENTS_PK: Constraint = key(
    "DEPARTMENTS_PK",
    TableId::Departments,
    Kind::Primary,
    &["DEPT_ID"],
    Form::OutOfLine,
);
pub(crate) const DEPARTMENTS_NAME_UK: Constraint = key(
    "DEPARTMENTS_NAME_UK",
    TableId::Departments,
    Kind::Unique,
    &["NAME"],
    Form::Alter,
);
pub(crate) const DEPARTMENTS_MANAGER_FK: Constraint = Constraint {
    kind: Kind::Foreign {
        parent: TableId::Employees,
        parent_columns: Some(&["EMP_ID"]),
        deferred: true,
    },
    ..fk(
        "DEPARTMENTS_MANAGER_FK",
        TableId::Departments,
        &["MANAGER_ID"],
        TableId::Employees,
    )
};
pub(crate) const DEPARTMENTS_COUNTRY_FK: Constraint = Constraint {
    kind: Kind::Foreign {
        parent: TableId::Countries,
        parent_columns: Some(&["COUNTRY_CODE"]),
        deferred: false,
    },
    ..fk(
        "DEPARTMENTS_COUNTRY_FK",
        TableId::Departments,
        &["COUNTRY_CODE"],
        TableId::Countries,
    )
};
pub(crate) const EMPLOYEES_PK: Constraint = key(
    "EMPLOYEES_PK",
    TableId::Employees,
    Kind::Primary,
    &["EMP_ID"],
    Form::OutOfLine,
);
pub(crate) const EMPLOYEES_DEPT_FK: Constraint = fk(
    "EMPLOYEES_DEPT_FK",
    TableId::Employees,
    &["DEPT_ID"],
    TableId::Departments,
);
pub(crate) const EMPLOYEES_MANAGER_FK: Constraint = Constraint {
    kind: Kind::Foreign {
        parent: TableId::Employees,
        parent_columns: Some(&["EMP_ID"]),
        deferred: false,
    },
    ..fk(
        "EMPLOYEES_MANAGER_FK",
        TableId::Employees,
        &["MANAGER_ID"],
        TableId::Employees,
    )
};
pub(crate) const PROJECTS_PK: Constraint = key(
    "PROJECTS_PK",
    TableId::Projects,
    Kind::Primary,
    &["PROJECT_CODE"],
    Form::OutOfLine,
);
pub(crate) const PROJECTS_EXTREF_UK: Constraint = key(
    "PROJECTS_EXTREF_UK",
    TableId::Projects,
    Kind::Unique,
    &["EXTERNAL_REF"],
    Form::OutOfLine,
);
pub(crate) const PROJECTS_DEPT_FK: Constraint = Constraint {
    kind: Kind::Foreign {
        parent: TableId::Departments,
        parent_columns: Some(&["NAME"]),
        deferred: false,
    },
    state: State::EnableNovalidate,
    ..fk(
        "PROJECTS_DEPT_FK",
        TableId::Projects,
        &["DEPT_NAME"],
        TableId::Departments,
    )
};
pub(crate) const ASSIGNMENTS_PK: Constraint = key(
    "ASSIGNMENTS_PK",
    TableId::Assignments,
    Kind::Primary,
    &["EMP_ID", "PROJECT_CODE"],
    Form::OutOfLine,
);
pub(crate) const ASSIGNMENTS_ROLE_UK: Constraint = key(
    "ASSIGNMENTS_ROLE_UK",
    TableId::Assignments,
    Kind::Unique,
    &["EMP_ID", "ROLE"],
    Form::Alter,
);
pub(crate) const ASSIGNMENTS_EMP_FK: Constraint = fk(
    "ASSIGNMENTS_EMP_FK",
    TableId::Assignments,
    &["EMP_ID"],
    TableId::Employees,
);
pub(crate) const ASSIGNMENTS_PROJECT_FK: Constraint = fk(
    "ASSIGNMENTS_PROJECT_FK",
    TableId::Assignments,
    &["PROJECT_CODE"],
    TableId::Projects,
);
pub(crate) const TIMESHEETS_PK: Constraint = key(
    "TIMESHEETS_PK",
    TableId::Timesheets,
    Kind::Primary,
    &["TS_ID"],
    Form::OutOfLine,
);
pub(crate) const TIMESHEETS_DAY_UK: Constraint = key(
    "TIMESHEETS_DAY_UK",
    TableId::Timesheets,
    Kind::Unique,
    &["EMP_ID", "PROJECT_CODE", "WORK_DATE"],
    Form::Alter,
);
pub(crate) const TIMESHEETS_ASSIGNMENT_FK: Constraint = Constraint {
    kind: Kind::Foreign {
        parent: TableId::Assignments,
        parent_columns: Some(&["EMP_ID", "PROJECT_CODE"]),
        deferred: false,
    },
    ..fk(
        "TIMESHEETS_ASSIGNMENT_FK",
        TableId::Timesheets,
        &["EMP_ID", "PROJECT_CODE"],
        TableId::Assignments,
    )
};
pub(crate) const LEGACY_NOTES_PK: Constraint = key(
    "LEGACY_NOTES_PK",
    TableId::LegacyNotes,
    Kind::Primary,
    &["NOTE_ID"],
    Form::OutOfLine,
);
pub(crate) const LEGACY_NOTES_EMP_FK: Constraint = Constraint {
    state: State::Disable,
    ..fk(
        "LEGACY_NOTES_EMP_FK",
        TableId::LegacyNotes,
        &["EMP_ID"],
        TableId::Employees,
    )
};
pub(crate) const AUDITLOG_PK: Constraint = key(
    "AUDITLOG_PK",
    TableId::AuditLog,
    Kind::Primary,
    &["LOG_ID"],
    Form::OutOfLine,
);
pub(crate) const AUDITLOG_EMP_FK: Constraint = fk(
    "AUDITLOG_EMP_FK",
    TableId::AuditLog,
    &["EMP_ID"],
    TableId::Employees,
);

pub(crate) const CONSTRAINTS: [Constraint; 22] = [
    COUNTRIES_PK,
    DEPARTMENTS_PK,
    DEPARTMENTS_NAME_UK,
    DEPARTMENTS_MANAGER_FK,
    DEPARTMENTS_COUNTRY_FK,
    EMPLOYEES_PK,
    EMPLOYEES_DEPT_FK,
    EMPLOYEES_MANAGER_FK,
    PROJECTS_PK,
    PROJECTS_EXTREF_UK,
    PROJECTS_DEPT_FK,
    ASSIGNMENTS_PK,
    ASSIGNMENTS_ROLE_UK,
    ASSIGNMENTS_EMP_FK,
    ASSIGNMENTS_PROJECT_FK,
    TIMESHEETS_PK,
    TIMESHEETS_DAY_UK,
    TIMESHEETS_ASSIGNMENT_FK,
    LEGACY_NOTES_PK,
    LEGACY_NOTES_EMP_FK,
    AUDITLOG_PK,
    AUDITLOG_EMP_FK,
];

pub(crate) fn constraints_of(table: TableId) -> impl Iterator<Item = &'static Constraint> {
    CONSTRAINTS.iter().filter(move |c| c.table == table)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct UniqueIndex {
    pub(crate) name: &'static str,
    pub(crate) table: TableId,
    pub(crate) columns: &'static [&'static str],
}

impl UniqueIndex {
    pub(crate) fn qualified(&self) -> String {
        qualified_name(self.table.schema(), self.name)
    }
}

pub(crate) const EMPLOYEES_EMAIL_UIX: UniqueIndex = UniqueIndex {
    name: "EMPLOYEES_EMAIL_UIX",
    table: TableId::Employees,
    columns: &["EMAIL"],
};

pub(crate) const AUDIT_SEQ: &str = "AUDIT_SEQ";
pub(crate) const AUDIT_TRIGGER: &str = "AUDITLOG_BI";

/// How the DDL is written.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DdlStyle<'a> {
    pub(crate) eol: &'a str,
    pub(crate) storage: bool,
    pub(crate) comments: bool,
    /// Primary and unique keys go into the table DDL; otherwise they are added after the data.
    pub(crate) keys_in_table: bool,
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn qt(table: TableId) -> String {
    format!("{}.{}", q(table.schema()), q(table.name()))
}

fn ty_sql(ty: Ty) -> String {
    match ty {
        Ty::Number(p, s) => format!("NUMBER({p},{s})"),
        Ty::Varchar2(n) => format!("VARCHAR2({n} BYTE)"),
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

impl DdlStyle<'_> {
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
                    line.push_str(default);
                }
                if c.not_null == NotNull::Inline {
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
            if c.not_null == NotNull::Modify {
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
                State::Enable => "ENABLE",
                State::EnableNovalidate => "ENABLE NOVALIDATE",
                State::Disable => "DISABLE",
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

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: DdlStyle<'static> = DdlStyle {
        eol: "\n",
        storage: false,
        comments: false,
        keys_in_table: true,
    };

    #[test]
    fn every_constraint_names_existing_columns() {
        for c in &CONSTRAINTS {
            for name in c.columns {
                assert!(
                    c.table.columns().iter().any(|col| col.name == *name),
                    "{} {name}",
                    c.name
                );
            }
            if let Kind::Foreign {
                parent,
                parent_columns: Some(cols),
                ..
            } = c.kind
            {
                for name in cols {
                    assert!(
                        parent.columns().iter().any(|col| col.name == *name),
                        "{} {name}",
                        c.name
                    );
                }
            }
        }
    }

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
        let late = DdlStyle {
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
}
