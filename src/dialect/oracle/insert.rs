//! `INSERT` statements as SQL Developer, SQLcl and the generator write them: one row (`INSERT INTO … VALUES (…)`)
//! or several (`INSERT ALL INTO … VALUES (…) … SELECT … FROM DUAL`). An insert whose rows come from a query is
//! skipped as not followed. Values are kept as written; [`super::eval`] says what they mean.

use super::RULES;
use super::lex::{self, Token, TokenKind};
use crate::source::{Encoding, Piece, Span};
use crate::statement::{Expr, Ident, InsertRow, Name, ParseError, Skip, Statement, Value};

/// Calls and negations nested deeper than this are kept as written, so no input can exhaust the stack.
const MAX_DEPTH: usize = 64;

/// Words that can't appear outside parentheses in a value, so a value list missing its `)` stops at them.
const STOP_WORDS: [&str; 4] = ["INSERT", "INTO", "VALUES", "SELECT"];

/// The insert `piece` holds, or `None` when it isn't an `INSERT`.
pub fn parse_insert(piece: &Piece, encoding: Encoding) -> Result<Option<Statement>, ParseError> {
    let text = piece.text.as_slice();
    let mut parser = Parser {
        piece,
        text,
        encoding,
        tokens: Vec::new(),
        pos: 0,
        counted: (0, piece.line),
    };
    match lex::lex(text) {
        Ok(tokens) if parser.starts_insert(&tokens) => {
            parser.tokens = tokens;
            parser.insert().map(Some)
        }
        Ok(_) => Ok(None),
        Err(error) => {
            let head = text
                .get(..error.offset)
                .and_then(|head| lex::lex(head).ok())
                .unwrap_or_default();
            if parser.starts_insert(&head) {
                Err(parser.error_at(error.offset, error.message.to_owned()))
            } else {
                Ok(None)
            }
        }
    }
}

fn not_followed(what: &str) -> Statement {
    Statement::Skipped(Skip::NotFollowed(what.to_owned()))
}

struct Parser<'a> {
    piece: &'a Piece,
    text: &'a [u8],
    encoding: Encoding,
    tokens: Vec<Token>,
    pos: usize,
    /// An offset in `text` and its line, so a statement's lines are counted once however many rows it has.
    counted: (usize, u64),
}

impl Parser<'_> {
    fn starts_insert(&self, tokens: &[Token]) -> bool {
        tokens.first().is_some_and(|&t| self.is_word(t, "INSERT"))
    }

    fn insert(&mut self) -> Result<Statement, ParseError> {
        self.pos = 1;
        if self.eat_word("INTO") {
            self.single()
        } else if self.eat_word("ALL") {
            self.multi()
        } else if self.peek_word("FIRST") {
            Ok(not_followed("INSERT FIRST"))
        } else {
            Err(self.expected("INTO, ALL or FIRST after INSERT"))
        }
    }

    fn single(&mut self) -> Result<Statement, ParseError> {
        if self.peek_punct(b'(') {
            return Ok(not_followed("INSERT INTO a subquery"));
        }
        let (table, columns) = self.target()?;
        if !self.eat_word("VALUES") {
            return if self.at_query(self.pos) {
                Ok(not_followed("INSERT … SELECT"))
            } else {
                Err(self.expected("VALUES or a query after the table"))
            };
        }
        let (values, _) = self.values()?;
        if self.peek().is_some() {
            return Err(self.expected("the end of the statement after the value list"));
        }
        Ok(Statement::Insert(vec![InsertRow {
            table,
            columns,
            values,
            span: self.piece.span,
            line: self.piece.line,
        }]))
    }

    fn multi(&mut self) -> Result<Statement, ParseError> {
        let mut rows = Vec::new();
        while let Some(into) = self.peek().filter(|&t| self.is_word(t, "INTO")) {
            self.pos += 1;
            let (table, columns) = self.target()?;
            if !self.eat_word("VALUES") {
                // Without VALUES the row is the query's select list.
                return Ok(not_followed("INSERT ALL … SELECT"));
            }
            let (values, end) = self.values()?;
            rows.push(InsertRow {
                table,
                columns,
                values,
                span: self.span(into.start, end),
                line: self.line_at(into.start),
            });
        }
        if rows.is_empty() {
            return if self.peek_word("WHEN") {
                Ok(not_followed("INSERT ALL WHEN"))
            } else {
                Err(self.expected("INTO after INSERT ALL"))
            };
        }
        if !self.eat_word("SELECT") {
            return if self.at_query(self.pos) {
                Ok(not_followed("INSERT ALL … SELECT"))
            } else {
                Err(self.expected("INTO or a query after the INTO clauses"))
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
            self.tokens.get(self.pos..),
            Some(&[_, from, dual]) if self.is_word(from, "FROM") && self.is_word(dual, "DUAL")
        )
    }

    /// `name [alias] [(columns)]`.
    fn target(&mut self) -> Result<(Name, Option<Vec<Ident>>), ParseError> {
        let table = self.name()?;
        // DBMS_METADATA aliases its target: `INTO "APP"."EMPLOYEES" KUT$ (…)`.
        if let Some(token) = self.peek() {
            let alias = match token.kind {
                TokenKind::QuotedIdent => true,
                TokenKind::Word => !["VALUES", "SELECT", "WITH", "INTO"]
                    .iter()
                    .any(|w| self.is_word(token, w)),
                _ => false,
            };
            if alias {
                self.pos += 1;
            }
        }
        let columns = if self.peek_punct(b'(') && !self.at_query(self.pos + 1) {
            Some(self.columns()?)
        } else {
            None
        };
        Ok((table, columns))
    }

    fn name(&mut self) -> Result<Name, ParseError> {
        let first = self.ident().ok_or_else(|| self.expected("a table name"))?;
        if !self.eat_punct(b'.') {
            return Ok(Name {
                schema: None,
                name: first,
            });
        }
        let name = self
            .ident()
            .ok_or_else(|| self.expected("a table name after the schema"))?;
        Ok(Name {
            schema: Some(first),
            name,
        })
    }

    fn columns(&mut self) -> Result<Vec<Ident>, ParseError> {
        self.pos += 1;
        let mut columns = Vec::new();
        loop {
            columns.push(self.ident().ok_or_else(|| self.expected("a column name"))?);
            if self.eat_punct(b')') {
                return Ok(columns);
            }
            if !self.eat_punct(b',') {
                return Err(self.expected("`)` or `,` in the column list"));
            }
        }
    }

    /// `(value, …)`, and the offset after its `)`.
    fn values(&mut self) -> Result<(Vec<Value>, usize), ParseError> {
        if !self.eat_punct(b'(') {
            return Err(self.expected("`(` after VALUES"));
        }
        let mut values = Vec::new();
        loop {
            let run = self.run();
            let tokens = self.tokens.get(run).unwrap_or_default();
            let (Some(first), Some(last)) = (tokens.first(), tokens.last()) else {
                return Err(self.expected("a value"));
            };
            values.push(Value {
                expr: self.expr(tokens, 0),
                span: self.span(first.start, last.end),
            });
            match self.peek() {
                Some(t) if self.is_punct(t, b',') => self.pos += 1,
                Some(t) if self.is_punct(t, b')') => {
                    self.pos += 1;
                    return Ok((values, t.end));
                }
                _ => return Err(self.expected("`)` or `,` in the value list")),
            }
        }
    }

    /// The tokens of one value: up to a `,` or `)` outside parentheses.
    fn run(&mut self) -> std::ops::Range<usize> {
        let start = self.pos;
        let mut depth = 0_usize;
        while let Some(token) = self.peek() {
            if self.is_punct(token, b'(') {
                depth += 1;
            } else if depth == 0
                && (self.is_punct(token, b',')
                    || self.is_punct(token, b')')
                    || self.is_punct(token, b';')
                    || STOP_WORDS.iter().any(|w| self.is_word(token, w)))
            {
                break;
            } else if self.is_punct(token, b')') {
                depth -= 1;
            }
            self.pos += 1;
        }
        start..self.pos
    }

    /// A value by its shape; anything without one of the shapes below is kept as written.
    fn expr(&self, tokens: &[Token], depth: usize) -> Expr {
        self.shaped(tokens, depth)
            .unwrap_or_else(|| Expr::Other(self.source(tokens)))
    }

    fn shaped(&self, tokens: &[Token], depth: usize) -> Option<Expr> {
        if depth > MAX_DEPTH {
            return None;
        }
        match *tokens {
            [token] => self.literal(token),
            [minus, ref rest @ ..] if self.is_punct(minus, b'-') => {
                Some(Expr::Negate(Box::new(self.shaped(rest, depth + 1)?)))
            }
            [keyword, string]
                if (self.is_word(keyword, "DATE") || self.is_word(keyword, "TIMESTAMP"))
                    && string.kind == (TokenKind::String { national: false }) =>
            {
                Some(Expr::Typed {
                    type_name: self.word(keyword).to_ascii_uppercase(),
                    text: self
                        .encoding
                        .decode(&string.content(self.text))
                        .into_owned(),
                })
            }
            [.., last] if self.is_punct(last, b')') => self.call(tokens, depth),
            _ => self.next_value(tokens),
        }
    }

    fn literal(&self, token: Token) -> Option<Expr> {
        match token.kind {
            TokenKind::Number => Some(Expr::Number(self.word(token))),
            TokenKind::String { national } | TokenKind::AltString { national } => {
                Some(Expr::String {
                    bytes: token.content(self.text),
                    national,
                })
            }
            _ if self.is_word(token, "NULL") => Some(Expr::Null),
            _ if self.is_word(token, "DEFAULT") => Some(Expr::Default),
            _ => None,
        }
    }

    /// `name(args)`, where the `(` after the name closes at the last token.
    fn call(&self, tokens: &[Token], depth: usize) -> Option<Expr> {
        let open = tokens.iter().position(|&t| self.is_punct(t, b'('))?;
        let (head, rest) = tokens.split_at_checked(open)?;
        let function = self
            .dotted(head)?
            .iter()
            .map(|part| part.value.as_str())
            .collect::<Vec<_>>()
            .join(".");
        let inner = rest.get(1..rest.len().checked_sub(1)?)?;
        let args = self
            .split_args(inner)?
            .into_iter()
            .map(|arg| (!arg.is_empty()).then(|| self.expr(arg, depth + 1)))
            .collect::<Option<_>>()?;
        Some(Expr::Call { function, args })
    }

    /// A call's arguments, split at the commas outside parentheses; `None` when a `)` closes the call early.
    fn split_args<'t>(&self, inner: &'t [Token]) -> Option<Vec<&'t [Token]>> {
        if inner.is_empty() {
            return Some(Vec::new());
        }
        let mut args = Vec::new();
        let (mut start, mut depth) = (0, 0_usize);
        for (i, &token) in inner.iter().enumerate() {
            if self.is_punct(token, b'(') {
                depth += 1;
            } else if self.is_punct(token, b')') {
                depth = depth.checked_sub(1)?;
            } else if depth == 0 && self.is_punct(token, b',') {
                args.push(inner.get(start..i)?);
                start = i + 1;
            }
        }
        args.push(inner.get(start..)?);
        (depth == 0).then_some(args)
    }

    /// `[schema.]sequence.NEXTVAL`.
    fn next_value(&self, tokens: &[Token]) -> Option<Expr> {
        let mut parts = self.dotted(tokens)?;
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
    fn dotted(&self, tokens: &[Token]) -> Option<Vec<Ident>> {
        let mut parts = Vec::new();
        let mut iter = tokens.iter();
        loop {
            parts.push(self.ident_of(*iter.next()?)?);
            match iter.next() {
                None => return Some(parts),
                Some(&dot) if self.is_punct(dot, b'.') => {}
                Some(_) => return None,
            }
        }
    }

    fn ident(&mut self) -> Option<Ident> {
        let ident = self.ident_of(self.peek()?)?;
        self.pos += 1;
        Some(ident)
    }

    fn ident_of(&self, token: Token) -> Option<Ident> {
        match token.kind {
            TokenKind::Word => Some(Ident {
                value: RULES.fold(&self.word(token)),
                quoted: false,
            }),
            TokenKind::QuotedIdent => Some(Ident {
                value: self.encoding.decode(&token.content(self.text)).into_owned(),
                quoted: true,
            }),
            _ => None,
        }
    }

    fn word(&self, token: Token) -> String {
        self.encoding.decode(token.text(self.text)).into_owned()
    }

    /// The text of `tokens` as written, from the first one's start to the last one's end.
    fn source(&self, tokens: &[Token]) -> String {
        let (Some(first), Some(last)) = (tokens.first(), tokens.last()) else {
            return String::new();
        };
        self.encoding
            .decode(self.text.get(first.start..last.end).unwrap_or_default())
            .into_owned()
    }

    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.pos).copied()
    }

    fn is_word(&self, token: Token, word: &str) -> bool {
        token.kind == TokenKind::Word && token.text(self.text).eq_ignore_ascii_case(word.as_bytes())
    }

    fn is_punct(&self, token: Token, c: u8) -> bool {
        token.kind == TokenKind::Punct && token.text(self.text) == [c]
    }

    fn peek_word(&self, word: &str) -> bool {
        self.peek().is_some_and(|t| self.is_word(t, word))
    }

    fn peek_punct(&self, c: u8) -> bool {
        self.peek().is_some_and(|t| self.is_punct(t, c))
    }

    /// Whether a query starts at token `at`.
    fn at_query(&self, at: usize) -> bool {
        self.tokens.get(at).is_some_and(|&t| {
            self.is_word(t, "SELECT") || self.is_word(t, "WITH") || self.is_punct(t, b'(')
        })
    }

    fn eat_word(&mut self, word: &str) -> bool {
        let found = self.peek_word(word);
        self.pos += usize::from(found);
        found
    }

    fn eat_punct(&mut self, c: u8) -> bool {
        let found = self.peek_punct(c);
        self.pos += usize::from(found);
        found
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span {
            start: self.piece.span.start + to_u64(start),
            end: self.piece.span.start + to_u64(end),
        }
    }

    /// The line `offset` in the piece's text is on.
    #[expect(
        clippy::naive_bytecount,
        reason = "one statement's text; not worth a dependency"
    )]
    fn line_at(&mut self, offset: usize) -> u64 {
        let (from, line) = if offset >= self.counted.0 {
            self.counted
        } else {
            (0, self.piece.line)
        };
        let newlines = self
            .text
            .get(from..offset)
            .unwrap_or_default()
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        let line = line + to_u64(newlines);
        self.counted = (offset, line);
        line
    }

    fn error_at(&mut self, offset: usize, message: String) -> ParseError {
        ParseError {
            offset: self.piece.span.start + to_u64(offset),
            line: self.line_at(offset),
            message,
        }
    }

    /// An error at the next token: "expected `what`, found …".
    fn expected(&mut self, what: &str) -> ParseError {
        let (offset, found) = match self.peek() {
            Some(token) => {
                let text = self.word(token);
                let short: String = text.chars().take(40).collect();
                let ellipsis = if short.len() < text.len() { "…" } else { "" };
                (token.start, format!("`{short}{ellipsis}`"))
            }
            None => (self.text.len(), "the end of the statement".to_owned()),
        };
        self.error_at(offset, format!("expected {what}, found {found}"))
    }
}

fn to_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::PieceKind;

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

    fn parse(text: &str) -> Result<Option<Statement>, ParseError> {
        parse_insert(&piece(text), Encoding::Utf8)
    }

    fn rows(text: &str) -> Vec<InsertRow> {
        match parse(text) {
            Ok(Some(Statement::Insert(rows))) => rows,
            other => panic!("{text}: {other:?}"),
        }
    }

    fn row(text: &str) -> InsertRow {
        let mut rows = rows(text);
        assert_eq!(rows.len(), 1);
        rows.remove(0)
    }

    fn exprs(text: &str) -> Vec<Expr> {
        row(text).values.into_iter().map(|v| v.expr).collect()
    }

    fn value_of(sql: &str) -> Expr {
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

    fn ident(value: &str, quoted: bool) -> Ident {
        Ident {
            value: value.into(),
            quoted,
        }
    }

    fn string(s: &str) -> Expr {
        Expr::String {
            bytes: s.as_bytes().to_vec(),
            national: false,
        }
    }

    fn number(s: &str) -> Expr {
        Expr::Number(s.into())
    }

    fn other(s: &str) -> Expr {
        Expr::Other(s.into())
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
                bytes: b"x".to_vec(),
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
                "expected a table name after the schema, found the end of the statement",
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
                bytes: b"\xe4".to_vec(),
                national: false
            }
        );
    }
}

#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::parse_insert;
    use crate::source::{Encoding, Piece, PieceKind, Span};
    use crate::statement::Statement;

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
    }
}
