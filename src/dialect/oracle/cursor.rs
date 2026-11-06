//! A cursor over one piece's tokens, for the Oracle parsers: keyword tests, names folded by the dialect's rules, and
//! errors placed by offset and line in the file.

use std::borrow::Cow;
use std::cell::Cell;

use super::RULES;
use super::lex::{self, Token, TokenKind, is_word_byte};
use crate::source::{Encoding, Piece, Span, to_u64};
use crate::statement::{Ident, Name, ParseError};

/// Whether the statement's first word, past blanks and comments, is `keyword`, without lexing the rest.
pub(crate) fn first_word_is(text: &[u8], keyword: &str) -> bool {
    words(text)
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case(keyword.as_bytes()))
}

/// Up to `n` leading words upper-cased, past blanks and comments, without lexing the rest.
pub(crate) fn leading_words(text: &[u8], n: usize) -> Vec<String> {
    words(text)
        .take(n)
        .map(|word| String::from_utf8_lossy(word).to_ascii_uppercase())
        .collect()
}

/// The leading words as written, past blanks and comments, up to the first byte that is neither.
pub(crate) fn words(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = text;
    std::iter::from_fn(move || {
        rest = past_blanks_and_comments(rest);
        let len = rest.iter().take_while(|&&b| is_word_byte(b)).count();
        let (word, after) = rest.split_at_checked(len).filter(|(w, _)| !w.is_empty())?;
        rest = after;
        Some(word)
    })
}

fn past_blanks_and_comments(text: &[u8]) -> &[u8] {
    let mut rest = text;
    loop {
        rest = rest.trim_ascii_start();
        if let Some(after) = rest.strip_prefix(b"--") {
            let end = after.iter().position(|&b| b == b'\n');
            rest = end.map_or(&[][..], |n| after.get(n..).unwrap_or_default());
        } else if let Some(after) = rest.strip_prefix(b"/*") {
            let end = after.windows(2).position(|w| w == b"*/");
            rest = end.map_or(&[][..], |n| after.get(n + 2..).unwrap_or_default());
        } else {
            return rest;
        }
    }
}

pub(crate) struct Cursor<'a> {
    piece: &'a Piece,
    encoding: Encoding,
    tokens: Vec<Token>,
    pos: usize,
    /// An offset in the piece's text and its line, so a statement's lines are counted once however many rows or
    /// errors ask for one.
    counted: Cell<(usize, u64)>,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(piece: &'a Piece, encoding: Encoding) -> Result<Self, ParseError> {
        let mut cursor = Self {
            piece,
            encoding,
            tokens: Vec::new(),
            pos: 0,
            counted: Cell::new((0, piece.line)),
        };
        cursor.tokens =
            lex::lex(&piece.text).map_err(|e| cursor.error_at(e.offset, e.message.to_owned()))?;
        Ok(cursor)
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    pub(crate) fn restore(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub(crate) fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub(crate) fn token(&self, index: usize) -> Option<Token> {
        self.tokens.get(index).copied()
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    pub(crate) fn peek(&self) -> Option<Token> {
        self.peek_at(0)
    }

    pub(crate) fn peek_at(&self, ahead: usize) -> Option<Token> {
        self.tokens.get(self.pos + ahead).copied()
    }

    pub(crate) fn bump(&mut self) -> Option<Token> {
        let token = self.peek();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    pub(crate) fn is_word(&self, token: Token, keyword: &str) -> bool {
        // The length first: most tokens tested against a keyword are another word.
        token.kind == TokenKind::Word
            && token.end - token.start == keyword.len()
            && token
                .text(&self.piece.text)
                .eq_ignore_ascii_case(keyword.as_bytes())
    }

    pub(crate) fn is_punct(&self, token: Token, c: u8) -> bool {
        token.kind == TokenKind::Punct && token.text(&self.piece.text) == [c]
    }

    pub(crate) fn word_at(&self, ahead: usize, keyword: &str) -> bool {
        self.peek_at(ahead)
            .is_some_and(|t| self.is_word(t, keyword))
    }

    pub(crate) fn punct_at(&self, ahead: usize, c: u8) -> bool {
        self.peek_at(ahead).is_some_and(|t| self.is_punct(t, c))
    }

    pub(crate) fn at_word(&self, keyword: &str) -> bool {
        self.word_at(0, keyword)
    }

    pub(crate) fn at_any_word(&self, keywords: &[&str]) -> bool {
        keywords.iter().any(|k| self.at_word(k))
    }

    /// The next tokens are these keywords, in order.
    pub(crate) fn at_words(&self, keywords: &[&str]) -> bool {
        keywords.iter().enumerate().all(|(i, k)| self.word_at(i, k))
    }

    pub(crate) fn at_punct(&self, c: u8) -> bool {
        self.punct_at(0, c)
    }

    pub(crate) fn eat_word(&mut self, keyword: &str) -> bool {
        self.eat_words(&[keyword])
    }

    pub(crate) fn eat_any_word(&mut self, keywords: &[&str]) -> bool {
        keywords.iter().any(|k| self.eat_word(k))
    }

    pub(crate) fn eat_words(&mut self, keywords: &[&str]) -> bool {
        let found = self.at_words(keywords);
        if found {
            self.pos += keywords.len();
        }
        found
    }

    pub(crate) fn eat_punct(&mut self, c: u8) -> bool {
        let found = self.at_punct(c);
        if found {
            self.pos += 1;
        }
        found
    }

    pub(crate) fn expect_word(&mut self, keyword: &str) -> Result<(), ParseError> {
        if self.eat_word(keyword) {
            Ok(())
        } else {
            Err(self.expected(&format!("`{keyword}`")))
        }
    }

    pub(crate) fn expect_punct(&mut self, c: u8) -> Result<(), ParseError> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            Err(self.expected(&format!("`{}`", char::from(c))))
        }
    }

    /// At the end of a list item: `,`, `)` or the end of the statement.
    pub(crate) fn at_item_end(&self) -> bool {
        self.at_end() || self.at_punct(b',') || self.at_punct(b')')
    }

    /// Past tokens up to the end of the list item, parentheses balanced.
    pub(crate) fn skip_item(&mut self) -> Result<(), ParseError> {
        while !self.at_item_end() {
            self.skip_token()?;
        }
        Ok(())
    }

    /// Past one token, or one parenthesized group.
    pub(crate) fn skip_token(&mut self) -> Result<(), ParseError> {
        if self.at_punct(b'(') {
            self.skip_parens()
        } else {
            self.bump();
            Ok(())
        }
    }

    /// Past a parenthesized group, nested ones included; the cursor is at its `(`.
    pub(crate) fn skip_parens(&mut self) -> Result<(), ParseError> {
        let open = self.pos;
        self.expect_punct(b'(')?;
        let mut depth = 1_usize;
        while depth > 0 {
            if self.eat_punct(b'(') {
                depth += 1;
            } else if self.eat_punct(b')') {
                depth -= 1;
            } else if self.bump().is_none() {
                self.pos = open;
                return Err(self.error("this `(` is never closed".to_owned()));
            }
        }
        Ok(())
    }

    /// Past a parenthesized group, returning the text inside it as written.
    pub(crate) fn paren_text(&mut self) -> Result<String, ParseError> {
        let open = self.pos;
        self.skip_parens()?;
        Ok(self.text(open + 1, self.pos - 1))
    }

    /// The next token's text upper-cased, when it is a word.
    pub(crate) fn word(&self) -> Option<String> {
        self.peek()
            .filter(|t| t.kind == TokenKind::Word)
            .map(|t| self.token_text(t).to_ascii_uppercase())
    }

    /// A token's text as written, decoded.
    pub(crate) fn token_text(&self, token: Token) -> Cow<'a, str> {
        self.encoding.decode(token.text(&self.piece.text))
    }

    /// A string's or quoted name's content, as stored.
    pub(crate) fn content(&self, token: Token) -> Cow<'a, [u8]> {
        token.content(&self.piece.text)
    }

    /// A token as an identifier: a word folded by the dialect's rules, or a quoted name as written.
    pub(crate) fn ident_of(&self, token: Token) -> Option<Ident<'a>> {
        match token.kind {
            TokenKind::Word => Some(Ident {
                value: RULES.folded(self.token_text(token)),
                quoted: false,
            }),
            TokenKind::QuotedIdent => Some(Ident {
                value: self.decoded(self.content(token)),
                quoted: true,
            }),
            _ => None,
        }
    }

    pub(crate) fn ident(&mut self, what: &str) -> Result<Ident<'a>, ParseError> {
        let ident = self.peek().and_then(|t| self.ident_of(t));
        let ident = ident.ok_or_else(|| self.expected(what))?;
        self.pos += 1;
        Ok(ident)
    }

    /// A name, schema-qualified or not.
    pub(crate) fn name(&mut self, what: &str) -> Result<Name<'a>, ParseError> {
        let first = self.ident(what)?;
        if self.eat_punct(b'.') {
            Ok(Name {
                schema: Some(first),
                name: self.ident(what)?,
            })
        } else {
            Ok(Name {
                schema: None,
                name: first,
            })
        }
    }

    /// The source text from token `from` up to, not including, token `to`, decoded.
    pub(crate) fn text(&self, from: usize, to: usize) -> String {
        self.tokens
            .get(from..to)
            .map_or_else(String::new, |tokens| self.text_of(tokens).into_owned())
    }

    /// The text of `tokens` as written, from the first one's start to the last one's end.
    pub(crate) fn text_of(&self, tokens: &[Token]) -> Cow<'a, str> {
        let (Some(first), Some(last)) = (tokens.first(), tokens.last()) else {
            return Cow::Borrowed("");
        };
        self.encoding.decode(
            self.piece
                .text
                .get(first.start..last.end)
                .unwrap_or_default(),
        )
    }

    pub(crate) fn source(&self) -> &[u8] {
        &self.piece.text
    }

    pub(crate) fn decode(&self, bytes: &[u8]) -> String {
        self.encoding.decode(bytes).into_owned()
    }

    /// Bytes of the piece as text, borrowed when they need no decoding.
    pub(crate) fn decoded(&self, bytes: Cow<'a, [u8]>) -> Cow<'a, str> {
        match bytes {
            Cow::Borrowed(bytes) => self.encoding.decode(bytes),
            Cow::Owned(bytes) => Cow::Owned(self.encoding.decode(&bytes).into_owned()),
        }
    }

    /// Offsets in the piece's text as a span in the file.
    pub(crate) fn span(&self, start: usize, end: usize) -> Span {
        Span {
            start: self.piece.span.start + to_u64(start),
            end: self.piece.span.start + to_u64(end),
        }
    }

    /// The line an offset in the piece's text is on.
    pub(crate) fn line_at(&self, offset: usize) -> u64 {
        let (from, line) = match self.counted.get() {
            (from, line) if offset >= from => (from, line),
            _ => (0, self.piece.line),
        };
        let between = self.piece.text.get(from..offset).unwrap_or_default();
        let line = line + to_u64(between.split(|&b| b == b'\n').count() - 1);
        self.counted.set((offset, line));
        line
    }

    pub(crate) fn expected(&self, what: &str) -> ParseError {
        self.error(format!("expected {what}, found {}", self.found()))
    }

    /// An error at the current token, or at the end of the statement.
    pub(crate) fn error(&self, message: String) -> ParseError {
        let at = self.peek().map_or(self.piece.text.len(), |t| t.start);
        self.error_at(at, message)
    }

    pub(crate) fn error_at(&self, offset: usize, message: String) -> ParseError {
        ParseError {
            offset: self.piece.span.start + to_u64(offset),
            line: self.line_at(offset),
            message,
        }
    }

    fn found(&self) -> String {
        match self.peek() {
            None => "the end of the statement".to_owned(),
            Some(t) => {
                let text = self.token_text(t);
                let short: String = text.chars().take(40).collect();
                let more = if short.len() < text.len() { "…" } else { "" };
                format!("`{short}{more}`")
            }
        }
    }
}
