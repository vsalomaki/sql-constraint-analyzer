//! The fixed schema the generator writes: tables, columns and constraints, in neutral terms; each dialect renders it.

use crate::dialect::Rules;

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

    /// The name as the schema defines it: upper case, except the one mixed-case table that needs quoting.
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

    pub(crate) fn qualified(self, rules: &Rules) -> String {
        rules.qualified(self.schema(), self.name())
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

    /// A table named `SCHEMA.NAME` or `NAME`, quoted or not, in any case.
    pub(crate) fn from_qualified(name: &str) -> Option<Self> {
        let name = name.replace('"', "");
        REF_TABLES
            .iter()
            .chain(APP_TABLES.iter())
            .copied()
            .find(|t| {
                format!("{}.{}", t.schema(), t.name()).eq_ignore_ascii_case(&name)
                    || t.name().eq_ignore_ascii_case(&name)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ty {
    Decimal(u8, u8),
    Varchar(u16),
    Char(u16),
    Date,
    Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotNull {
    No,
    /// Declared on the column in the `CREATE TABLE`.
    InColumn,
    /// Declared by a separate statement right after the `CREATE TABLE`.
    AfterTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DefaultValue {
    Text(&'static str),
    /// The time the row is inserted.
    CurrentTimestamp,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Column {
    pub(crate) name: &'static str,
    pub(crate) ty: Ty,
    pub(crate) not_null: NotNull,
    pub(crate) default: Option<DefaultValue>,
    /// An explicit NULL takes the default too. A dialect without this leaves the column nullable, so an explicit NULL
    /// is no violation there either.
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
        not_null: NotNull::AfterTable,
        ..col(name, ty)
    }
}

const COUNTRIES: &[Column] = &[
    col("COUNTRY_CODE", Ty::Char(3)),
    nn("NAME", Ty::Varchar(60)),
];

const DEPARTMENTS: &[Column] = &[
    col("DEPT_ID", Ty::Decimal(9, 0)),
    nn("NAME", Ty::Varchar(40)),
    col("MANAGER_ID", Ty::Decimal(9, 0)),
    col("COUNTRY_CODE", Ty::Char(3)),
];

const EMPLOYEES: &[Column] = &[
    col("EMP_ID", Ty::Decimal(9, 0)),
    col("FIRST_NAME", Ty::Varchar(40)),
    nn("LAST_NAME", Ty::Varchar(40)),
    Column {
        not_null: NotNull::InColumn,
        ..col("EMAIL", Ty::Varchar(80))
    },
    col("DEPT_ID", Ty::Decimal(9, 0)),
    col("MANAGER_ID", Ty::Decimal(9, 0)),
    nn("HIRE_DATE", Ty::Date),
    Column {
        not_null: NotNull::InColumn,
        default: Some(DefaultValue::Text("ACTIVE")),
        ..col("STATUS", Ty::Varchar(10))
    },
    Column {
        default: Some(DefaultValue::CurrentTimestamp),
        on_null: true,
        ..col("CREATED_AT", Ty::Timestamp)
    },
    Column {
        added_later: true,
        ..col("PHONE", Ty::Varchar(20))
    },
];

const PROJECTS: &[Column] = &[
    col("PROJECT_CODE", Ty::Varchar(10)),
    nn("NAME", Ty::Varchar(80)),
    col("DEPT_NAME", Ty::Varchar(40)),
    col("EXTERNAL_REF", Ty::Varchar(20)),
    col("START_DATE", Ty::Date),
];

const ASSIGNMENTS: &[Column] = &[
    col("EMP_ID", Ty::Decimal(9, 0)),
    col("PROJECT_CODE", Ty::Varchar(10)),
    col("ROLE", Ty::Varchar(20)),
    col("ASSIGNED_ON", Ty::Date),
];

const TIMESHEETS: &[Column] = &[
    col("TS_ID", Ty::Decimal(12, 0)),
    col("EMP_ID", Ty::Decimal(9, 0)),
    col("PROJECT_CODE", Ty::Varchar(10)),
    nn("WORK_DATE", Ty::Date),
    nn("HOURS", Ty::Decimal(4, 2)),
];

const LEGACY_NOTES: &[Column] = &[
    col("NOTE_ID", Ty::Decimal(9, 0)),
    col("EMP_ID", Ty::Decimal(9, 0)),
    col("NOTE_TEXT", Ty::Varchar(400)),
];

const AUDIT_LOG: &[Column] = &[
    col("LOG_ID", Ty::Decimal(12, 0)),
    col("EMP_ID", Ty::Decimal(9, 0)),
    nn("LOGGED_AT", Ty::Timestamp),
    col("DETAIL", Ty::Varchar(200)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Enforced,
    /// Enforced for rows inserted after it; rows already there are not checked.
    NotValidated,
    Disabled,
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
    /// `ALTER TABLE … ADD CONSTRAINT … ENABLE NOVALIDATE` between the table's rows, over a non-unique index the table
    /// DDL creates; written only with `cascades`.
    BetweenRows,
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
    pub(crate) fn qualified(&self, rules: &Rules) -> String {
        rules.qualified(self.table.schema(), self.name)
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
        state: State::Enforced,
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
pub(crate) const PROJECTS_NAME_UK: Constraint = Constraint {
    state: State::NotValidated,
    ..key(
        "PROJECTS_NAME_UK",
        TableId::Projects,
        Kind::Unique,
        &["NAME"],
        Form::BetweenRows,
    )
};
pub(crate) const PROJECTS_DEPT_FK: Constraint = Constraint {
    kind: Kind::Foreign {
        parent: TableId::Departments,
        parent_columns: Some(&["NAME"]),
        deferred: false,
    },
    state: State::NotValidated,
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
    state: State::Disabled,
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

pub(crate) const CONSTRAINTS: [Constraint; 23] = [
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
    PROJECTS_NAME_UK,
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

/// Whether `fix` can set the column to NULL: it isn't NOT NULL or part of the primary key.
pub(crate) fn can_be_null(table: TableId, column: &str) -> bool {
    table
        .columns()
        .iter()
        .any(|c| c.name == column && c.not_null == NotNull::No)
        && !constraints_of(table).any(|c| c.kind == Kind::Primary && c.columns.contains(&column))
}

/// An index the DDL creates on its own.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Index {
    pub(crate) name: &'static str,
    pub(crate) table: TableId,
    pub(crate) columns: &'static [&'static str],
}

impl Index {
    pub(crate) fn qualified(&self, rules: &Rules) -> String {
        rules.qualified(self.table.schema(), self.name)
    }
}

/// Unique.
pub(crate) const EMPLOYEES_EMAIL_UIX: Index = Index {
    name: "EMPLOYEES_EMAIL_UIX",
    table: TableId::Employees,
    columns: &["EMAIL"],
};

/// Not unique, so that `PROJECTS_NAME_UK` can be enabled over rows that already have duplicates.
pub(crate) const PROJECTS_NAME_IX: Index = Index {
    name: "PROJECTS_NAME_IX",
    table: TableId::Projects,
    columns: &["NAME"],
};

pub(crate) const AUDIT_SEQ: &str = "AUDIT_SEQ";
pub(crate) const AUDIT_TRIGGER: &str = "AUDITLOG_BI";

#[cfg(test)]
mod tests {
    use super::*;

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
}
