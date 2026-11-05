//! A cursor over one piece's tokens, for the Oracle parsers: keyword tests, names folded by the dialect's rules, and
//! errors placed by offset and line in the file.

use super::RULES;
use super::lex::{self, Token, TokenKind};
use crate::dialect::Fold;
use crate::source::{Encoding, Piece};
use crate::statement::{Ident, Name, ParseError};

pub(crate) struct Cursor<'a> {
    piece: &'a Piece,
    encoding: Encoding,
    tokens: Vec<Token>,
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(piece: &'a Piece, encoding: Encoding) -> Result<Self, ParseError> {
        let tokens = lex::lex(&piece.text).map_err(|e| ParseError {
            offset: piece.span.start + to_u64(e.offset),
            line: line_at(piece, e.offset),
            message: e.message.to_owned(),
        })?;
        Ok(Self {
            piece,
            encoding,
            tokens,
            pos: 0,
        })
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
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

    fn is_word(&self, token: Option<Token>, keyword: &str) -> bool {
        token.is_some_and(|t| {
            t.kind == TokenKind::Word
                && t.text(&self.piece.text)
                    .eq_ignore_ascii_case(keyword.as_bytes())
        })
    }

    pub(crate) fn at_word(&self, keyword: &str) -> bool {
        self.is_word(self.peek(), keyword)
    }

    pub(crate) fn at_any_word(&self, keywords: &[&str]) -> bool {
        keywords.iter().any(|k| self.at_word(k))
    }

    /// The next tokens are these keywords, in order.
    pub(crate) fn at_words(&self, keywords: &[&str]) -> bool {
        keywords
            .iter()
            .enumerate()
            .all(|(i, k)| self.is_word(self.peek_at(i), k))
    }

    pub(crate) fn word_at(&self, ahead: usize, keyword: &str) -> bool {
        self.is_word(self.peek_at(ahead), keyword)
    }

    pub(crate) fn punct_at(&self, ahead: usize, c: u8) -> bool {
        self.peek_at(ahead)
            .is_some_and(|t| t.kind == TokenKind::Punct && t.text(&self.piece.text) == [c])
    }

    pub(crate) fn restore(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub(crate) fn eat_any_word(&mut self, keywords: &[&str]) -> bool {
        keywords.iter().any(|k| self.eat_word(k))
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

    pub(crate) fn eat_word(&mut self, keyword: &str) -> bool {
        self.eat_words(&[keyword])
    }

    pub(crate) fn eat_words(&mut self, keywords: &[&str]) -> bool {
        let found = self.at_words(keywords);
        if found {
            self.pos += keywords.len();
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

    pub(crate) fn at_punct(&self, c: u8) -> bool {
        self.peek()
            .is_some_and(|t| t.kind == TokenKind::Punct && t.text(&self.piece.text) == [c])
    }

    pub(crate) fn eat_punct(&mut self, c: u8) -> bool {
        let found = self.at_punct(c);
        if found {
            self.pos += 1;
        }
        found
    }

    pub(crate) fn expect_punct(&mut self, c: u8) -> Result<(), ParseError> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            Err(self.expected(&format!("`{}`", char::from(c))))
        }
    }

    /// The next token's text upper-cased, when it is a word.
    pub(crate) fn word(&self) -> Option<String> {
        self.peek()
            .filter(|t| t.kind == TokenKind::Word)
            .map(|t| String::from_utf8_lossy(t.text(&self.piece.text)).to_ascii_uppercase())
    }

    /// An identifier: a word folded by the dialect's rules, or a quoted name as written.
    pub(crate) fn ident(&mut self, what: &str) -> Result<Ident, ParseError> {
        let ident = match self.peek() {
            Some(t) if t.kind == TokenKind::Word => {
                let text = self.encoding.decode(t.text(&self.piece.text));
                Ident {
                    value: match RULES.fold {
                        Fold::Upper => text.to_uppercase(),
                        Fold::Lower => text.to_lowercase(),
                    },
                    quoted: false,
                }
            }
            Some(t) if t.kind == TokenKind::QuotedIdent => Ident {
                value: self
                    .encoding
                    .decode(&t.content(&self.piece.text))
                    .into_owned(),
                quoted: true,
            },
            _ => return Err(self.expected(what)),
        };
        self.pos += 1;
        Ok(ident)
    }

    /// A name, schema-qualified or not.
    pub(crate) fn name(&mut self, what: &str) -> Result<Name, ParseError> {
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

    /// The source text from token `from` up to, not including, token `to`, decoded.
    pub(crate) fn text(&self, from: usize, to: usize) -> String {
        let (Some(first), Some(last)) = (
            self.tokens.get(from),
            to.checked_sub(1).and_then(|i| self.tokens.get(i)),
        ) else {
            return String::new();
        };
        let bytes = self
            .piece
            .text
            .get(first.start..last.end)
            .unwrap_or_default();
        self.encoding.decode(bytes).into_owned()
    }

    pub(crate) fn token(&self, index: usize) -> Option<Token> {
        self.tokens.get(index).copied()
    }

    pub(crate) fn source(&self) -> &[u8] {
        &self.piece.text
    }

    pub(crate) fn decode(&self, bytes: &[u8]) -> String {
        self.encoding.decode(bytes).into_owned()
    }

    pub(crate) fn expected(&self, what: &str) -> ParseError {
        self.error(format!("expected {what}, found {}", self.found()))
    }

    /// An error at the current token, or at the end of the statement.
    pub(crate) fn error(&self, message: String) -> ParseError {
        let at = self.peek().map_or(self.piece.text.len(), |t| t.start);
        ParseError {
            offset: self.piece.span.start + to_u64(at),
            line: line_at(self.piece, at),
            message,
        }
    }

    fn found(&self) -> String {
        match self.peek() {
            None => "the end of the statement".to_owned(),
            Some(t) => {
                let text = self.decode(t.text(&self.piece.text));
                let short: String = text.chars().take(40).collect();
                let more = if short.len() < text.len() { "…" } else { "" };
                format!("`{short}{more}`")
            }
        }
    }
}

fn line_at(piece: &Piece, at: usize) -> u64 {
    let before = piece.text.get(..at).unwrap_or(&piece.text);
    piece.line + to_u64(before.split(|&b| b == b'\n').count() - 1)
}

fn to_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}
