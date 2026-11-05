//! The statements a dialect's parser produces, in neutral terms: what the catalog and the checker need, and where
//! in the file each value sits, for the fix pass. A dialect parses its own syntax into these; nothing here names one.

use crate::source::Span;

/// An identifier as the dialect resolves it: an unquoted one already folded by the dialect's rules, a quoted one as
/// written (without its quotes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ident {
    pub value: String,
    pub quoted: bool,
}

/// A possibly schema-qualified name, as written.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name {
    pub schema: Option<Ident>,
    pub name: Ident,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four independent Oracle constraint state switches"
)]
pub struct ConstraintState {
    /// `DISABLE` makes this false; a disabled constraint is never checked.
    pub enabled: bool,
    /// `NOVALIDATE` makes this false: rows already in the table when it is added are not checked.
    pub validated: bool,
    pub deferrable: bool,
    pub initially_deferred: bool,
}

impl Default for ConstraintState {
    fn default() -> Self {
        Self {
            enabled: true,
            validated: true,
            deferrable: false,
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
        columns: Vec<Ident>,
    },
    Unique {
        columns: Vec<Ident>,
    },
    ForeignKey {
        columns: Vec<Ident>,
        parent: Name,
        /// `None`: no column list, which means the parent's primary key.
        parent_columns: Option<Vec<Ident>>,
    },
    NotNull {
        column: Ident,
    },
    /// Kept as written; checking CHECK constraints is a later milestone.
    Check {
        condition: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintDef {
    pub name: Option<Ident>,
    pub kind: ConstraintKind,
    pub state: ConstraintState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnDefault {
    /// An expression the checker can evaluate, such as a literal.
    Value(Expr),
    /// One it can't, such as `SYSTIMESTAMP` or a sequence.
    Opaque(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: Ident,
    pub data_type: DataType,
    pub default: Option<ColumnDefault>,
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
    ModifyColumns(Vec<(Ident, Vec<ConstraintDef>)>),
    /// `ENABLE` / `DISABLE` / `MODIFY CONSTRAINT name …`: what changes in a constraint's state.
    ChangeConstraint {
        name: Ident,
        change: StateChange,
    },
    /// Anything else, as written: storage, renames, drops.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexColumn {
    Column(Ident),
    /// A function-based index part, as written; a unique index with one is not checked.
    Expression(String),
}

/// A value expression in an `INSERT`, as written. Evaluating it is the dialect's job (`to_date` is Oracle's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Null,
    /// A numeric literal as written: `100`, `1.0E2`, `.5`.
    Number(String),
    /// A string literal's content, quotes removed and doubled quotes undone, as stored (not decoded).
    String {
        bytes: Vec<u8>,
        national: bool,
    },
    /// `DATE '…'`, `TIMESTAMP '…'`.
    Typed {
        type_name: String,
        text: String,
    },
    /// A function call such as `to_date('17-JUN-03','DD-MON-RR')`; the name upper-cased.
    Call {
        function: String,
        args: Vec<Expr>,
    },
    Negate(Box<Expr>),
    /// `seq.NEXTVAL`.
    NextValue(Name),
    /// The `DEFAULT` keyword as a value.
    Default,
    /// Anything else, as written: the checker treats it as unknown.
    Other(String),
}

/// One value in a row, with where it sits in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub expr: Expr,
    pub span: Span,
}

/// One row an `INSERT` adds. A multi-table insert (`INSERT ALL`) has one per `INTO` clause, each with its own table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertRow {
    pub table: Name,
    /// `None`: no column list, so `values` follow the table's column order.
    pub columns: Option<Vec<Ident>>,
    pub values: Vec<Value>,
    /// The row's clause in the file (the whole statement, or one `INTO … VALUES (…)`), and the line it starts on.
    pub span: Span,
    pub line: u64,
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
pub enum Statement {
    CreateTable {
        name: Name,
        columns: Vec<ColumnDef>,
        constraints: Vec<ConstraintDef>,
    },
    AlterTable {
        table: Name,
        actions: Vec<AlterAction>,
    },
    CreateIndex {
        name: Name,
        table: Name,
        unique: bool,
        columns: Vec<IndexColumn>,
    },
    Insert(Vec<InsertRow>),
    Skipped(Skip),
}

/// A statement that could not be read, with where the reading stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: u64,
    pub line: u64,
    pub message: String,
}
