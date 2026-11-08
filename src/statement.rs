//! The statements a dialect's parser produces, in neutral terms: what the catalog and the checker need, and where
//! in the file each value sits, for the fix pass. A dialect parses its own syntax into these; nothing here names one.

use std::borrow::Cow;

use crate::source::Span;

/// An identifier as the dialect resolves it: an unquoted one already folded by the dialect's rules, a quoted one as
/// written (without its quotes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ident<'a> {
    pub value: Cow<'a, str>,
    pub quoted: bool,
}

impl Ident<'_> {
    pub fn into_owned(self) -> Ident<'static> {
        Ident {
            value: Cow::Owned(self.value.into_owned()),
            quoted: self.quoted,
        }
    }
}

/// A possibly schema-qualified name, as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name<'a> {
    pub schema: Option<Ident<'a>>,
    pub name: Ident<'a>,
}

impl Name<'_> {
    pub fn into_owned(self) -> Name<'static> {
        Name {
            schema: self.schema.map(Ident::into_owned),
            name: self.name.into_owned(),
        }
    }
}

/// The family a column's type belongs to: what decides how its values compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFamily {
    Number,
    /// Variable-length text.
    Text,
    /// Fixed-length text, blank-padded.
    FixedText,
    Date,
    Timestamp,
    Binary,
    /// Large objects, XML, user-defined and other types the checker doesn't compare.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataType {
    pub family: TypeFamily,
    /// The type as written, upper-cased: `NUMBER(9,0)`, `VARCHAR2(40 BYTE)`.
    pub written: String,
    /// The digits a decimal number type stores; `None` where it stores a number as given (`NUMBER`, `FLOAT`).
    pub decimal: Option<Decimal>,
}

impl DataType {
    /// A type of `family` with nothing more known about it.
    pub fn of(family: TypeFamily) -> Self {
        Self {
            family,
            written: String::new(),
            decimal: None,
        }
    }
}

/// `NUMBER(precision, scale)`: values are rounded to `scale` digits after the point (before it when negative), and
/// may have at most `precision` significant digits from there. `NUMBER(*, s)`, `INTEGER` and `DECIMAL` have a scale
/// but no precision of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decimal {
    pub precision: Option<u8>,
    pub scale: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstraintState {
    /// `DISABLE` makes this false; a disabled constraint is never checked.
    pub enabled: bool,
    /// `NOVALIDATE` makes this false: rows already in the table when it is added are not checked.
    pub validated: bool,
    pub initially_deferred: bool,
}

impl Default for ConstraintState {
    fn default() -> Self {
        Self {
            enabled: true,
            validated: true,
            initially_deferred: false,
        }
    }
}

/// A change to an existing constraint's state; `None` leaves that switch as it was. Deferrability itself can't change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateChange {
    pub enabled: Option<bool>,
    pub validated: Option<bool>,
    pub initially_deferred: Option<bool>,
}

impl ConstraintState {
    pub fn apply(&mut self, change: StateChange) {
        self.enabled = change.enabled.unwrap_or(self.enabled);
        self.validated = change.validated.unwrap_or(self.validated);
        self.initially_deferred = change.initially_deferred.unwrap_or(self.initially_deferred);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstraintKind {
    PrimaryKey {
        columns: Vec<Ident<'static>>,
    },
    Unique {
        columns: Vec<Ident<'static>>,
    },
    ForeignKey {
        columns: Vec<Ident<'static>>,
        parent: Name<'static>,
        /// `None`: no column list, which means the parent's primary key.
        parent_columns: Option<Vec<Ident<'static>>>,
    },
    NotNull {
        column: Ident<'static>,
    },
    /// Kept as written; checking CHECK constraints is a later milestone.
    Check {
        condition: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintDef {
    pub name: Option<Ident<'static>>,
    pub kind: ConstraintKind,
    pub state: ConstraintState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: Ident<'static>,
    pub data_type: DataType,
    pub default: Option<Expr<'static>>,
    /// `DEFAULT ON NULL`: an explicit NULL takes the default.
    pub default_on_null: bool,
    /// Identity and invisible columns are noted so the checker can warn; their values are not checked.
    pub identity: bool,
    pub invisible: bool,
    /// Constraints written on the column itself, NOT NULL included.
    pub constraints: Vec<ConstraintDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlterAction {
    AddConstraint(ConstraintDef),
    AddColumns(Vec<ColumnDef>),
    /// `MODIFY (col NOT NULL …)` and the like: the constraints the modified columns gain.
    ModifyColumns(Vec<(Ident<'static>, Vec<ConstraintDef>)>),
    /// `MODIFY col GENERATED … AS IDENTITY …`. Oracle accepts it only on an identity column, so it changes no more
    /// than the next generated value unless it changes whether an explicit NULL takes one.
    Identity {
        column: Ident<'static>,
        on_null: bool,
    },
    /// `ENABLE` / `DISABLE` / `MODIFY CONSTRAINT name …`: what changes in a constraint's state.
    ChangeConstraint {
        name: Ident<'static>,
        change: StateChange,
    },
    /// Anything else, as written: storage, renames, drops.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexColumn {
    Column(Ident<'static>),
    /// A function-based index part, as written; a unique index with one is not checked.
    Expression(String),
}

/// A value expression in an `INSERT`, as written. Evaluating it is the dialect's job (`to_date` is Oracle's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr<'a> {
    Null,
    /// A numeric literal as written: `100`, `1.0E2`, `.5`.
    Number(Cow<'a, str>),
    /// A string literal's content, quotes removed and doubled quotes undone, as stored (not decoded).
    String {
        bytes: Cow<'a, [u8]>,
        national: bool,
    },
    /// `DATE '…'`, `TIMESTAMP '…'`.
    Typed {
        type_name: Cow<'a, str>,
        text: Cow<'a, str>,
    },
    /// A function call such as `to_date('17-JUN-03','DD-MON-RR')`; the name as an identifier, so folded.
    Call {
        function: Cow<'a, str>,
        args: Vec<Expr<'a>>,
    },
    Negate(Box<Expr<'a>>),
    /// `seq.NEXTVAL`.
    NextValue(Name<'a>),
    /// The `DEFAULT` keyword as a value.
    Default,
    /// Anything else, as written: the checker treats it as unknown.
    Other(Cow<'a, str>),
}

impl Expr<'_> {
    pub fn into_owned(self) -> Expr<'static> {
        let owned = |text: Cow<'_, str>| Cow::Owned(text.into_owned());
        match self {
            Self::Null => Expr::Null,
            Self::Number(text) => Expr::Number(owned(text)),
            Self::String { bytes, national } => Expr::String {
                bytes: Cow::Owned(bytes.into_owned()),
                national,
            },
            Self::Typed { type_name, text } => Expr::Typed {
                type_name: owned(type_name),
                text: owned(text),
            },
            Self::Call { function, args } => Expr::Call {
                function: owned(function),
                args: args.into_iter().map(Expr::into_owned).collect(),
            },
            Self::Negate(inner) => Expr::Negate(Box::new(inner.into_owned())),
            Self::NextValue(name) => Expr::NextValue(name.into_owned()),
            Self::Default => Expr::Default,
            Self::Other(text) => Expr::Other(owned(text)),
        }
    }
}

/// One value in a row, with where it sits in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value<'a> {
    pub expr: Expr<'a>,
    pub span: Span,
}

/// One row an `INSERT` adds. A multi-table insert (`INSERT ALL`) has one per `INTO` clause, each with its own table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertRow<'a> {
    pub table: Name<'a>,
    /// `None`: no column list, so `values` follow the table's column order.
    pub columns: Option<Vec<Ident<'a>>>,
    pub values: Vec<Value<'a>>,
    /// The row's clause in the file (the whole statement, or one `INTO … VALUES (…)`), and the line it starts on.
    pub span: Span,
    pub line: u64,
}

impl InsertRow<'_> {
    pub fn into_owned(self) -> InsertRow<'static> {
        InsertRow {
            table: self.table.into_owned(),
            columns: self
                .columns
                .map(|columns| columns.into_iter().map(Ident::into_owned).collect()),
            values: self
                .values
                .into_iter()
                .map(|v| Value {
                    expr: v.expr.into_owned(),
                    span: v.span,
                })
                .collect(),
            span: self.span,
            line: self.line,
        }
    }
}

/// Why a statement was not parsed into something the checker uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    /// A statement the checker has no use for: `COMMIT`, `GRANT`, `CREATE VIEW`, …, named by its first words.
    Unused(String),
    /// A statement that changes data in a way the checker doesn't follow: `UPDATE`, `DELETE`, `MERGE`,
    /// `INSERT … SELECT`. Reported, since the end state may differ from what the checker sees.
    NotFollowed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A statement; only an insert's rows borrow from the text they were read from.
pub enum Statement<'a> {
    CreateTable {
        name: Name<'static>,
        columns: Vec<ColumnDef>,
        constraints: Vec<ConstraintDef>,
    },
    AlterTable {
        table: Name<'static>,
        actions: Vec<AlterAction>,
    },
    CreateIndex {
        name: Name<'static>,
        table: Name<'static>,
        unique: bool,
        columns: Vec<IndexColumn>,
    },
    Insert(Vec<InsertRow<'a>>),
    /// The end of a transaction: a commit, or a statement the dialect commits before and after, such as DDL.
    Commit,
    Skipped(Skip),
}

impl Statement<'_> {
    /// The statement with nothing borrowed, to keep.
    pub fn into_owned(self) -> Statement<'static> {
        match self {
            Self::CreateTable {
                name,
                columns,
                constraints,
            } => Statement::CreateTable {
                name,
                columns,
                constraints,
            },
            Self::AlterTable { table, actions } => Statement::AlterTable { table, actions },
            Self::CreateIndex {
                name,
                table,
                unique,
                columns,
            } => Statement::CreateIndex {
                name,
                table,
                unique,
                columns,
            },
            Self::Insert(rows) => {
                Statement::Insert(rows.into_iter().map(InsertRow::into_owned).collect())
            }
            Self::Commit => Statement::Commit,
            Self::Skipped(skip) => Statement::Skipped(skip),
        }
    }
}

/// A statement that could not be read, with where the reading stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: u64,
    pub line: u64,
    pub message: String,
}
