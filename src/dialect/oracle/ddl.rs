//! Oracle DDL into neutral statements: `CREATE TABLE`, `ALTER TABLE` and `CREATE INDEX`, as far as the catalog needs
//! them. Physical attributes (storage, tablespaces, partitions, LOB clauses) are skipped by balancing parentheses.

use super::cursor::{Cursor, leading_words};
use super::lex::TokenKind;
use crate::source::{Encoding, Piece, PieceKind};
use crate::statement::{
    AlterAction, ColumnDef, ColumnDefault, ConstraintDef, ConstraintKind, ConstraintState,
    DataType, Decimal, Expr, Ident, IndexColumn, ParseError, Skip, StateChange, Statement,
    TypeFamily,
};

/// The statement a table or index DDL piece makes, or `None` when the piece is something else.
pub fn parse_ddl(piece: &Piece, encoding: Encoding) -> Result<Option<Statement>, ParseError> {
    if piece.kind != PieceKind::Sql || !is_table_ddl(&piece.text) {
        return Ok(None);
    }
    let mut c = Cursor::new(piece, encoding)?;
    if c.eat_words(&["ALTER", "TABLE"]) {
        return alter_table(&mut c).map(Some);
    }
    if !c.eat_word("CREATE") {
        return Ok(None);
    }
    if c.eat_word("UNIQUE") {
        return create_index(&mut c, true).map(Some);
    }
    c.eat_any_word(INDEX_MODIFIERS);
    if c.at_word("INDEX") {
        return create_index(&mut c, false).map(Some);
    }
    while c.eat_any_word(TABLE_MODIFIERS) {}
    if c.eat_word("TABLE") {
        return create_table(&mut c).map(Some);
    }
    Ok(None)
}

/// Whether a statement is table or index DDL, by its first words, without lexing it.
pub(crate) fn is_table_ddl(text: &[u8]) -> bool {
    let words = leading_words(text, 4);
    let mut words = words.iter().map(String::as_str);
    match words.next() {
        Some("ALTER") => words.next() == Some("TABLE"),
        Some("CREATE") => words
            .find(|w| !TABLE_MODIFIERS.contains(w) && !INDEX_MODIFIERS.contains(w))
            .is_some_and(|w| w == "TABLE" || w == "INDEX"),
        _ => false,
    }
}

const INDEX_MODIFIERS: &[&str] = &["UNIQUE", "BITMAP", "MULTIVALUE"];

const TABLE_MODIFIERS: &[&str] = &[
    "GLOBAL",
    "PRIVATE",
    "TEMPORARY",
    "SHARDED",
    "DUPLICATED",
    "IMMUTABLE",
    "BLOCKCHAIN",
];

fn create_table(c: &mut Cursor) -> Result<Statement, ParseError> {
    c.eat_words(&["IF", "NOT", "EXISTS"]);
    let name = c.name("a table name")?;
    if c.at_word("OF") {
        return Err(
            c.error("object tables (`CREATE TABLE … OF type`) are not supported".to_owned())
        );
    }
    let mut columns = Vec::new();
    let mut constraints = Vec::new();
    if c.eat_punct(b'(') {
        loop {
            if at_constraint_start(c) {
                constraints.push(out_of_line_constraint(c)?);
            } else if c.at_any_word(&["SUPPLEMENTAL", "PERIOD"]) {
                c.skip_item()?;
            } else {
                columns.push(column_def(c)?);
            }
            if !c.eat_punct(b',') {
                c.expect_punct(b')')?;
                break;
            }
        }
    }
    while !c.at_end() {
        let subquery = |ahead| c.word_at(ahead, "SELECT") || c.word_at(ahead, "WITH");
        if c.at_word("AS") && (subquery(1) || c.punct_at(1, b'(') && subquery(2)) {
            return Ok(Statement::Skipped(Skip::NotFollowed(
                "CREATE TABLE … AS SELECT".to_owned(),
            )));
        }
        c.skip_token()?;
    }
    if columns.is_empty() {
        return Err(c.error("a `CREATE TABLE` without a column list".to_owned()));
    }
    Ok(Statement::CreateTable {
        name,
        columns,
        constraints,
    })
}

fn at_constraint_start(c: &Cursor) -> bool {
    c.at_any_word(&["CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN", "CHECK"])
}

/// Words that end a column's type or default and start one of its other clauses.
const COLUMN_CLAUSES: &[&str] = &[
    "SORT",
    "VISIBLE",
    "INVISIBLE",
    "COLLATE",
    "DEFAULT",
    "GENERATED",
    "AS",
    "CONSTRAINT",
    "NOT",
    "NULL",
    "PRIMARY",
    "UNIQUE",
    "REFERENCES",
    "CHECK",
    "ENABLE",
    "DISABLE",
];

/// A column definition; the type may be missing (`CREATE TABLE … (a, b) AS SELECT`).
fn column_def(c: &mut Cursor) -> Result<ColumnDef, ParseError> {
    let name = c.ident("a column name")?;
    let data_type = if c.at_item_end() || c.at_any_word(COLUMN_CLAUSES) {
        DataType::of(TypeFamily::Other)
    } else {
        data_type(c)?
    };
    let mut column = ColumnDef {
        name,
        data_type,
        default: None,
        default_on_null: false,
        identity: false,
        invisible: false,
        constraints: Vec::new(),
    };
    while !c.at_item_end() {
        if c.eat_word("SORT") || c.eat_word("VISIBLE") {
        } else if c.eat_word("INVISIBLE") {
            column.invisible = true;
        } else if c.eat_word("COLLATE") {
            c.ident("a collation")?;
        } else if c.eat_word("DEFAULT") {
            column.default_on_null = c.eat_words(&["ON", "NULL"]);
            if column.default_on_null {
                c.eat_words(&["FOR", "INSERT", "ONLY"]);
                c.eat_words(&["FOR", "INSERT", "AND", "UPDATE"]);
            }
            column.default = Some(default_value(c)?);
        } else if c.eat_word("GENERATED") {
            // `BY DEFAULT ON NULL`: an explicit NULL takes a generated value, like `DEFAULT ON NULL`.
            if !c.eat_word("ALWAYS") && c.eat_words(&["BY", "DEFAULT"]) {
                column.default_on_null = c.eat_words(&["ON", "NULL"]);
            }
            c.expect_word("AS")?;
            if c.eat_word("IDENTITY") {
                column.identity = true;
                if c.at_punct(b'(') {
                    c.skip_parens()?;
                }
            } else {
                virtual_expression(c)?;
            }
        } else if c.eat_word("AS") {
            virtual_expression(c)?;
        } else if let Some(constraint) = inline_constraint(c, &column.name)? {
            column.constraints.push(constraint);
        }
    }
    Ok(column)
}

fn virtual_expression(c: &mut Cursor) -> Result<(), ParseError> {
    c.skip_parens()?;
    c.eat_word("VIRTUAL");
    Ok(())
}

/// The family of the type a column is declared with, and the type written in one canonical spelling.
fn data_type(c: &mut Cursor) -> Result<DataType, ParseError> {
    let start = c.pos();
    let first = c.word();
    let family = match first.as_deref() {
        Some(
            "NUMBER" | "FLOAT" | "INTEGER" | "INT" | "SMALLINT" | "DECIMAL" | "DEC" | "NUMERIC"
            | "REAL" | "DOUBLE" | "BINARY_FLOAT" | "BINARY_DOUBLE",
        ) => TypeFamily::Number,
        Some("VARCHAR2" | "NVARCHAR2" | "VARCHAR") => TypeFamily::Text,
        Some("CHAR" | "CHARACTER" | "NCHAR" | "NATIONAL") => TypeFamily::FixedText,
        Some("DATE") => TypeFamily::Date,
        Some("TIMESTAMP") => TypeFamily::Timestamp,
        Some("RAW") => TypeFamily::Binary,
        _ => TypeFamily::Other,
    };
    c.name("a column type")?;
    let mut family = family;
    // Oracle gives these scale 0 and no precision of their own unless given them.
    let mut decimal = matches!(
        first.as_deref(),
        Some("INTEGER" | "INT" | "SMALLINT" | "DECIMAL" | "DEC" | "NUMERIC")
    )
    .then_some(Decimal {
        precision: None,
        scale: 0,
    });
    let sized = matches!(
        first.as_deref(),
        Some("NUMBER" | "DECIMAL" | "DEC" | "NUMERIC")
    );
    loop {
        if c.at_punct(b'(') {
            let open = c.pos();
            c.skip_parens()?;
            if sized {
                decimal = decimal_args(c, open + 1, c.pos() - 1).or(decimal);
            }
        } else if c.eat_any_word(&[
            "PRECISION",
            "CHAR",
            "CHARACTER",
            "YEAR",
            "DAY",
            "MONTH",
            "SECOND",
        ]) {
        } else if c.eat_word("VARYING") {
            family = TypeFamily::Text;
        } else if c.eat_word("TO") {
        } else if c.eat_word("RAW") {
            family = TypeFamily::Other;
        } else if c.eat_word("WITH") {
            c.eat_word("LOCAL");
            c.expect_word("TIME")?;
            c.expect_word("ZONE")?;
            family = TypeFamily::Other;
        } else {
            break;
        }
    }
    if first.as_deref() == Some("LONG") || first.as_deref() == Some("INTERVAL") {
        family = TypeFamily::Other;
    }
    Ok(DataType {
        family,
        written: written_type(c, start),
        decimal,
    })
}

/// `(p)`, `(p, s)` or `(*, s)` from the tokens `from..to` inside a number type's parentheses.
fn decimal_args(c: &Cursor, from: usize, to: usize) -> Option<Decimal> {
    let text: String = (from..to)
        .filter_map(|i| c.token(i))
        .map(|t| c.token_text(t))
        .collect();
    let (precision, scale) = text.split_once(',').unwrap_or((&text, "0"));
    let precision = if precision == "*" {
        None
    } else {
        Some(precision.parse().ok()?)
    };
    Some(Decimal {
        precision,
        scale: scale.parse().ok()?,
    })
}

/// The tokens from `start` to the cursor, words upper-cased, one space between words and none around punctuation.
fn written_type(c: &Cursor, start: usize) -> String {
    let mut out = String::new();
    let mut tight = true;
    for index in start..c.pos() {
        let Some(token) = c.token(index) else { break };
        let text = token.text(c.source());
        let punct = token.kind == TokenKind::Punct;
        if !out.is_empty() && !tight && !punct {
            out.push(' ');
        }
        let text = c.decode(text);
        if token.kind == TokenKind::Word {
            out.push_str(&text.to_ascii_uppercase());
        } else {
            out.push_str(&text);
        }
        tight = punct && text != ")";
    }
    out
}

/// A default value, up to the column's next clause.
fn default_value(c: &mut Cursor) -> Result<ColumnDefault, ParseError> {
    let start = c.pos();
    while !c.at_item_end() {
        let at_clause = c.at_any_word(&[
            "CONSTRAINT",
            "NOT",
            "PRIMARY",
            "UNIQUE",
            "REFERENCES",
            "CHECK",
            "ENABLE",
            "DISABLE",
            "GENERATED",
            "VISIBLE",
            "INVISIBLE",
        ]) || (c.pos() > start && c.at_word("NULL"));
        if at_clause {
            break;
        }
        c.skip_token()?;
    }
    let end = c.pos();
    if end == start {
        return Err(c.expected("a default value"));
    }
    Ok(literal(c, start, end).map_or_else(
        || ColumnDefault::Opaque(c.text(start, end)),
        ColumnDefault::Value,
    ))
}

/// The tokens from `start` to `end` as a literal, when they are one.
fn literal(c: &Cursor, start: usize, end: usize) -> Option<Expr> {
    let tokens: Vec<_> = (start..end).filter_map(|i| c.token(i)).collect();
    let source = c.source();
    let is_word = |t: &super::lex::Token, w: &str| {
        t.kind == TokenKind::Word && t.text(source).eq_ignore_ascii_case(w.as_bytes())
    };
    match tokens.as_slice() {
        [t] if is_word(t, "NULL") => Some(Expr::Null),
        [t] if t.kind == TokenKind::Number => Some(Expr::Number(c.decode(t.text(source)))),
        [t] => match t.kind {
            TokenKind::String { national } | TokenKind::AltString { national } => {
                Some(Expr::String {
                    bytes: t.content(source),
                    national,
                })
            }
            _ => None,
        },
        [minus, t]
            if minus.kind == TokenKind::Punct
                && minus.text(source) == b"-"
                && t.kind == TokenKind::Number =>
        {
            Some(Expr::Negate(Box::new(Expr::Number(
                c.decode(t.text(source)),
            ))))
        }
        [ty, t]
            if (is_word(ty, "DATE") || is_word(ty, "TIMESTAMP"))
                && matches!(t.kind, TokenKind::String { .. }) =>
        {
            Some(Expr::Typed {
                type_name: c.decode(ty.text(source)).to_ascii_uppercase(),
                text: c.decode(&t.content(source)),
            })
        }
        _ => None,
    }
}

/// A constraint on a column; `None` for a bare `NULL`, which only says the column is nullable.
fn inline_constraint(c: &mut Cursor, column: &Ident) -> Result<Option<ConstraintDef>, ParseError> {
    let name = constraint_name(c)?;
    let columns = vec![column.clone()];
    let kind = if c.eat_words(&["NOT", "NULL"]) {
        ConstraintKind::NotNull {
            column: column.clone(),
        }
    } else if c.eat_word("NULL") {
        constraint_state(c)?;
        return Ok(None);
    } else if c.eat_words(&["PRIMARY", "KEY"]) {
        ConstraintKind::PrimaryKey { columns }
    } else if c.eat_word("UNIQUE") {
        ConstraintKind::Unique { columns }
    } else if c.at_word("REFERENCES") {
        references(c, columns)?
    } else if c.eat_word("CHECK") {
        ConstraintKind::Check {
            condition: c.paren_text()?,
        }
    } else {
        return Err(c.expected("a column clause or `,`"));
    };
    let state = constraint_state(c)?.into_state();
    Ok(Some(ConstraintDef { name, kind, state }))
}

fn constraint_name(c: &mut Cursor) -> Result<Option<Ident>, ParseError> {
    if c.eat_word("CONSTRAINT") {
        c.ident("a constraint name").map(Some)
    } else {
        Ok(None)
    }
}

fn out_of_line_constraint(c: &mut Cursor) -> Result<ConstraintDef, ParseError> {
    let name = constraint_name(c)?;
    let kind = if c.eat_words(&["PRIMARY", "KEY"]) {
        ConstraintKind::PrimaryKey {
            columns: column_list(c)?,
        }
    } else if c.eat_word("UNIQUE") {
        ConstraintKind::Unique {
            columns: column_list(c)?,
        }
    } else if c.eat_words(&["FOREIGN", "KEY"]) {
        let columns = column_list(c)?;
        references(c, columns)?
    } else if c.eat_word("CHECK") {
        ConstraintKind::Check {
            condition: c.paren_text()?,
        }
    } else {
        return Err(c.expected("`PRIMARY KEY`, `UNIQUE`, `FOREIGN KEY` or `CHECK`"));
    };
    let state = constraint_state(c)?.into_state();
    Ok(ConstraintDef { name, kind, state })
}

fn column_list(c: &mut Cursor) -> Result<Vec<Ident>, ParseError> {
    c.expect_punct(b'(')?;
    let mut columns = vec![c.ident("a column name")?];
    while c.eat_punct(b',') {
        columns.push(c.ident("a column name")?);
    }
    c.expect_punct(b')')?;
    Ok(columns)
}

/// `REFERENCES parent [(columns)] [ON DELETE …]`.
fn references(c: &mut Cursor, columns: Vec<Ident>) -> Result<ConstraintKind, ParseError> {
    c.expect_word("REFERENCES")?;
    let parent = c.name("the parent table")?;
    let parent_columns = if c.at_punct(b'(') {
        Some(column_list(c)?)
    } else {
        None
    };
    if c.eat_words(&["ON", "DELETE"]) && !c.eat_word("CASCADE") {
        c.expect_word("SET")?;
        c.expect_word("NULL")?;
    }
    Ok(ConstraintKind::ForeignKey {
        columns,
        parent,
        parent_columns,
    })
}

/// The state words after a constraint, as written; what isn't written stays `None`.
#[derive(Default)]
struct WrittenState {
    enabled: Option<bool>,
    validated: Option<bool>,
    deferrable: Option<bool>,
    initially_deferred: Option<bool>,
}

impl WrittenState {
    /// `ENABLE` means `ENABLE VALIDATE` and `DISABLE` means `DISABLE NOVALIDATE`, unless written otherwise.
    fn validated(&self) -> Option<bool> {
        self.validated.or(self.enabled)
    }

    fn into_state(self) -> ConstraintState {
        let enabled = self.enabled.unwrap_or(true);
        let initially_deferred = self.initially_deferred.unwrap_or(false);
        ConstraintState {
            enabled,
            validated: self.validated().unwrap_or(enabled),
            deferrable: self.deferrable.unwrap_or(initially_deferred),
            initially_deferred,
        }
    }

    fn into_change(self) -> StateChange {
        StateChange {
            enabled: self.enabled,
            validated: self.validated(),
            initially_deferred: self.initially_deferred,
        }
    }
}

fn constraint_state(c: &mut Cursor) -> Result<WrittenState, ParseError> {
    let mut state = WrittenState::default();
    loop {
        if c.eat_word("ENABLE") {
            state.enabled = Some(true);
        } else if c.eat_word("DISABLE") {
            state.enabled = Some(false);
        } else if c.eat_word("VALIDATE") {
            state.validated = Some(true);
        } else if c.eat_word("NOVALIDATE") {
            state.validated = Some(false);
        } else if c.eat_word("DEFERRABLE") {
            state.deferrable = Some(true);
        } else if c.eat_words(&["NOT", "DEFERRABLE"]) {
            state.deferrable = Some(false);
        } else if c.eat_words(&["INITIALLY", "DEFERRED"]) {
            state.initially_deferred = Some(true);
        } else if c.eat_words(&["INITIALLY", "IMMEDIATE"]) {
            state.initially_deferred = Some(false);
        } else if c.eat_word("RELY") || c.eat_word("NORELY") {
        } else if c.eat_words(&["USING", "INDEX"]) {
            skip_index_clause(c)?;
        } else if c.eat_words(&["EXCEPTIONS", "INTO"]) {
            c.name("an exceptions table")?;
        } else {
            return Ok(state);
        }
    }
}

/// `USING INDEX` takes an index name, a `(CREATE INDEX …)` or index attributes; none of them matter here.
fn skip_index_clause(c: &mut Cursor) -> Result<(), ParseError> {
    while !c.at_item_end()
        && !c.at_any_word(&[
            "ENABLE",
            "DISABLE",
            "VALIDATE",
            "NOVALIDATE",
            "DEFERRABLE",
            "NOT",
            "INITIALLY",
            "RELY",
            "NORELY",
            "EXCEPTIONS",
            "CONSTRAINT",
            "PRIMARY",
            "UNIQUE",
            "REFERENCES",
            "CHECK",
            "NULL",
            "ADD",
            "MODIFY",
        ])
    {
        c.skip_token()?;
    }
    Ok(())
}

fn alter_table(c: &mut Cursor) -> Result<Statement, ParseError> {
    let table = c.name("a table name")?;
    let mut actions = Vec::new();
    while !c.at_end() {
        let start = c.pos();
        if c.at_word("ADD") && !ADD_OTHER.iter().any(|w| c.word_at(1, w)) {
            c.bump();
            add(c, &mut actions)?;
        } else if c.eat_words(&["MODIFY", "CONSTRAINT"]) {
            let name = c.ident("a constraint name")?;
            let change = constraint_state(c)?.into_change();
            actions.push(AlterAction::ChangeConstraint { name, change });
        } else if c.at_word("MODIFY")
            && !c.at_words(&["MODIFY", "PRIMARY"])
            && !c.at_words(&["MODIFY", "UNIQUE"])
        {
            c.bump();
            actions.push(modify(c, start)?);
        } else if let Some(action) = enable_constraint(c)? {
            actions.push(action);
        } else {
            while c.bump().is_some() {}
            actions.push(AlterAction::Other(c.text(start, c.pos())));
        }
    }
    if actions.is_empty() {
        return Err(c.expected("an `ALTER TABLE` action"));
    }
    Ok(Statement::AlterTable { table, actions })
}

/// `ADD` clauses that add neither columns nor constraints.
const ADD_OTHER: &[&str] = &[
    "SUPPLEMENTAL",
    "PERIOD",
    "PARTITION",
    "SUBPARTITION",
    "OVERFLOW",
];

/// `ADD` a constraint, or columns and constraints in parentheses, or one column.
fn add(c: &mut Cursor, actions: &mut Vec<AlterAction>) -> Result<(), ParseError> {
    if at_constraint_start(c) {
        actions.push(AlterAction::AddConstraint(out_of_line_constraint(c)?));
        return Ok(());
    }
    if !c.eat_punct(b'(') {
        actions.push(AlterAction::AddColumns(vec![column_def(c)?]));
        return Ok(());
    }
    let mut columns = Vec::new();
    loop {
        if at_constraint_start(c) {
            actions.push(AlterAction::AddConstraint(out_of_line_constraint(c)?));
        } else {
            columns.push(column_def(c)?);
        }
        if !c.eat_punct(b',') {
            c.expect_punct(b')')?;
            break;
        }
    }
    if !columns.is_empty() {
        actions.push(AlterAction::AddColumns(columns));
    }
    Ok(())
}

/// `MODIFY` columns: the constraints they gain. Anything else a modification does (a type, a default, `NULL`) isn't
/// followed, so such a `MODIFY` is kept as written.
fn modify(c: &mut Cursor, start: usize) -> Result<AlterAction, ParseError> {
    let parenthesized = c.eat_punct(b'(');
    let mut items = Vec::new();
    let mut followed = true;
    loop {
        let column = c.ident("a column name")?;
        let mut constraints = Vec::new();
        while !c.at_item_end() {
            let at_constraint = c.at_any_word(&[
                "CONSTRAINT",
                "NOT",
                "PRIMARY",
                "UNIQUE",
                "REFERENCES",
                "CHECK",
            ]);
            match at_constraint
                .then(|| inline_constraint(c, &column))
                .transpose()?
            {
                Some(Some(constraint)) => constraints.push(constraint),
                Some(None) => {}
                None => {
                    followed = false;
                    c.skip_token()?;
                }
            }
        }
        items.push((column, constraints));
        if !parenthesized || !c.eat_punct(b',') {
            break;
        }
    }
    if parenthesized {
        c.expect_punct(b')')?;
    }
    Ok(if followed {
        AlterAction::ModifyColumns(items)
    } else {
        AlterAction::Other(c.text(start, c.pos()))
    })
}

/// `ENABLE|DISABLE [VALIDATE|NOVALIDATE] CONSTRAINT name …`; `None` when the cursor isn't at one.
fn enable_constraint(c: &mut Cursor) -> Result<Option<AlterAction>, ParseError> {
    let start = c.pos();
    let enabled = if c.eat_word("ENABLE") {
        true
    } else if c.eat_word("DISABLE") {
        false
    } else {
        return Ok(None);
    };
    let validated = if c.eat_word("VALIDATE") {
        Some(true)
    } else if c.eat_word("NOVALIDATE") {
        Some(false)
    } else {
        None
    };
    if !c.eat_word("CONSTRAINT") {
        c.restore(start);
        return Ok(None);
    }
    let name = c.ident("a constraint name")?;
    loop {
        if c.eat_words(&["USING", "INDEX"]) {
            skip_index_clause(c)?;
        } else if c.eat_words(&["EXCEPTIONS", "INTO"]) {
            c.name("an exceptions table")?;
        } else if c.at_word("CASCADE") {
            // `CASCADE` also disables the foreign keys that depend on this key.
            while c.bump().is_some() {}
            return Ok(Some(AlterAction::Other(c.text(start, c.pos()))));
        } else if !(c.eat_words(&["KEEP", "INDEX"]) || c.eat_words(&["DROP", "INDEX"])) {
            break;
        }
    }
    Ok(Some(AlterAction::ChangeConstraint {
        name,
        change: StateChange {
            enabled: Some(enabled),
            validated: validated.or(Some(enabled)),
            initially_deferred: None,
        },
    }))
}

fn create_index(c: &mut Cursor, unique: bool) -> Result<Statement, ParseError> {
    c.expect_word("INDEX")?;
    c.eat_words(&["IF", "NOT", "EXISTS"]);
    let name = c.name("an index name")?;
    c.expect_word("ON")?;
    if c.at_word("CLUSTER") {
        return Ok(Statement::Skipped(Skip::Unused(
            "CREATE INDEX … ON CLUSTER".to_owned(),
        )));
    }
    let table = c.name("a table name")?;
    if !c.at_punct(b'(') {
        c.ident("a table alias")?;
    }
    c.expect_punct(b'(')?;
    let mut columns = Vec::new();
    loop {
        columns.push(index_column(c)?);
        if !c.eat_punct(b',') {
            c.expect_punct(b')')?;
            break;
        }
    }
    Ok(Statement::CreateIndex {
        name,
        table,
        unique,
        columns,
    })
}

/// A column, or an expression kept as written.
fn index_column(c: &mut Cursor) -> Result<IndexColumn, ParseError> {
    let start = c.pos();
    let plain = matches!(
        c.peek().map(|t| t.kind),
        Some(TokenKind::Word | TokenKind::QuotedIdent)
    );
    if plain {
        let column = c.ident("a column")?;
        // A descending part is an expression to Oracle (`SYS_OP_DESCEND`), so its NULLs conflict.
        c.eat_word("ASC");
        if c.at_item_end() {
            return Ok(IndexColumn::Column(column));
        }
    }
    c.skip_item()?;
    if c.pos() == start {
        return Err(c.expected("an index column"));
    }
    Ok(IndexColumn::Expression(c.text(start, c.pos())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Span;
    use crate::statement::Name;

    fn piece(text: &str) -> Piece {
        Piece {
            kind: PieceKind::Sql,
            span: Span {
                start: 100,
                end: 100 + u64::try_from(text.len()).unwrap(),
            },
            line: 10,
            text: text.as_bytes().to_vec(),
        }
    }

    fn parse(text: &str) -> Statement {
        parse_ddl(&piece(text), Encoding::Utf8).unwrap().unwrap()
    }

    fn error(text: &str) -> ParseError {
        parse_ddl(&piece(text), Encoding::Utf8).unwrap_err()
    }

    fn id(value: &str, quoted: bool) -> Ident {
        Ident {
            value: value.into(),
            quoted,
        }
    }

    fn q(value: &str) -> Ident {
        id(value, true)
    }

    fn name(schema: &str, table: &str) -> Name {
        Name {
            schema: Some(q(schema)),
            name: q(table),
        }
    }

    const ENFORCED: ConstraintState = ConstraintState {
        enabled: true,
        validated: true,
        deferrable: false,
        initially_deferred: false,
    };

    #[test]
    fn a_sql_developer_create_table_gives_columns_types_defaults_and_constraints() {
        let Statement::CreateTable {
            name: table,
            columns,
            constraints,
        } = parse(
            "CREATE TABLE \"APP\".\"EMPLOYEES\" \n   (\t\"EMP_ID\" NUMBER(9,0), \n\t\"EMAIL\" VARCHAR2(80 BYTE) NOT NULL ENABLE, \n\t\"CODE\" CHAR(3 BYTE) CONSTRAINT \"C_PK\" PRIMARY KEY, \n\t\"STATUS\" VARCHAR2(10 BYTE) DEFAULT 'ACTIVE' NOT NULL ENABLE, \n\t\"CREATED_AT\" TIMESTAMP (6) DEFAULT ON NULL SYSTIMESTAMP, \n\t CONSTRAINT \"EMPLOYEES_UK\" UNIQUE (\"EMP_ID\",\"CODE\")\n  USING INDEX PCTFREE 10 INITRANS 2 MAXTRANS 255 COMPUTE STATISTICS \n  TABLESPACE \"USERS\"  ENABLE\n   ) SEGMENT CREATION IMMEDIATE \n  PCTFREE 10 PCTUSED 40 NOCOMPRESS LOGGING\n  STORAGE(INITIAL 65536 NEXT 1048576 BUFFER_POOL DEFAULT)\n  TABLESPACE \"USERS\" ",
        )
        else {
            panic!("not a CREATE TABLE");
        };
        assert_eq!(table, name("APP", "EMPLOYEES"));
        let shape: Vec<(&str, TypeFamily, &str)> = columns
            .iter()
            .map(|c| {
                (
                    c.name.value.as_str(),
                    c.data_type.family,
                    c.data_type.written.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("EMP_ID", TypeFamily::Number, "NUMBER(9,0)"),
                ("EMAIL", TypeFamily::Text, "VARCHAR2(80 BYTE)"),
                ("CODE", TypeFamily::FixedText, "CHAR(3 BYTE)"),
                ("STATUS", TypeFamily::Text, "VARCHAR2(10 BYTE)"),
                ("CREATED_AT", TypeFamily::Timestamp, "TIMESTAMP(6)"),
            ]
        );
        assert_eq!(
            columns[1].constraints,
            [ConstraintDef {
                name: None,
                kind: ConstraintKind::NotNull { column: q("EMAIL") },
                state: ENFORCED,
            }]
        );
        assert_eq!(
            columns[2].constraints,
            [ConstraintDef {
                name: Some(q("C_PK")),
                kind: ConstraintKind::PrimaryKey {
                    columns: vec![q("CODE")]
                },
                state: ENFORCED,
            }]
        );
        assert_eq!(
            columns[3].default,
            Some(ColumnDefault::Value(Expr::String {
                bytes: b"ACTIVE".to_vec(),
                national: false
            }))
        );
        assert!(!columns[3].default_on_null);
        assert_eq!(
            columns[4].default,
            Some(ColumnDefault::Opaque("SYSTIMESTAMP".into()))
        );
        assert!(columns[4].default_on_null);
        assert_eq!(
            constraints,
            [ConstraintDef {
                name: Some(q("EMPLOYEES_UK")),
                kind: ConstraintKind::Unique {
                    columns: vec![q("EMP_ID"), q("CODE")]
                },
                state: ENFORCED,
            }]
        );
    }

    #[test]
    fn unquoted_names_fold_and_keywords_ignore_case() {
        let Statement::CreateTable { name, columns, .. } =
            parse("create table app.Emp (id number primary key, \"Mixed\" date)")
        else {
            panic!("not a CREATE TABLE");
        };
        assert_eq!(
            name,
            Name {
                schema: Some(id("APP", false)),
                name: id("EMP", false)
            }
        );
        assert_eq!(columns[0].name, id("ID", false));
        assert_eq!(columns[1].name, q("Mixed"));
        assert_eq!(columns[1].data_type.family, TypeFamily::Date);
    }

    #[test]
    fn types_have_families_and_one_spelling() {
        let cases = [
            ("NUMBER", TypeFamily::Number, "NUMBER"),
            ("number(*, 0)", TypeFamily::Number, "NUMBER(*,0)"),
            ("FLOAT(126)", TypeFamily::Number, "FLOAT(126)"),
            ("INTEGER", TypeFamily::Number, "INTEGER"),
            ("DOUBLE PRECISION", TypeFamily::Number, "DOUBLE PRECISION"),
            ("BINARY_DOUBLE", TypeFamily::Number, "BINARY_DOUBLE"),
            ("NVARCHAR2(20)", TypeFamily::Text, "NVARCHAR2(20)"),
            ("VARCHAR2(20 CHAR)", TypeFamily::Text, "VARCHAR2(20 CHAR)"),
            ("CHAR VARYING(5)", TypeFamily::Text, "CHAR VARYING(5)"),
            ("NCHAR(2)", TypeFamily::FixedText, "NCHAR(2)"),
            (
                "NATIONAL CHARACTER(2)",
                TypeFamily::FixedText,
                "NATIONAL CHARACTER(2)",
            ),
            ("DATE", TypeFamily::Date, "DATE"),
            ("TIMESTAMP", TypeFamily::Timestamp, "TIMESTAMP"),
            (
                "TIMESTAMP (6) WITH TIME ZONE",
                TypeFamily::Other,
                "TIMESTAMP(6) WITH TIME ZONE",
            ),
            (
                "TIMESTAMP WITH LOCAL TIME ZONE",
                TypeFamily::Other,
                "TIMESTAMP WITH LOCAL TIME ZONE",
            ),
            (
                "INTERVAL DAY (2) TO SECOND (6)",
                TypeFamily::Other,
                "INTERVAL DAY(2) TO SECOND(6)",
            ),
            (
                "INTERVAL YEAR TO MONTH",
                TypeFamily::Other,
                "INTERVAL YEAR TO MONTH",
            ),
            ("RAW(16)", TypeFamily::Binary, "RAW(16)"),
            ("LONG RAW", TypeFamily::Other, "LONG RAW"),
            ("LONG", TypeFamily::Other, "LONG"),
            ("CLOB", TypeFamily::Other, "CLOB"),
            (
                "MDSYS.SDO_GEOMETRY",
                TypeFamily::Other,
                "MDSYS.SDO_GEOMETRY",
            ),
            (
                "\"APP\".\"ADDRESS_T\"",
                TypeFamily::Other,
                "\"APP\".\"ADDRESS_T\"",
            ),
        ];
        for (written, family, spelled) in cases {
            let Statement::CreateTable { columns, .. } =
                parse(&format!("CREATE TABLE T (C {written}, D DATE)"))
            else {
                panic!("not a CREATE TABLE");
            };
            assert_eq!(columns.len(), 2, "{written}");
            assert_eq!(
                (
                    columns[0].data_type.family,
                    columns[0].data_type.written.as_str()
                ),
                (family, spelled),
                "{written}"
            );
        }
    }

    #[test]
    fn decimal_types_know_their_precision_and_scale() {
        type PrecisionScale = Option<(Option<u8>, i16)>;
        let cases: [(&str, PrecisionScale); 9] = [
            ("NUMBER(9,0)", Some((Some(9), 0))),
            ("NUMBER(9)", Some((Some(9), 0))),
            ("NUMBER(*, 2)", Some((None, 2))),
            ("NUMBER(2,-3)", Some((Some(2), -3))),
            ("NUMBER", None),
            ("INTEGER", Some((None, 0))),
            ("DECIMAL(10,2)", Some((Some(10), 2))),
            ("NUMERIC", Some((None, 0))),
            ("FLOAT(126)", None),
        ];
        for (written, expected) in cases {
            let Statement::CreateTable { columns, .. } =
                parse(&format!("CREATE TABLE T (C {written})"))
            else {
                panic!("not a CREATE TABLE");
            };
            let decimal = columns[0].data_type.decimal.map(|d| (d.precision, d.scale));
            assert_eq!(decimal, expected, "{written}");
        }
    }

    #[test]
    fn defaults_are_literals_or_kept_as_written() {
        let cases: [(&str, ColumnDefault); 6] = [
            ("DEFAULT 0", ColumnDefault::Value(Expr::Number("0".into()))),
            (
                "DEFAULT -1.5 NOT NULL",
                ColumnDefault::Value(Expr::Negate(Box::new(Expr::Number("1.5".into())))),
            ),
            ("DEFAULT NULL NULL", ColumnDefault::Value(Expr::Null)),
            (
                "DEFAULT DATE '2024-01-31'",
                ColumnDefault::Value(Expr::Typed {
                    type_name: "DATE".into(),
                    text: "2024-01-31".into(),
                }),
            ),
            (
                "DEFAULT \"APP\".\"S\".NEXTVAL",
                ColumnDefault::Opaque("\"APP\".\"S\".NEXTVAL".into()),
            ),
            (
                "DEFAULT TRUNC(SYSDATE) + 1 CONSTRAINT X CHECK (C > 0)",
                ColumnDefault::Opaque("TRUNC(SYSDATE) + 1".into()),
            ),
        ];
        for (written, expected) in cases {
            let Statement::CreateTable { columns, .. } =
                parse(&format!("CREATE TABLE T (C NUMBER {written})"))
            else {
                panic!("not a CREATE TABLE");
            };
            assert_eq!(columns[0].default.as_ref(), Some(&expected), "{written}");
        }
    }

    #[test]
    fn identity_invisible_and_virtual_columns_are_noted_or_skipped() {
        let Statement::CreateTable { columns, .. } = parse(
            "CREATE TABLE T (ID NUMBER GENERATED BY DEFAULT ON NULL AS IDENTITY (START WITH 1 CACHE 20) NOT NULL, \
             H VARCHAR2(10) INVISIBLE, V NUMBER GENERATED ALWAYS AS (ID * 2) VIRTUAL, W AS (ID + 1))",
        ) else {
            panic!("not a CREATE TABLE");
        };
        let flags: Vec<(&str, bool, bool, bool, usize)> = columns
            .iter()
            .map(|c| {
                (
                    c.name.value.as_str(),
                    c.identity,
                    c.default_on_null,
                    c.invisible,
                    c.constraints.len(),
                )
            })
            .collect();
        assert_eq!(
            flags,
            [
                ("ID", true, true, false, 1),
                ("H", false, false, true, 0),
                ("V", false, false, false, 0),
                ("W", false, false, false, 0)
            ]
        );
    }

    #[test]
    fn constraint_states_follow_oracle_defaults() {
        let state = |words: &str| {
            let Statement::AlterTable { actions, .. } = parse(&format!(
                "ALTER TABLE T ADD CONSTRAINT F FOREIGN KEY (A) REFERENCES P {words}"
            )) else {
                panic!("not an ALTER TABLE");
            };
            let [AlterAction::AddConstraint(def)] = actions.as_slice() else {
                panic!("{actions:?}");
            };
            let s = def.state;
            (s.enabled, s.validated, s.deferrable, s.initially_deferred)
        };
        assert_eq!(state(""), (true, true, false, false));
        assert_eq!(state("ENABLE"), (true, true, false, false));
        assert_eq!(state("ENABLE NOVALIDATE"), (true, false, false, false));
        assert_eq!(state("DISABLE"), (false, false, false, false));
        assert_eq!(state("DISABLE VALIDATE"), (false, true, false, false));
        assert_eq!(
            state("DEFERRABLE INITIALLY DEFERRED ENABLE"),
            (true, true, true, true)
        );
        assert_eq!(state("INITIALLY DEFERRED"), (true, true, true, true));
        assert_eq!(
            state("ON DELETE CASCADE RELY DISABLE NOVALIDATE"),
            (false, false, false, false)
        );
        assert_eq!(
            state("NOT DEFERRABLE USING INDEX \"APP\".\"X\" ENABLE"),
            (true, true, false, false)
        );
    }

    #[test]
    fn foreign_keys_name_their_parent_with_or_without_columns() {
        let Statement::AlterTable { table, actions } = parse(
            "ALTER TABLE \"APP\".\"TIMESHEETS\" ADD CONSTRAINT \"TS_FK\" FOREIGN KEY (\"EMP_ID\", \"PROJECT_CODE\")\n\t  REFERENCES \"APP\".\"ASSIGNMENTS\" (\"EMP_ID\", \"PROJECT_CODE\") ENABLE",
        ) else {
            panic!("not an ALTER TABLE");
        };
        assert_eq!(table, name("APP", "TIMESHEETS"));
        assert_eq!(
            actions,
            [AlterAction::AddConstraint(ConstraintDef {
                name: Some(q("TS_FK")),
                kind: ConstraintKind::ForeignKey {
                    columns: vec![q("EMP_ID"), q("PROJECT_CODE")],
                    parent: name("APP", "ASSIGNMENTS"),
                    parent_columns: Some(vec![q("EMP_ID"), q("PROJECT_CODE")]),
                },
                state: ENFORCED,
            })]
        );
        let Statement::CreateTable { columns, .. } =
            parse("CREATE TABLE C (P NUMBER REFERENCES PARENT ON DELETE SET NULL)")
        else {
            panic!("not a CREATE TABLE");
        };
        assert_eq!(
            columns[0].constraints[0].kind,
            ConstraintKind::ForeignKey {
                columns: vec![id("P", false)],
                parent: Name {
                    schema: None,
                    name: id("PARENT", false)
                },
                parent_columns: None,
            }
        );
    }

    #[test]
    fn alter_table_actions() {
        let actions = |text: &str| {
            let Statement::AlterTable { actions, .. } = parse(text) else {
                panic!("not an ALTER TABLE");
            };
            actions
        };
        let not_null = |c: &str| ConstraintDef {
            name: None,
            kind: ConstraintKind::NotNull { column: q(c) },
            state: ENFORCED,
        };
        assert_eq!(
            actions("ALTER TABLE \"APP\".\"T\" MODIFY (\"NAME\" NOT NULL ENABLE)"),
            [AlterAction::ModifyColumns(vec![(
                q("NAME"),
                vec![not_null("NAME")]
            )])]
        );
        assert_eq!(
            actions("ALTER TABLE T MODIFY \"A\" NOT NULL"),
            [AlterAction::ModifyColumns(vec![(
                q("A"),
                vec![not_null("A")]
            )])]
        );
        let Some(AlterAction::AddColumns(added)) =
            actions("ALTER TABLE \"APP\".\"T\" ADD (\"PHONE\" VARCHAR2(20 BYTE), \"FAX\" NUMBER)")
                .pop()
        else {
            panic!("no columns added");
        };
        assert_eq!(
            added
                .iter()
                .map(|c| c.name.value.as_str())
                .collect::<Vec<_>>(),
            ["PHONE", "FAX"]
        );
        assert!(
            matches!(actions("ALTER TABLE T ADD X DATE").as_slice(), [AlterAction::AddColumns(c)] if c.len() == 1)
        );
        assert!(matches!(
            actions("ALTER TABLE T ADD (CONSTRAINT A PRIMARY KEY (X), CONSTRAINT B UNIQUE (Y))")
                .as_slice(),
            [AlterAction::AddConstraint(_), AlterAction::AddConstraint(_)]
        ));
        assert_eq!(
            actions("ALTER TABLE T ENABLE NOVALIDATE CONSTRAINT \"F\" EXCEPTIONS INTO E"),
            [AlterAction::ChangeConstraint {
                name: q("F"),
                change: StateChange {
                    enabled: Some(true),
                    validated: Some(false),
                    initially_deferred: None
                },
            }]
        );
        assert_eq!(
            actions("ALTER TABLE T DISABLE CONSTRAINT F KEEP INDEX"),
            [AlterAction::ChangeConstraint {
                name: id("F", false),
                change: StateChange {
                    enabled: Some(false),
                    validated: Some(false),
                    initially_deferred: None
                },
            }]
        );
        assert_eq!(
            actions("ALTER TABLE T MODIFY CONSTRAINT F INITIALLY IMMEDIATE"),
            [AlterAction::ChangeConstraint {
                name: id("F", false),
                change: StateChange {
                    enabled: None,
                    validated: None,
                    initially_deferred: Some(false)
                },
            }]
        );
        for other in [
            "ALTER TABLE T MODIFY (A NULL)",
            "ALTER TABLE T MODIFY (A DEFAULT 5)",
            "ALTER TABLE T DROP CONSTRAINT F",
            "ALTER TABLE T ENABLE ROW MOVEMENT",
            "ALTER TABLE T ADD SUPPLEMENTAL LOG DATA (ALL) COLUMNS",
            "ALTER TABLE T MODIFY PRIMARY KEY DISABLE",
            "ALTER TABLE T DISABLE CONSTRAINT T_PK CASCADE",
        ] {
            assert!(
                matches!(actions(other).as_slice(), [AlterAction::Other(_)]),
                "{other}"
            );
        }
    }

    #[test]
    fn indexes_keep_plain_columns_and_expressions() {
        assert_eq!(
            parse(
                "CREATE UNIQUE INDEX \"APP\".\"E_UIX\" ON \"APP\".\"E\" (\"EMAIL\") PCTFREE 10 TABLESPACE \"USERS\""
            ),
            Statement::CreateIndex {
                name: name("APP", "E_UIX"),
                table: name("APP", "E"),
                unique: true,
                columns: vec![IndexColumn::Column(q("EMAIL"))],
            }
        );
        let Statement::CreateIndex {
            unique, columns, ..
        } = parse("create bitmap index i on t x (a asc, b desc, upper(\"B\"))")
        else {
            panic!("not an index");
        };
        assert!(!unique);
        assert_eq!(
            columns,
            [
                IndexColumn::Column(id("A", false)),
                IndexColumn::Expression("b desc".into()),
                IndexColumn::Expression("upper(\"B\")".into())
            ]
        );
    }

    #[test]
    fn other_statements_are_not_ddl() {
        for text in [
            "INSERT INTO T VALUES (1)",
            "  -- note\n/* x */ insert into t values (1)",
            "CREATE SEQUENCE S",
            "CREATE OR REPLACE VIEW V AS SELECT 1 FROM DUAL",
            "ALTER TRIGGER X ENABLE",
            "GRANT SELECT ON T TO U",
            "COMMIT",
        ] {
            assert_eq!(parse_ddl(&piece(text), Encoding::Utf8), Ok(None), "{text}");
        }
        let block = Piece {
            kind: PieceKind::Block,
            ..piece("CREATE TABLE X (A NUMBER)")
        };
        assert_eq!(parse_ddl(&block, Encoding::Utf8), Ok(None));
    }

    #[test]
    fn create_table_as_select_is_not_followed() {
        for text in [
            "CREATE TABLE T AS SELECT * FROM U",
            "CREATE TABLE T (A, B) AS SELECT 1, 2 FROM DUAL",
            "CREATE TABLE T (A NUMBER) TABLESPACE X AS (SELECT 1 FROM DUAL)",
        ] {
            assert!(
                matches!(parse(text), Statement::Skipped(Skip::NotFollowed(_))),
                "{text}"
            );
        }
        let lob = parse("CREATE TABLE T (A CLOB) LOB (A) STORE AS (TABLESPACE X)");
        assert!(matches!(lob, Statement::CreateTable { .. }));
    }

    #[test]
    fn errors_say_what_was_expected_and_where() {
        let e = error("CREATE TABLE T (\n  A NUMBER,\n  B NUMBER ENCRYPT\n)");
        assert_eq!(e.line, 12);
        assert_eq!(
            e.offset,
            100 + u64::try_from("CREATE TABLE T (\n  A NUMBER,\n  B NUMBER ".len()).unwrap()
        );
        assert_eq!(
            e.message,
            "expected a column clause or `,`, found `ENCRYPT`"
        );
        assert_eq!(
            error("CREATE TABLE T (A NUMBER(9").message,
            "this `(` is never closed"
        );
        assert_eq!(
            error("ALTER TABLE T ADD CONSTRAINT X").message,
            "expected `PRIMARY KEY`, `UNIQUE`, `FOREIGN KEY` or `CHECK`, found the end of the statement"
        );
        assert_eq!(
            error("CREATE TABLE T OF ADDRESS_T").message,
            "object tables (`CREATE TABLE … OF type`) are not supported"
        );
        assert_eq!(
            error("CREATE TABLE T (A NUMBER) AS 'x").message,
            "unterminated string"
        );
    }

    #[test]
    fn windows_1252_names_are_decoded() {
        let p = Piece {
            text: b"CREATE TABLE \"T\xe4\" (\"\xc4\" NUMBER, \xe5 DATE)".to_vec(),
            ..piece("")
        };
        let Some(Statement::CreateTable { name, columns, .. }) =
            parse_ddl(&p, Encoding::Windows1252).unwrap()
        else {
            panic!("not a CREATE TABLE");
        };
        assert_eq!(name.name.value, "Tä");
        assert_eq!(columns[0].name.value, "Ä");
        assert_eq!(columns[1].name, id("Å", false));
    }
}

#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::parse_ddl;
    use crate::source::{Encoding, Piece, PieceKind, Span};

    fn ddl() -> impl Strategy<Value = Vec<u8>> {
        let words = prop::sample::select(vec![
            "CREATE ",
            "TABLE ",
            "ALTER ",
            "INDEX ",
            "UNIQUE ",
            "ADD ",
            "MODIFY ",
            "CONSTRAINT ",
            "PRIMARY ",
            "KEY ",
            "FOREIGN ",
            "REFERENCES ",
            "NOT ",
            "NULL ",
            "DEFAULT ",
            "ON ",
            "AS ",
            "USING ",
            "ENABLE ",
            "(",
            ")",
            ",",
            ".",
            "\"A\"",
            "B ",
            "'x'",
            "1 ",
            "-",
            "GENERATED ",
            "IDENTITY ",
            "CHECK ",
            "WITH ",
            "TIME ",
            "ZONE ",
        ]);
        prop::collection::vec(words, 0..40).prop_map(|w| {
            let mut text = b"CREATE TABLE ".to_vec();
            text.extend(w.concat().into_bytes());
            text
        })
    }

    proptest! {
        #[test]
        fn never_panics_and_errors_point_inside_the_piece(text in ddl(), alter in any::<bool>()) {
            let mut text = text;
            if alter {
                text.splice(0..6, b"ALTER ".iter().copied());
            }
            let len = u64::try_from(text.len()).unwrap();
            let piece = Piece { kind: PieceKind::Sql, span: Span { start: 7, end: 7 + len }, line: 3, text };
            if let Err(e) = parse_ddl(&piece, Encoding::Utf8) {
                prop_assert!(e.offset >= 7 && e.offset <= 7 + len);
                prop_assert!(e.line >= 3);
            }
        }
    }
}

/// The catalog built from Oracle DDL: the neutral catalog's rules, exercised through the syntax that feeds them.
#[cfg(test)]
mod catalog {
    use super::parse_ddl;
    use crate::catalog::{Catalog, CatalogBuilder, Kind, Lookup, Origin, Parent, Table};
    use crate::dialect::oracle::RULES;
    use crate::source::{Encoding, Piece, PieceKind, Span};
    use crate::statement::{Ident, Name};

    fn build(statements: &[&str], default_schema: Option<&str>) -> (Catalog, Vec<String>) {
        let mut builder = CatalogBuilder::new(&RULES, default_schema.map(str::to_owned));
        for (line, text) in (1..).zip(statements) {
            let piece = Piece {
                kind: PieceKind::Sql,
                span: Span { start: 0, end: 0 },
                line,
                text: text.as_bytes().to_vec(),
            };
            let statement = parse_ddl(&piece, Encoding::Utf8).unwrap().unwrap();
            builder.add(
                statement,
                Origin {
                    file: 0,
                    offset: 0,
                    line,
                },
            );
        }
        let (catalog, issues) = builder.finish();
        (
            catalog,
            issues
                .into_iter()
                .map(|i| format!("{}: {}", i.origin.line, i.message))
                .collect(),
        )
    }

    fn name(text: &str) -> Name {
        let ident = |v: &str| Ident {
            value: v.to_owned(),
            quoted: false,
        };
        match text.split_once('.') {
            Some((s, n)) => Name {
                schema: Some(ident(s)),
                name: ident(n),
            },
            None => Name {
                schema: None,
                name: ident(text),
            },
        }
    }

    fn table<'a>(catalog: &'a Catalog, text: &str) -> &'a Table {
        let Lookup::Found(id) = catalog.resolve(&name(text)) else {
            panic!("{text} doesn't resolve");
        };
        catalog.table(id).unwrap()
    }

    /// The table's constraints as `NAME kind columns`, with a foreign key's parent key.
    fn keys(catalog: &Catalog, text: &str) -> Vec<String> {
        let t = table(catalog, text);
        let cols = |c: &[usize]| {
            c.iter()
                .map(|&i| t.columns[i].name.clone())
                .collect::<Vec<_>>()
                .join(",")
        };
        t.constraints
            .iter()
            .map(|c| {
                let n = c.name.clone().unwrap_or_default();
                match &c.kind {
                    Kind::PrimaryKey { columns } => format!("{n} PK {}", cols(columns)),
                    Kind::Unique { columns, index } => format!(
                        "{n} {} {}",
                        if *index { "UIX" } else { "UK" },
                        cols(columns)
                    ),
                    Kind::NotNull { column } => format!("{n} NN {}", t.columns[*column].name),
                    Kind::Check { condition } => format!("{n} CK {condition}"),
                    Kind::ForeignKey {
                        columns,
                        parent: Parent::Missing(p),
                    } => format!("{n} FK {} -> missing {p}", cols(columns)),
                    Kind::ForeignKey {
                        columns,
                        parent:
                            Parent::Key {
                                table: id,
                                constraint,
                            },
                    } => {
                        let parent = catalog.table(*id).unwrap();
                        let key = parent.constraints[*constraint]
                            .name
                            .clone()
                            .unwrap_or_default();
                        format!("{n} FK {} -> {}.{key}", cols(columns), parent.name.name)
                    }
                }
            })
            .collect()
    }

    #[test]
    fn statements_apply_whatever_order_the_files_come_in() {
        let (catalog, issues) = build(
            &[
                "ALTER TABLE APP.C ADD CONSTRAINT C_FK FOREIGN KEY (B, A) REFERENCES APP.P (Y, X)",
                "ALTER TABLE APP.C ADD CONSTRAINT C_P_FK FOREIGN KEY (A) REFERENCES APP.P",
                "ALTER TABLE APP.C ADD (D NUMBER CONSTRAINT D_NN NOT NULL)",
                "CREATE TABLE APP.C (A NUMBER, B NUMBER)",
                "CREATE TABLE APP.P (X NUMBER CONSTRAINT P_PK PRIMARY KEY, Y NUMBER, CONSTRAINT P_UK UNIQUE (X, Y))",
            ],
            None,
        );
        assert_eq!(issues, Vec::<String>::new());
        assert_eq!(
            table(&catalog, "APP.C")
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "D"]
        );
        assert_eq!(
            keys(&catalog, "APP.C"),
            [
                "D_NN NN D",
                "C_FK FK A,B -> P.P_UK",
                "C_P_FK FK A -> P.P_PK"
            ]
        );
    }

    #[test]
    fn names_resolve_by_schema_default_schema_or_the_only_match() {
        let ddl = [
            "CREATE TABLE A.T (X NUMBER)",
            "CREATE TABLE B.T (X NUMBER)",
            "CREATE TABLE B.U (X NUMBER)",
            "CREATE TABLE V (X NUMBER)",
        ];
        let (catalog, _) = build(&ddl, None);
        assert!(matches!(catalog.resolve(&name("T")), Lookup::Ambiguous(ids) if ids.len() == 2));
        assert!(matches!(catalog.resolve(&name("U")), Lookup::Found(_)));
        assert!(matches!(catalog.resolve(&name("V")), Lookup::Found(_)));
        assert_eq!(catalog.resolve(&name("C.T")), Lookup::Missing);
        assert_eq!(
            catalog.resolve(&name("APP.V")),
            Lookup::Missing,
            "a table created without a schema has none"
        );
        let (catalog, _) = build(&ddl, Some("A"));
        assert_eq!(table(&catalog, "T").name.schema.as_deref(), Some("A"));
        assert_eq!(table(&catalog, "A.V").name.schema.as_deref(), Some("A"));
        assert_eq!(
            catalog.resolve(&name("U")),
            Lookup::Missing,
            "the default schema decides an unqualified name"
        );
        let quoted = Name {
            schema: Some(Ident {
                value: "B".into(),
                quoted: true,
            }),
            name: Ident {
                value: "U".into(),
                quoted: true,
            },
        };
        assert!(matches!(catalog.resolve(&quoted), Lookup::Found(_)));
    }

    #[test]
    fn what_cannot_be_applied_is_an_issue() {
        let (catalog, issues) = build(
            &[
                "CREATE TABLE T (A NUMBER, B NUMBER, A DATE)",
                "CREATE TABLE T (Z NUMBER)",
                "ALTER TABLE NOPE ADD CONSTRAINT X PRIMARY KEY (A)",
                "ALTER TABLE T ADD CONSTRAINT T_PK PRIMARY KEY (C)",
                "ALTER TABLE T ENABLE CONSTRAINT GHOST",
                "ALTER TABLE T DROP CONSTRAINT T_PK",
                "CREATE UNIQUE INDEX T_FX ON T (UPPER(A))",
                "CREATE TABLE NOKEY (K NUMBER)",
                "ALTER TABLE T ADD CONSTRAINT T_FK FOREIGN KEY (A) REFERENCES NOKEY",
                "ALTER TABLE T ADD CONSTRAINT T_FK2 FOREIGN KEY (A, B) REFERENCES NOKEY (K)",
                "ALTER TABLE T ADD CONSTRAINT T_EXT FOREIGN KEY (B) REFERENCES OTHER.PARENT",
            ],
            None,
        );
        assert_eq!(
            issues,
            [
                "1: column A of T is defined twice; the first is kept",
                "2: table T is created again; the first definition is kept",
                "3: ALTER TABLE names table NOPE, which no CREATE TABLE in the input defines",
                "4: constraint T_PK names column C of T, which the table doesn't have",
                "5: constraint GHOST of T is changed, but no statement adds it",
                "6: ALTER TABLE T DROP CONSTRAINT T_PK: not followed",
                "7: unique index T_FX has an expression, so it is not checked",
                "9: constraint T_FK references NOKEY, which has no primary key",
                "10: constraint T_FK2 references NOKEY, which has no primary key or unique constraint on those columns",
            ]
        );
        assert_eq!(keys(&catalog, "T"), ["T_EXT FK B -> missing OTHER.PARENT"]);
    }

    #[test]
    fn unique_indexes_are_keys_unless_an_enabled_key_covers_them() {
        let (catalog, issues) = build(
            &[
                "CREATE TABLE T (A NUMBER, B NUMBER, C NUMBER)",
                "CREATE UNIQUE INDEX T_PK ON T (A)",
                "CREATE UNIQUE INDEX T_BC ON T (C, B)",
                "CREATE UNIQUE INDEX T_C ON T (C)",
                "ALTER TABLE T ADD CONSTRAINT T_PK PRIMARY KEY (A) USING INDEX T_PK ENABLE",
                "ALTER TABLE T ADD CONSTRAINT T_UK UNIQUE (B, C) DISABLE",
                "CREATE INDEX T_B ON T (B)",
                "CREATE TABLE U (C NUMBER REFERENCES T (C))",
            ],
            None,
        );
        assert_eq!(
            keys(&catalog, "T"),
            ["T_PK PK A", "T_UK UK B,C", "T_BC UIX C,B", "T_C UIX C"]
        );
        assert_eq!(
            issues,
            [
                "8: an unnamed constraint references T, which has no primary key or unique constraint on those columns"
            ]
        );
    }

    #[test]
    fn state_changes_apply_to_the_constraint_they_name() {
        let (catalog, issues) = build(
            &[
                "ALTER TABLE T MODIFY CONSTRAINT T_FK INITIALLY IMMEDIATE",
                "ALTER TABLE T DISABLE CONSTRAINT T_FK",
                "ALTER TABLE T ADD CONSTRAINT T_FK FOREIGN KEY (A) REFERENCES T DEFERRABLE INITIALLY DEFERRED",
                "CREATE TABLE T (A NUMBER PRIMARY KEY)",
                "ALTER TABLE T ENABLE NOVALIDATE CONSTRAINT T_FK",
            ],
            None,
        );
        assert_eq!(issues.len(), 2, "{issues:?}");
        let fk = table(&catalog, "T")
            .constraints
            .iter()
            .find(|c| c.name.as_deref() == Some("T_FK"))
            .unwrap();
        assert_eq!(
            (
                fk.state.enabled,
                fk.state.validated,
                fk.state.deferrable,
                fk.state.initially_deferred
            ),
            (true, false, true, true)
        );
        assert_eq!(fk.declared.line, 3);
    }
}

/// Milestone 3's check: every DDL statement of the committed exports parses, and the catalog built from them is the
/// generator's schema.
#[cfg(test)]
#[cfg(feature = "gen")]
mod fixtures {
    use std::path::{Path, PathBuf};

    use super::parse_ddl;
    use crate::catalog::{Catalog, CatalogBuilder, Kind, Lookup, Origin, Parent, Table};
    use crate::dialect::oracle::cursor::first_word;

    use crate::dialect::Runner;
    use crate::dialect::oracle::RULES;
    use crate::dialect::oracle::split::Splitter;
    use crate::source::{Encoding, PieceKind};
    use crate::statement::{AlterAction, ColumnDefault, Expr, Ident, Name, Statement, TypeFamily};
    use crate::synth::Manifest;
    use crate::synth::schema::{
        self, APP_TABLES, DefaultValue, EMPLOYEES_EMAIL_UIX, NotNull, REF_TABLES, State, TableId,
        Ty,
    };

    fn exports() -> Vec<(PathBuf, Encoding)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut found: Vec<(PathBuf, Encoding)> = std::fs::read_dir(root.join("gen/oracle"))
            .unwrap()
            .map(|e| {
                let dir = e.unwrap().path();
                let manifest: Manifest = serde_json::from_str(
                    &std::fs::read_to_string(dir.join("expected.json")).unwrap(),
                )
                .unwrap();
                (dir, manifest.options.encoding)
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found.push((root.join("sqlcl"), Encoding::Utf8));
        found
    }

    /// The export's DDL statements with their origins, in `order.txt` order.
    fn ddl(dir: &Path, encoding: Encoding) -> Vec<(Statement, Origin)> {
        let order = std::fs::read_to_string(dir.join("order.txt")).unwrap();
        let mut statements = Vec::new();
        for (file, path) in order.lines().enumerate() {
            let bytes = std::fs::read(dir.join(path)).unwrap();
            for piece in Splitter::new(bytes.as_slice(), Runner::SqlPlus) {
                let piece = piece.unwrap();
                let at = format!("{}:{}", path, piece.line);
                assert_ne!(piece.kind, PieceKind::Unterminated, "{at}");
                let parsed = parse_ddl(&piece, encoding)
                    .unwrap_or_else(|e| panic!("{path}:{}: {}", e.line, e.message));
                let head = first_word(&piece.text);
                let Some(statement) = parsed else {
                    assert!(
                        piece.kind != PieceKind::Sql
                            || !matches!(head.as_deref(), Some("CREATE" | "ALTER"))
                            || is_other_ddl(&piece.text),
                        "{at}: table DDL not parsed"
                    );
                    continue;
                };
                match &statement {
                    Statement::Skipped(skip) => panic!("{at}: skipped {skip:?}"),
                    Statement::AlterTable { actions, .. } => {
                        assert!(
                            !actions.iter().any(|a| matches!(a, AlterAction::Other(_))),
                            "{at}: {actions:?}"
                        );
                    }
                    _ => {}
                }
                let origin = Origin {
                    file,
                    offset: piece.span.start,
                    line: piece.line,
                };
                statements.push((statement, origin));
            }
        }
        statements
    }

    /// `CREATE SEQUENCE`, `ALTER TRIGGER`: DDL of other objects.
    fn is_other_ddl(text: &[u8]) -> bool {
        let text = String::from_utf8_lossy(text).to_ascii_uppercase();
        ["SEQUENCE", "TRIGGER"]
            .iter()
            .any(|w| text.split_whitespace().nth(1) == Some(w))
    }

    fn catalog(statements: Vec<(Statement, Origin)>) -> Catalog {
        let mut builder = CatalogBuilder::new(&RULES, None);
        for (statement, origin) in statements {
            builder.add(statement, origin);
        }
        let (catalog, issues) = builder.finish();
        assert!(issues.is_empty(), "{issues:?}");
        catalog
    }

    fn table(catalog: &Catalog, id: TableId) -> &Table {
        let ident = |v: &str| Ident {
            value: v.to_owned(),
            quoted: true,
        };
        let name = Name {
            schema: Some(ident(id.schema())),
            name: ident(id.name()),
        };
        let Lookup::Found(found) = catalog.resolve(&name) else {
            panic!("{} missing", id.name());
        };
        catalog.table(found).unwrap()
    }

    fn names(table: &Table, columns: &[usize]) -> Vec<String> {
        columns
            .iter()
            .map(|&i| table.columns[i].name.clone())
            .collect()
    }

    /// Compares the catalog with the generator's schema, table by table.
    fn assert_matches_generator(catalog: &Catalog, export: &Path) {
        let at = export.display();
        assert_eq!(
            catalog.tables().len(),
            REF_TABLES.len() + APP_TABLES.len(),
            "{at}"
        );
        for id in REF_TABLES.into_iter().chain(APP_TABLES) {
            let t = table(catalog, id);
            let expected: Vec<(&str, TypeFamily)> = id
                .columns()
                .iter()
                .map(|c| {
                    let family = match c.ty {
                        Ty::Decimal(..) => TypeFamily::Number,
                        Ty::Varchar(_) => TypeFamily::Text,
                        Ty::Char(_) => TypeFamily::FixedText,
                        Ty::Date => TypeFamily::Date,
                        Ty::Timestamp => TypeFamily::Timestamp,
                    };
                    (c.name, family)
                })
                .collect();
            let actual: Vec<(&str, TypeFamily)> = t
                .columns
                .iter()
                .map(|c| (c.name.as_str(), c.data_type.family))
                .collect();
            assert_eq!(actual, expected, "{at}: {}", id.name());
            for (column, generated) in t.columns.iter().zip(id.columns()) {
                let default = generated.default.map(|d| match d {
                    DefaultValue::Text(text) => ColumnDefault::Value(Expr::String {
                        bytes: text.as_bytes().to_vec(),
                        national: false,
                    }),
                    DefaultValue::CurrentTimestamp => {
                        ColumnDefault::Opaque("SYSTIMESTAMP".to_owned())
                    }
                });
                assert_eq!(column.default, default, "{at}: {}", column.name);
                assert_eq!(
                    column.default_on_null, generated.on_null,
                    "{at}: {}",
                    column.name
                );
            }

            let mut not_null: Vec<String> = t
                .constraints
                .iter()
                .filter_map(|c| match c.kind {
                    Kind::NotNull { column } if c.state.enabled => {
                        Some(t.columns[column].name.clone())
                    }
                    _ => None,
                })
                .collect();
            not_null.sort();
            let mut expected: Vec<String> = id
                .columns()
                .iter()
                .filter(|c| c.not_null != NotNull::No)
                .map(|c| c.name.to_owned())
                .collect();
            // SQLcl exports DEFAULT ON NULL columns as NOT NULL, which is what Oracle makes them.
            if export.ends_with("sqlcl") {
                expected.extend(
                    id.columns()
                        .iter()
                        .filter(|c| c.on_null)
                        .map(|c| c.name.to_owned()),
                );
            }
            expected.sort();
            assert_eq!(not_null, expected, "{at}: {} NOT NULL", id.name());

            let mut keys: Vec<String> = t
                .constraints
                .iter()
                .filter(|c| !matches!(c.kind, Kind::NotNull { .. }))
                .map(|c| c.name.clone().unwrap_or_default())
                .collect();
            keys.sort();
            let mut expected: Vec<String> = schema::constraints_of(id)
                .map(|c| c.name.to_owned())
                .collect();
            if id == EMPLOYEES_EMAIL_UIX.table {
                expected.push(EMPLOYEES_EMAIL_UIX.name.to_owned());
            }
            expected.sort();
            assert_eq!(keys, expected, "{at}: {} keys", id.name());

            assert_constraints(catalog, t, id, &at.to_string());
        }
        assert_email_index(catalog, &at.to_string());
    }

    fn assert_email_index(catalog: &Catalog, at: &str) {
        let t = table(catalog, EMPLOYEES_EMAIL_UIX.table);
        let index = t
            .constraints
            .iter()
            .find(|c| c.name.as_deref() == Some(EMPLOYEES_EMAIL_UIX.name))
            .unwrap();
        let Kind::Unique {
            columns,
            index: true,
        } = &index.kind
        else {
            panic!("{at}: {index:?}");
        };
        assert_eq!(names(t, columns), EMPLOYEES_EMAIL_UIX.columns, "{at}");
    }
    fn assert_constraints(catalog: &Catalog, t: &Table, id: TableId, at: &str) {
        for generated in schema::constraints_of(id) {
            let c = t
                .constraints
                .iter()
                .find(|c| c.name.as_deref() == Some(generated.name))
                .unwrap();
            let state = match generated.state {
                State::Enforced => (true, true),
                State::NotValidated => (true, false),
                State::Disabled => (false, false),
            };
            assert_eq!(
                (c.state.enabled, c.state.validated),
                state,
                "{at}: {}",
                generated.name
            );
            let columns: Vec<String> = generated.columns.iter().map(|&c| c.to_owned()).collect();
            match (&c.kind, generated.kind) {
                (Kind::PrimaryKey { columns: actual }, schema::Kind::Primary)
                | (
                    Kind::Unique {
                        columns: actual,
                        index: false,
                    },
                    schema::Kind::Unique,
                ) => {
                    assert_eq!(names(t, actual), columns, "{at}: {}", generated.name);
                }
                (
                    Kind::ForeignKey {
                        columns: actual,
                        parent:
                            Parent::Key {
                                table: parent,
                                constraint,
                            },
                    },
                    schema::Kind::Foreign {
                        parent: parent_id,
                        parent_columns,
                        deferred,
                    },
                ) => {
                    let parent = catalog.table(*parent).unwrap();
                    assert_eq!(
                        parent.name.name,
                        parent_id.name(),
                        "{at}: {}",
                        generated.name
                    );
                    let key = &parent.constraints[*constraint];
                    let key_columns = match &key.kind {
                        Kind::PrimaryKey { columns } | Kind::Unique { columns, .. } => {
                            names(parent, columns)
                        }
                        other => panic!("{at}: {} references {other:?}", generated.name),
                    };
                    let wanted = parent_columns.map_or_else(
                        || {
                            schema::constraints_of(parent_id)
                                .find(|k| k.kind == schema::Kind::Primary)
                                .unwrap()
                                .columns
                        },
                        |p| p,
                    );
                    assert_eq!(key_columns, wanted, "{at}: {}", generated.name);
                    assert_eq!(names(t, actual), columns, "{at}: {}", generated.name);
                    assert_eq!(
                        (c.state.deferrable, c.state.initially_deferred),
                        (deferred, deferred),
                        "{at}: {}",
                        generated.name
                    );
                }
                (actual, expected) => {
                    panic!("{at}: {}: {actual:?} is not {expected:?}", generated.name)
                }
            }
        }
    }

    #[test]
    fn every_export_parses_into_the_generators_schema() {
        for (dir, encoding) in exports() {
            assert_matches_generator(&catalog(ddl(&dir, encoding)), &dir);
        }
    }

    #[test]
    fn the_catalog_does_not_depend_on_file_order() {
        for (dir, encoding) in exports() {
            let mut statements = ddl(&dir, encoding);
            statements.reverse();
            assert_matches_generator(&catalog(statements), &dir);
        }
    }
}
