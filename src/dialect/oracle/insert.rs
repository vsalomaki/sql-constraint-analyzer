//! `INSERT` statements as SQL Developer, SQLcl and the generator write them: one row (`INSERT INTO … VALUES (…)`)
//! or several (`INSERT ALL INTO … VALUES (…) … SELECT … FROM DUAL`). An insert whose rows come from a query is
//! skipped as not followed. Values are kept as written; [`super::eval`] says what they mean.

use std::borrow::Cow;

use super::RULES;
use super::cursor::{Cursor, first_word_is};
use super::lex::{Token, TokenKind, is_word_byte, same_ignoring_case};
use crate::source::{Encoding, Piece};
use crate::statement::{Expr, Ident, InsertRow, Name, ParseError, Skip, Statement, Value};

/// Calls and negations nested deeper than this are kept as written, so no input can exhaust the stack.
const MAX_DEPTH: usize = 64;

/// Words that can't appear outside parentheses in a value, so a value list missing its `)` stops at them.
const STOP_WORDS: [&str; 4] = ["INSERT", "INTO", "VALUES", "SELECT"];

/// The insert `piece` holds, or `None` when it isn't an `INSERT`.
pub fn parse_insert(
    piece: &Piece,
    encoding: Encoding,
) -> Result<Option<Statement<'_>>, ParseError> {
    parse_insert_with(piece, encoding, None)
}

/// The start of the last single-row insert read from a file, up to the end of its table name or column list, and
/// what it names. The rows of one table repeat it byte for byte, so a row that starts with it, then blanks and
/// `VALUES`, has only its values read: with `VALUES` next, nothing else (an alias, a column list) follows the
/// prefix. Only a statement that read without error is kept.
#[derive(Debug, Default)]
pub struct Memo {
    target: Option<Target>,
}

#[derive(Debug)]
struct Target {
    prefix: Vec<u8>,
    table: Name<'static>,
    columns: Option<Vec<Ident<'static>>>,
}

impl Memo {
    /// Where the rest of `text` starts, when it starts with the kept prefix, then blanks and `VALUES`.
    fn hit(&self, text: &[u8]) -> Option<usize> {
        let prefix = &self.target.as_ref()?.prefix;
        let rest = text.strip_prefix(prefix.as_slice())?;
        let next = rest.trim_ascii_start();
        let values = next
            .get(..6)
            .is_some_and(|word| same_ignoring_case(word, b"VALUES"))
            && !next.get(6).is_some_and(|&b| is_word_byte(b));
        (next.len() < rest.len() && values).then_some(prefix.len())
    }
}

/// `parse_insert`, keeping in `memo`, when there is one, what a file's next row may start with.
pub fn parse_insert_with<'a>(
    piece: &'a Piece,
    encoding: Encoding,
    memo: Option<&'a mut Memo>,
) -> Result<Option<Statement<'a>>, ParseError> {
    if !first_word_is(&piece.text, "INSERT") {
        return Ok(None);
    }
    let Some(memo) = memo else {
        return Parser::new(piece, encoding)?.insert().map(Some);
    };
    if let Some(rest) = memo.hit(&piece.text) {
        let memo: &'a Memo = memo;
        return match &memo.target {
            Some(target) => {
                let mut parser = Parser {
                    piece,
                    c: Cursor::from_offset(piece, encoding, rest)?,
                    target_end: None,
                };
                let columns = target
                    .columns
                    .as_ref()
                    .map(|columns| columns.iter().map(borrowed_ident).collect());
                parser.row(borrowed_name(&target.table), columns).map(Some)
            }
            None => Parser::new(piece, encoding)?.insert().map(Some),
        };
    }
    let mut parser = Parser::new(piece, encoding)?;
    let statement = parser.insert()?;
    if let (Some(end), Statement::Insert(rows)) = (parser.target_end, &statement)
        && let [row] = rows.as_slice()
        && let Some(prefix) = piece.text.get(..end)
        && piece.text.get(end).is_some_and(u8::is_ascii_whitespace)
    {
        memo.target = Some(Target {
            prefix: prefix.to_vec(),
            table: row.table.clone().into_owned(),
            columns: row
                .columns
                .as_ref()
                .map(|columns| columns.iter().cloned().map(Ident::into_owned).collect()),
        });
    }
    Ok(Some(statement))
}

fn borrowed_ident<'a>(ident: &'a Ident<'static>) -> Ident<'a> {
    Ident {
        value: Cow::Borrowed(&ident.value),
        quoted: ident.quoted,
    }
}

fn borrowed_name<'a>(name: &'a Name<'static>) -> Name<'a> {
    Name {
        schema: name.schema.as_ref().map(borrowed_ident),
        name: borrowed_ident(&name.name),
    }
}

fn not_followed(what: &str) -> Statement<'static> {
    Statement::Skipped(Skip::NotFollowed(what.to_owned()))
}

struct Parser<'a> {
    piece: &'a Piece,
    c: Cursor<'a>,
    /// Where a single-row insert's table name or column list ends in the piece.
    target_end: Option<usize>,
}

impl<'a> Parser<'a> {
    fn new(piece: &'a Piece, encoding: Encoding) -> Result<Self, ParseError> {
        Ok(Self {
            piece,
            c: Cursor::new(piece, encoding)?,
            target_end: None,
        })
    }

    fn insert(&mut self) -> Result<Statement<'a>, ParseError> {
        self.c.expect_word("INSERT")?;
        if self.c.eat_word("INTO") {
            self.single()
        } else if self.c.eat_word("ALL") {
            self.multi()
        } else if self.c.at_word("FIRST") {
            Ok(not_followed("INSERT FIRST"))
        } else {
            Err(self.c.expected("INTO, ALL or FIRST after INSERT"))
        }
    }

    fn single(&mut self) -> Result<Statement<'a>, ParseError> {
        if self.c.at_punct(b'(') {
            return Ok(not_followed("INSERT INTO a subquery"));
        }
        let (table, columns) = self.target()?;
        self.target_end = self
            .c
            .pos()
            .checked_sub(1)
            .and_then(|last| self.c.token(last))
            .map(|token| token.end);
        self.row(table, columns)
    }

    /// The rest of a single-row insert after its table name or column list.
    fn row(
        &mut self,
        table: Name<'a>,
        columns: Option<Vec<Ident<'a>>>,
    ) -> Result<Statement<'a>, ParseError> {
        if !self.c.eat_word("VALUES") {
            return if self.at_query(0) {
                Ok(not_followed("INSERT … SELECT"))
            } else {
                Err(self.c.expected("VALUES or a query after the table"))
            };
        }
        let (values, _) = self.values(columns.as_ref().map_or(0, Vec::len))?;
        if !self.c.at_end() {
            return Err(self
                .c
                .expected("the end of the statement after the value list"));
        }
        Ok(Statement::Insert(vec![InsertRow {
            table,
            columns,
            values,
            span: self.piece.span,
            line: self.piece.line,
        }]))
    }

    fn multi(&mut self) -> Result<Statement<'a>, ParseError> {
        let mut rows = Vec::new();
        while let Some(into) = self.c.peek().filter(|&t| self.c.is_word(t, "INTO")) {
            self.c.bump();
            let (table, columns) = self.target()?;
            if !self.c.eat_word("VALUES") {
                // Without VALUES the row is the query's select list.
                return Ok(not_followed("INSERT ALL … SELECT"));
            }
            let (values, end) = self.values(columns.as_ref().map_or(0, Vec::len))?;
            rows.push(InsertRow {
                table,
                columns,
                values,
                span: self.c.span(into.start, end),
                line: self.c.line_at(into.start),
            });
        }
        if rows.is_empty() {
            return if self.c.at_word("WHEN") {
                Ok(not_followed("INSERT ALL WHEN"))
            } else {
                Err(self.c.expected("INTO after INSERT ALL"))
            };
        }
        if !self.c.eat_word("SELECT") {
            return if self.at_query(0) {
                Ok(not_followed("INSERT ALL … SELECT"))
            } else {
                Err(self.c.expected("INTO or a query after the INTO clauses"))
            };
        }
        Ok(if self.selects_from_dual() {
            Statement::Insert(rows)
        } else {
            not_followed("INSERT ALL … SELECT")
        })
    }

    /// After `SELECT`: whether the query is `<one item> FROM DUAL` and nothing more, so the clauses run exactly once.
    /// Any other query, a set operator among them, may give another number of rows.
    fn selects_from_dual(&self) -> bool {
        matches!(
            self.c.tokens().get(self.c.pos()..),
            Some(&[_, from, dual]) if self.c.is_word(from, "FROM") && self.c.is_word(dual, "DUAL")
        )
    }

    /// `name [alias] [(columns)]`.
    fn target(&mut self) -> Result<(Name<'a>, Option<Vec<Ident<'a>>>), ParseError> {
        let table = self.c.name("a table name")?;
        // DBMS_METADATA aliases its target: `INTO "APP"."EMPLOYEES" KUT$ (…)`.
        if let Some(token) = self.c.peek() {
            let alias = match token.kind {
                TokenKind::QuotedIdent => true,
                TokenKind::Word => !["VALUES", "SELECT", "WITH", "INTO"]
                    .iter()
                    .any(|w| self.c.is_word(token, w)),
                _ => false,
            };
            if alias {
                self.c.bump();
            }
        }
        let columns = if self.c.at_punct(b'(') && !self.at_query(1) {
            Some(self.columns()?)
        } else {
            None
        };
        Ok((table, columns))
    }

    fn columns(&mut self) -> Result<Vec<Ident<'a>>, ParseError> {
        self.c.bump();
        let mut columns = Vec::new();
        loop {
            let at = self.c.peek().map_or(0, |t| t.start);
            let column = self.c.ident("a column name")?;
            // Oracle refuses the statement (ORA-00957); `A` and `"A"` are one column.
            if columns.iter().any(|c: &Ident| c.value == column.value) {
                let message = format!("column {} is listed twice", column.value);
                return Err(self.c.error_at(at, message));
            }
            columns.push(column);
            if self.c.eat_punct(b')') {
                return Ok(columns);
            }
            if !self.c.eat_punct(b',') {
                return Err(self.c.expected("`)` or `,` in the column list"));
            }
        }
    }

    /// `(value, …)` of a row with `columns` listed, and the offset after its `)`.
    fn values(&mut self, columns: usize) -> Result<(Vec<Value<'a>>, usize), ParseError> {
        if !self.c.eat_punct(b'(') {
            return Err(self.c.expected("`(` after VALUES"));
        }
        let mut values = Vec::with_capacity(columns);
        loop {
            let run = self.run();
            let tokens = self.c.tokens().get(run).unwrap_or_default();
            let (Some(first), Some(last)) = (tokens.first(), tokens.last()) else {
                return Err(self.c.expected("a value"));
            };
            values.push(Value {
                expr: expr(&self.c, tokens, 0),
                span: self.c.span(first.start, last.end),
            });
            match self.c.peek() {
                Some(t) if self.c.is_punct(t, b',') => {
                    self.c.bump();
                }
                Some(t) if self.c.is_punct(t, b')') => {
                    self.c.bump();
                    return Ok((values, t.end));
                }
                _ => return Err(self.c.expected("`)` or `,` in the value list")),
            }
        }
    }

    /// The tokens of one value: up to a `,` or `)` outside parentheses.
    fn run(&mut self) -> std::ops::Range<usize> {
        let start = self.c.pos();
        let mut depth = 0_usize;
        while let Some(token) = self.c.peek() {
            if self.c.is_punct(token, b'(') {
                depth += 1;
            } else if depth == 0
                && (self.c.is_punct(token, b',')
                    || self.c.is_punct(token, b')')
                    || self.c.is_punct(token, b';')
                    || STOP_WORDS.iter().any(|w| self.c.is_word(token, w)))
            {
                break;
            } else if self.c.is_punct(token, b')') {
                depth -= 1;
            }
            self.c.bump();
        }
        start..self.c.pos()
    }

    /// Whether a query starts `ahead` tokens from the cursor.
    fn at_query(&self, ahead: usize) -> bool {
        self.c.word_at(ahead, "SELECT")
            || self.c.word_at(ahead, "WITH")
            || self.c.punct_at(ahead, b'(')
    }
}

/// A value by its shape; anything without one of the shapes below is kept as written.
pub(super) fn expr<'a>(c: &Cursor<'a>, tokens: &[Token], depth: usize) -> Expr<'a> {
    shaped(c, tokens, depth).unwrap_or_else(|| Expr::Other(c.text_of(tokens)))
}

fn shaped<'a>(c: &Cursor<'a>, tokens: &[Token], depth: usize) -> Option<Expr<'a>> {
    if depth > MAX_DEPTH {
        return None;
    }
    match *tokens {
        [token] => literal(c, token),
        [minus, ref rest @ ..] if c.is_punct(minus, b'-') => {
            Some(Expr::Negate(Box::new(shaped(c, rest, depth + 1)?)))
        }
        [keyword, string]
            if (c.is_word(keyword, "DATE") || c.is_word(keyword, "TIMESTAMP"))
                && string.kind == (TokenKind::String { national: false }) =>
        {
            Some(Expr::Typed {
                type_name: RULES.folded(c.token_text(keyword)),
                text: c.decoded(c.content(string)),
            })
        }
        [.., last] if c.is_punct(last, b')') => call(c, tokens, depth),
        _ => next_value(c, tokens),
    }
}

fn literal<'a>(c: &Cursor<'a>, token: Token) -> Option<Expr<'a>> {
    match token.kind {
        TokenKind::Number => Some(Expr::Number(c.token_text(token))),
        TokenKind::String { national } | TokenKind::AltString { national } => Some(Expr::String {
            bytes: c.content(token),
            national,
        }),
        _ if c.is_word(token, "NULL") => Some(Expr::Null),
        _ if c.is_word(token, "DEFAULT") => Some(Expr::Default),
        _ => None,
    }
}

/// `name(args)`, where the `(` after the name closes at the last token.
fn call<'a>(c: &Cursor<'a>, tokens: &[Token], depth: usize) -> Option<Expr<'a>> {
    let open = tokens.iter().position(|&t| c.is_punct(t, b'('))?;
    let (head, rest) = tokens.split_at_checked(open)?;
    let function = match head {
        [name] => c.ident_of(*name)?.value,
        _ => Cow::Owned(
            dotted(c, head)?
                .iter()
                .map(|part| &*part.value)
                .collect::<Vec<_>>()
                .join("."),
        ),
    };
    let inner = rest.get(1..rest.len().checked_sub(1)?)?;
    let mut args = Vec::new();
    if !inner.is_empty() {
        let (mut start, mut nesting) = (0, 0_usize);
        for (i, &token) in inner.iter().enumerate() {
            if c.is_punct(token, b'(') {
                nesting += 1;
            } else if c.is_punct(token, b')') {
                // A `)` that closes the call early.
                nesting = nesting.checked_sub(1)?;
            } else if nesting == 0 && c.is_punct(token, b',') {
                args.push(argument(c, inner.get(start..i)?, depth)?);
                start = i + 1;
            }
        }
        if nesting > 0 {
            return None;
        }
        args.push(argument(c, inner.get(start..)?, depth)?);
    }
    Some(Expr::Call { function, args })
}

/// One argument of a call; `None` when it is empty.
fn argument<'a>(c: &Cursor<'a>, tokens: &[Token], depth: usize) -> Option<Expr<'a>> {
    (!tokens.is_empty()).then(|| expr(c, tokens, depth + 1))
}

/// `[schema.]sequence.NEXTVAL`.
fn next_value<'a>(c: &Cursor<'a>, tokens: &[Token]) -> Option<Expr<'a>> {
    let mut parts = dotted(c, tokens)?;
    let last = parts.pop()?;
    if last.quoted || last.value != "NEXTVAL" {
        return None;
    }
    let name = parts.pop()?;
    let schema = parts.pop();
    parts
        .is_empty()
        .then_some(Expr::NextValue(Name { schema, name }))
}

/// `a.b.c`: identifiers separated by dots.
fn dotted<'a>(c: &Cursor<'a>, tokens: &[Token]) -> Option<Vec<Ident<'a>>> {
    let mut parts = Vec::new();
    let mut iter = tokens.iter();
    loop {
        parts.push(c.ident_of(*iter.next()?)?);
        match iter.next() {
            None => return Some(parts),
            Some(&dot) if c.is_punct(dot, b'.') => {}
            Some(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::to_u64;
    use crate::source::{PieceKind, Span};

    /// A piece that starts at offset 100 on line 10, so offsets and lines are visibly absolute.
    fn piece(text: &str) -> Piece {
        Piece {
            kind: PieceKind::Sql,
            span: Span {
                start: 100,
                end: 100 + to_u64(text.len()) + 1,
            },
            line: 10,
            text: text.as_bytes().to_vec(),
        }
    }

    fn parse(text: &str) -> Result<Option<Statement<'static>>, ParseError> {
        parse_insert(&piece(text), Encoding::Utf8).map(|s| s.map(Statement::into_owned))
    }

    fn rows(text: &str) -> Vec<InsertRow<'static>> {
        match parse(text) {
            Ok(Some(Statement::Insert(rows))) => rows,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn row(text: &str) -> InsertRow<'static> {
        let mut rows = rows(text);
        assert_eq!(rows.len(), 1);
        rows.remove(0)
    }

    fn exprs(text: &str) -> Vec<Expr<'static>> {
        row(text).values.into_iter().map(|v| v.expr).collect()
    }

    fn value_of(sql: &str) -> Expr<'static> {
        let mut exprs = exprs(&format!("insert into t values ({sql})"));
        assert_eq!(exprs.len(), 1, "{sql}");
        exprs.remove(0)
    }

    fn skipped(text: &str) -> String {
        match parse(text) {
            Ok(Some(Statement::Skipped(Skip::NotFollowed(what)))) => what,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn error(text: &str) -> ParseError {
        parse(text).unwrap_err()
    }

    fn ident(value: &str, quoted: bool) -> Ident<'static> {
        Ident {
            value: value.to_owned().into(),
            quoted,
        }
    }

    fn string(s: &str) -> Expr<'static> {
        Expr::String {
            bytes: s.as_bytes().to_vec().into(),
            national: false,
        }
    }

    fn number(s: &str) -> Expr<'static> {
        Expr::Number(s.to_owned().into())
    }

    fn other(s: &str) -> Expr<'static> {
        Expr::Other(s.to_owned().into())
    }

    #[test]
    fn an_sql_developer_insert_is_one_row() {
        let text = "Insert into APP.\"AuditLog\" (LOG_ID,DETAIL,AT) values ('1',null,to_date('17-JUN-03','DD-MON-RR'))";
        let row = row(text);
        assert_eq!(
            row.table,
            Name {
                schema: Some(ident("APP", false)),
                name: ident("AuditLog", true)
            }
        );
        assert_eq!(
            row.columns,
            Some(vec![
                ident("LOG_ID", false),
                ident("DETAIL", false),
                ident("AT", false)
            ])
        );
        assert_eq!(row.span, piece(text).span);
        assert_eq!(row.line, 10);
        let exprs: Vec<&Expr> = row.values.iter().map(|v| &v.expr).collect();
        assert_eq!(
            exprs,
            [
                &string("1"),
                &Expr::Null,
                &Expr::Call {
                    function: "TO_DATE".into(),
                    args: vec![string("17-JUN-03"), string("DD-MON-RR")]
                }
            ]
        );
        let spans: Vec<&str> = row
            .values
            .iter()
            .map(|v| {
                let start = usize::try_from(v.span.start - 100).unwrap();
                let end = usize::try_from(v.span.end - 100).unwrap();
                text.get(start..end).unwrap()
            })
            .collect();
        assert_eq!(spans, ["'1'", "null", "to_date('17-JUN-03','DD-MON-RR')"]);
    }

    #[test]
    fn names_fold_unless_quoted_and_the_schema_is_optional() {
        let row = row("INSERT INTO app.emp (Emp_Id, \"Mixed\") VALUES (1, 2)");
        assert_eq!(
            row.table,
            Name {
                schema: Some(ident("APP", false)),
                name: ident("EMP", false)
            }
        );
        assert_eq!(
            row.columns,
            Some(vec![ident("EMP_ID", false), ident("Mixed", true)])
        );
        let row = self::row("insert into \"t\" values (1)");
        assert_eq!(
            row.table,
            Name {
                schema: None,
                name: ident("t", true)
            }
        );
        assert_eq!(row.columns, None);
    }

    #[test]
    fn values_keep_their_oracle_shapes() {
        assert_eq!(value_of("NULL"), Expr::Null);
        assert_eq!(value_of("default"), Expr::Default);
        assert_eq!(value_of("1.0E2"), number("1.0E2"));
        assert_eq!(value_of("-1E0"), Expr::Negate(Box::new(number("1E0"))));
        assert_eq!(value_of("- '5'"), Expr::Negate(Box::new(string("5"))));
        assert_eq!(value_of("''"), string(""));
        assert_eq!(value_of("'it''s'"), string("it's"));
        assert_eq!(value_of("q'[a'b]'"), string("a'b"));
        assert_eq!(
            value_of("N'x'"),
            Expr::String {
                bytes: b"x".to_vec().into(),
                national: true
            }
        );
        assert_eq!(
            value_of("DATE '2003-06-17'"),
            Expr::Typed {
                type_name: "DATE".into(),
                text: "2003-06-17".into()
            }
        );
        assert_eq!(
            value_of("timestamp '2003-06-17 10:00:00.5'"),
            Expr::Typed {
                type_name: "TIMESTAMP".into(),
                text: "2003-06-17 10:00:00.5".into()
            }
        );
        assert_eq!(
            value_of("EMPTY_CLOB()"),
            Expr::Call {
                function: "EMPTY_CLOB".into(),
                args: vec![]
            }
        );
        assert_eq!(
            value_of("UNISTR('\\00e4')"),
            Expr::Call {
                function: "UNISTR".into(),
                args: vec![string("\\00e4")]
            }
        );
        assert_eq!(
            value_of("to_timestamp(to_char(sysdate, 'x'), 'y')"),
            Expr::Call {
                function: "TO_TIMESTAMP".into(),
                args: vec![
                    Expr::Call {
                        function: "TO_CHAR".into(),
                        args: vec![other("sysdate"), string("x")]
                    },
                    string("y")
                ]
            }
        );
        assert_eq!(
            value_of("app.pkg.f(1)"),
            Expr::Call {
                function: "APP.PKG.F".into(),
                args: vec![number("1")]
            }
        );
    }

    #[test]
    fn sequences_are_next_values_with_or_without_a_schema() {
        assert_eq!(
            value_of("APP.AUDIT_SEQ.NEXTVAL"),
            Expr::NextValue(Name {
                schema: Some(ident("APP", false)),
                name: ident("AUDIT_SEQ", false)
            })
        );
        assert_eq!(
            value_of("audit_seq.nextval"),
            Expr::NextValue(Name {
                schema: None,
                name: ident("AUDIT_SEQ", false)
            })
        );
        assert_eq!(value_of("seq.\"NEXTVAL\""), other("seq.\"NEXTVAL\""));
        assert_eq!(value_of("seq.currval"), other("seq.currval"));
        assert_eq!(value_of("a.b.c.nextval"), other("a.b.c.nextval"));
    }

    #[test]
    fn anything_else_is_kept_as_written() {
        for sql in [
            "SYSDATE",
            "'a' || 'b'",
            "(1 + 2)",
            "f(1) + g(2)",
            "f(1,,2)",
            "- f(1) * 2",
            "CASE WHEN 1 = 1 THEN 'y' END",
            "(SELECT MAX(x) FROM t)",
            ":1",
            "INTERVAL '1' DAY",
            "DATE N'2003-06-17'",
            "TIMESTAMP '2003-06-17 10:00:00' AT TIME ZONE 'UTC'",
        ] {
            assert_eq!(value_of(sql), other(sql), "{sql}");
        }
    }

    #[test]
    fn deep_nesting_is_kept_as_written_without_exhausting_the_stack() {
        let depth = 20_000;
        let nested = format!("{}1{}", "f(".repeat(depth), ")".repeat(depth));
        assert!(matches!(value_of(&nested), Expr::Call { .. }));
        let negated = format!("{}1", "- ".repeat(depth));
        assert_eq!(value_of(&negated), other(&negated));
    }

    #[test]
    fn insert_all_has_a_row_per_into_with_its_own_span_and_line() {
        let text = "INSERT ALL\n  INTO app.t (a,b) VALUES (1,'x')\n  INTO \"APP\".\"T\" (\"A\",\n    \"B\")\nVALUES (2,\n    'y')\nSELECT 1 FROM DUAL";
        let rows = rows(text);
        assert_eq!(rows.len(), 2);
        let clause = |r: &InsertRow| {
            let start = usize::try_from(r.span.start - 100).unwrap();
            let end = usize::try_from(r.span.end - 100).unwrap();
            text.get(start..end).unwrap().to_owned()
        };
        assert_eq!(clause(&rows[0]), "INTO app.t (a,b) VALUES (1,'x')");
        assert_eq!(
            clause(&rows[1]),
            "INTO \"APP\".\"T\" (\"A\",\n    \"B\")\nVALUES (2,\n    'y')"
        );
        assert_eq!((rows[0].line, rows[1].line), (11, 12));
        assert_eq!(rows[1].values[1].expr, string("y"));
        assert_eq!(rows[1].table.name.value, "T");
        assert!(rows[1].table.name.quoted && !rows[0].table.name.quoted);
    }

    #[test]
    fn rows_from_a_query_are_not_followed() {
        for (text, what) in [
            ("INSERT INTO t SELECT * FROM s", "INSERT … SELECT"),
            (
                "insert into t (a, b) select 1, 2 from dual",
                "INSERT … SELECT",
            ),
            (
                "INSERT INTO t WITH q AS (SELECT 1 FROM DUAL) SELECT * FROM q",
                "INSERT … SELECT",
            ),
            ("INSERT INTO t (SELECT * FROM s)", "INSERT … SELECT"),
            ("INSERT INTO t (a) (SELECT 1 FROM s)", "INSERT … SELECT"),
            (
                "INSERT INTO (SELECT a FROM t) VALUES (1)",
                "INSERT INTO a subquery",
            ),
            (
                "INSERT FIRST WHEN 1=1 THEN INTO t VALUES (1) SELECT 1 FROM DUAL",
                "INSERT FIRST",
            ),
            (
                "INSERT ALL WHEN 1=1 THEN INTO t VALUES (1) SELECT 1 FROM DUAL",
                "INSERT ALL WHEN",
            ),
            (
                "INSERT ALL INTO t VALUES (1) SELECT * FROM s",
                "INSERT ALL … SELECT",
            ),
            (
                "INSERT ALL INTO t VALUES (1) SELECT 1 FROM DUAL WHERE 1 = 0",
                "INSERT ALL … SELECT",
            ),
            (
                "INSERT ALL INTO t VALUES (1) SELECT 1 FROM DUAL CONNECT BY LEVEL <= 3",
                "INSERT ALL … SELECT",
            ),
            (
                "INSERT ALL INTO t VALUES (1) INTO u SELECT 1 FROM DUAL",
                "INSERT ALL … SELECT",
            ),
            (
                "INSERT ALL INTO t VALUES (1) WITH q AS (SELECT 1 FROM DUAL) SELECT * FROM q",
                "INSERT ALL … SELECT",
            ),
            (
                "INSERT /*+ APPEND */ INTO\n\"APP\".\"EMPLOYEES\"  KUT$ (\"EMP_ID\", \"NAME\")\nSELECT /*+ x */ \"EMP_ID\", \"NAME\" FROM \"APP\".\"EMPLOYEES\"@ KU$",
                "INSERT … SELECT",
            ),
            (
                "INSERT ALL INTO t VALUES (7) SELECT 1 UNION ALL SELECT 2 FROM DUAL",
                "INSERT ALL … SELECT",
            ),
            (
                "INSERT ALL INTO t VALUES (7) SELECT 1 MINUS SELECT 1 FROM DUAL",
                "INSERT ALL … SELECT",
            ),
            (
                "INSERT ALL INTO t VALUES (7) SELECT (SELECT 1 FROM s) FROM DUAL",
                "INSERT ALL … SELECT",
            ),
        ] {
            assert_eq!(skipped(text), what, "{text}");
        }
        for one_row in [
            "SELECT 1 FROM DUAL",
            "select * from dual",
            "SELECT NULL FROM Dual",
        ] {
            assert!(
                matches!(
                    parse(&format!("INSERT ALL INTO t VALUES (1) {one_row}")),
                    Ok(Some(Statement::Insert(_)))
                ),
                "{one_row}"
            );
        }
    }

    #[test]
    fn other_statements_are_not_inserts() {
        for text in [
            "COMMIT",
            "CREATE TABLE t (a NUMBER)",
            "insertx into t values (1)",
            "UPDATE t SET a = 'open",
            "",
            "-- INSERT INTO t VALUES (1)",
        ] {
            assert_eq!(parse(text), Ok(None), "{text}");
        }
    }

    #[test]
    fn a_column_listed_twice_is_an_error_at_its_second_mention() {
        let text = "INSERT INTO T (A, \"A\") VALUES (1, 2)";
        let e = error(text);
        assert_eq!(e.message, "column A is listed twice");
        assert_eq!(e.offset, 100 + to_u64(text.find("\"A\"").unwrap()));
    }

    #[test]
    fn a_malformed_insert_is_an_error_where_reading_stopped() {
        let text = "Insert into t (a,b) values ('1'\nInsert into t (a,b) values ('2','3')";
        assert_eq!(
            error(text),
            ParseError {
                offset: 100 + to_u64(text.find("Insert into t (a,b) values ('2'").unwrap()),
                line: 11,
                message: "expected `)` or `,` in the value list, found `Insert`".into()
            }
        );
        assert_eq!(
            error("insert into t values (1 values (2)").message,
            "expected `)` or `,` in the value list, found `values`"
        );
        let unterminated = error("insert into t values ('1',\n'open)");
        assert_eq!(
            (
                unterminated.offset,
                unterminated.line,
                unterminated.message.as_str()
            ),
            (127, 11, "unterminated string")
        );
        for (text, message) in [
            (
                "INSERT t VALUES (1)",
                "expected INTO, ALL or FIRST after INSERT, found `t`",
            ),
            (
                "INSERT INTO",
                "expected a table name, found the end of the statement",
            ),
            (
                "INSERT INTO s. VALUES (1)",
                "expected a column name, found `1`",
            ),
            (
                "INSERT INTO s.",
                "expected a table name, found the end of the statement",
            ),
            (
                "INSERT INTO t (a b) VALUES (1)",
                "expected `)` or `,` in the column list, found `b`",
            ),
            (
                "INSERT INTO t (a,) VALUES (1)",
                "expected a column name, found `)`",
            ),
            (
                "INSERT INTO t VALUES",
                "expected `(` after VALUES, found the end of the statement",
            ),
            ("INSERT INTO t VALUES ()", "expected a value, found `)`"),
            ("INSERT INTO t VALUES (1,,2)", "expected a value, found `,`"),
            (
                "INSERT INTO t VALUES (1",
                "expected `)` or `,` in the value list, found the end of the statement",
            ),
            (
                "INSERT INTO t VALUES (f(1)",
                "expected `)` or `,` in the value list, found the end of the statement",
            ),
            (
                "INSERT INTO t VALUES (1; x",
                "expected `)` or `,` in the value list, found `;`",
            ),
            (
                "INSERT INTO t VALUES (1) RETURNING a INTO :a",
                "expected the end of the statement after the value list, found `RETURNING`",
            ),
            (
                "INSERT INTO t VALUES (1) ;",
                "expected the end of the statement after the value list, found `;`",
            ),
            (
                "INSERT INTO t = 1",
                "expected VALUES or a query after the table, found `=`",
            ),
            (
                "INSERT ALL SELECT 1 FROM DUAL",
                "expected INTO after INSERT ALL, found `SELECT`",
            ),
            (
                "INSERT ALL INTO t VALUES (1)",
                "expected INTO or a query after the INTO clauses, found the end of the statement",
            ),
            (
                "INSERT ALL INTO t VALUES (1 SELECT 1 FROM DUAL",
                "expected `)` or `,` in the value list, found `SELECT`",
            ),
        ] {
            assert_eq!(error(text).message, message, "{text}");
        }
        let long = format!("INSERT INTO t VALUES (1) '{}'", "x".repeat(100));
        assert!(
            error(&long).message.ends_with("xxx…`"),
            "{}",
            error(&long).message
        );
    }

    #[test]
    fn identifiers_and_strings_follow_the_encoding() {
        let mut latin1 = piece("");
        latin1.text = b"insert into \"T\xe4\" (n\xe4me) values ('\xe4')".to_vec();
        let Ok(Some(Statement::Insert(rows))) = parse_insert(&latin1, Encoding::Windows1252) else {
            panic!()
        };
        assert_eq!(rows[0].table.name, ident("Tä", true));
        assert_eq!(rows[0].columns, Some(vec![ident("NÄME", false)]));
        assert_eq!(
            rows[0].values[0].expr,
            Expr::String {
                bytes: b"\xe4".to_vec().into(),
                national: false
            }
        );
    }
}

#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::{Memo, parse_insert, parse_insert_with};
    use crate::source::{Encoding, Piece, PieceKind, Span};
    use crate::statement::Statement;

    /// A single-row insert put together from parts that a repeated start can share or just miss: the same table in
    /// other cases and quotes, another column list or none, an alias, blanks, comments and line ends before
    /// `VALUES`, and endings that aren't an insert or don't read.
    fn insert_row() -> impl Strategy<Value = String> {
        let pick = |parts: &[&'static str]| prop::sample::select(parts.to_vec());
        (
            pick(&["INSERT INTO", "insert into", "Insert  into"]),
            pick(&[" T", " APP.T", " \"APP\".\"T\"", " T2", " t", " APP .T"]),
            pick(&["", " KUT$"]),
            pick(&[
                "",
                " (A,B)",
                " (B,A)",
                "(A, B)",
                " (A)",
                " (\"A\",B)",
                " (A,A)",
                " (A,B",
            ]),
            pick(&[" ", "  ", "\n", " /*c*/ ", "", "\t"]),
            pick(&["VALUES", "values", "VALUESX", "SELECT 1 FROM DUAL", "VALUE"]),
            pick(&[" (1,2)", " (1)", "(1,2,3)", " ('a;b',2)", " (1,", ""]),
        )
            .prop_map(|(head, table, alias, columns, gap, values, list)| {
                format!("{head}{table}{alias}{columns}{gap}{values}{list}")
            })
    }

    /// Insert-shaped text: the grammar's words and punctuation in any order, and any byte now and then.
    fn statement() -> impl Strategy<Value = Vec<u8>> {
        let fragment = prop_oneof![
            8 => prop::sample::select(vec![
                "INSERT", "ALL", "FIRST", "INTO", "VALUES", "SELECT", "FROM", "DUAL", "WITH", "WHEN", "t", "s.t",
                "\"Q\"", "(", ")", ",", ".", "-", ";", "'x'", "''", "q'[y]'", "N'z'", "1", "1.5E3", "NULL",
                "DEFAULT", "DATE", "TIMESTAMP", "to_date", "seq.NEXTVAL", "\n", "/*", "*/", "--", "'", "\"",
            ]).prop_map(|s| s.as_bytes().to_vec()),
            1 => any::<u8>().prop_map(|b| vec![b]),
        ];
        (prop::collection::vec(fragment, 0..60), any::<bool>()).prop_map(|(fragments, insert)| {
            let mut text = if insert {
                b"INSERT ".to_vec()
            } else {
                Vec::new()
            };
            for fragment in fragments {
                text.extend_from_slice(&fragment);
                text.push(b' ');
            }
            text
        })
    }

    proptest! {
        #[test]
        fn parsing_never_panics_and_stays_inside_the_piece(text in statement(), start in 0_u64..1000) {
            let end = start + u64::try_from(text.len()).unwrap() + 1;
            let piece = Piece { kind: PieceKind::Sql, span: Span { start, end }, line: 3, text };
            let inside = |span: Span| start <= span.start && span.start <= span.end && span.end <= end;
            match parse_insert(&piece, Encoding::Utf8) {
                Ok(Some(Statement::Insert(rows))) => {
                    prop_assert!(!rows.is_empty());
                    for row in rows {
                        prop_assert!(inside(row.span) && row.line >= 3);
                        for value in row.values {
                            prop_assert!(inside(value.span));
                            prop_assert!(row.span.start <= value.span.start && value.span.end <= row.span.end);
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => prop_assert!(start <= error.offset && error.offset < end && error.line >= 3),
            }
        }

        #[test]
        fn a_repeated_start_reads_as_reading_every_row_whole(rows in prop::collection::vec(insert_row(), 1..12)) {
            let mut memo = Memo::default();
            let mut start = 0;
            for text in rows {
                let end = start + u64::try_from(text.len()).unwrap();
                let piece = Piece { kind: PieceKind::Sql, span: Span { start, end }, line: 1, text: text.into_bytes() };
                let whole = format!("{:?}", parse_insert(&piece, Encoding::Utf8));
                let remembered = format!("{:?}", parse_insert_with(&piece, Encoding::Utf8, Some(&mut memo)));
                prop_assert_eq!(remembered, whole, "{}", String::from_utf8_lossy(&piece.text));
                start = end + 1;
            }
        }
    }
}
