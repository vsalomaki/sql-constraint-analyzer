//! Tokens of one Oracle statement, as byte spans in its text. Like the splitter it works on bytes, so string
//! contents stay as stored until something decodes them. Comments, including `/*+ hints */`, are skipped. Words keep
//! their case; folding them is the parser's job, by the dialect's rules.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A keyword or unquoted identifier.
    Word,
    /// `"Name"`, with `""` for a quote inside.
    QuotedIdent,
    /// `'text'` with `''` for a quote inside, or `N'text'`.
    String {
        national: bool,
    },
    /// `q'[text]'` and its other delimiters, or `nq'…'`.
    AltString {
        national: bool,
    },
    Number,
    /// `:name` or `:1`.
    Bind,
    /// `||`, `:=`, `=>`, `<=`, `>=`, `<>`, `!=`, `^=`, `**`.
    Operator,
    /// Any other single character: `(`, `)`, `,`, `.`, `;`, `+`, `-`, …
    Punct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, source: &'a [u8]) -> &'a [u8] {
        source.get(self.start..self.end).unwrap_or_default()
    }

    /// A string's or quoted identifier's content without its quotes, with doubled quotes undone.
    pub fn content(&self, source: &[u8]) -> Vec<u8> {
        let text = self.text(source);
        match self.kind {
            TokenKind::String { national } => {
                let inner = text
                    .get(usize::from(national) + 1..text.len().saturating_sub(1))
                    .unwrap_or_default();
                undouble(inner, b'\'')
            }
            TokenKind::QuotedIdent => undouble(
                text.get(1..text.len().saturating_sub(1))
                    .unwrap_or_default(),
                b'"',
            ),
            // `q'[` … `]'`: the prefix, quote and opening delimiter, then the closing delimiter and quote.
            TokenKind::AltString { national } => {
                let quote = usize::from(national) + 1;
                let Some((delimiter, open_len)) = alternative_delimiter(text, quote) else {
                    return Vec::new();
                };
                let end = text.len().saturating_sub(delimiter.close().len() + 1);
                text.get(quote + open_len..end).unwrap_or_default().to_vec()
            }
            _ => text.to_vec(),
        }
    }
}

fn undouble(inner: &[u8], quote: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(inner.len());
    let mut bytes = inner.iter().copied().peekable();
    while let Some(b) = bytes.next() {
        out.push(b);
        if b == quote && bytes.peek() == Some(&quote) {
            bytes.next();
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub offset: usize,
    pub message: &'static str,
}

fn is_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

pub(crate) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'#') || b >= 0x80
}

const OPERATORS: [&[u8]; 9] = [
    b"||", b":=", b"=>", b"<=", b">=", b"<>", b"!=", b"^=", b"**",
];

pub fn lex(source: &[u8]) -> Result<Vec<Token>, LexError> {
    // About one token per four bytes in an exported INSERT.
    let mut tokens = Vec::with_capacity(source.len() / 4);
    let mut i = 0;
    while let Some(&b) = source.get(i) {
        let next = source.get(i + 1).copied();
        let start = i;
        let unterminated = |message| LexError {
            offset: start,
            message,
        };
        let kind = match (b, next) {
            _ if b.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            (b'-', Some(b'-')) => {
                i = source
                    .get(i..)
                    .and_then(|rest| rest.iter().position(|&c| c == b'\n'))
                    .map_or(source.len(), |p| i + p);
                continue;
            }
            (b'/', Some(b'*')) => {
                let rest = source.get(i + 2..).unwrap_or_default();
                let close = rest
                    .windows(2)
                    .position(|w| w == b"*/")
                    .ok_or_else(|| unterminated("unterminated comment"))?;
                i += 2 + close + 2;
                continue;
            }
            (b'\'', _) => {
                i = quoted(source, i, b'\'').ok_or_else(|| unterminated("unterminated string"))?;
                TokenKind::String { national: false }
            }
            (b'"', _) => {
                i = quoted(source, i, b'"')
                    .ok_or_else(|| unterminated("unterminated quoted identifier"))?;
                TokenKind::QuotedIdent
            }
            (b'n' | b'N', Some(b'\'')) => {
                i = quoted(source, i + 1, b'\'')
                    .ok_or_else(|| unterminated("unterminated string"))?;
                TokenKind::String { national: true }
            }
            (b'q' | b'Q', Some(b'\'')) => {
                i = alternative(source, i + 1)
                    .ok_or_else(|| unterminated("unterminated q-quoted string"))?;
                TokenKind::AltString { national: false }
            }
            (b'n' | b'N', Some(b'q' | b'Q')) if source.get(i + 2) == Some(&b'\'') => {
                i = alternative(source, i + 2)
                    .ok_or_else(|| unterminated("unterminated q-quoted string"))?;
                TokenKind::AltString { national: true }
            }
            _ if is_word_start(b) => {
                i += source
                    .get(i..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|&&c| is_word_byte(c))
                    .count();
                TokenKind::Word
            }
            _ if b.is_ascii_digit() || (b == b'.' && next.is_some_and(|c| c.is_ascii_digit())) => {
                i = number(source, i);
                TokenKind::Number
            }
            (b':', Some(c)) if is_word_byte(c) => {
                i += 1 + source
                    .get(i + 1..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|&&c| is_word_byte(c))
                    .count();
                TokenKind::Bind
            }
            _ if OPERATORS.iter().any(|op| source.get(i..i + 2) == Some(*op)) => {
                i += 2;
                TokenKind::Operator
            }
            _ => {
                i += 1;
                TokenKind::Punct
            }
        };
        tokens.push(Token {
            kind,
            start,
            end: i,
        });
    }
    Ok(tokens)
}

/// From an opening `quote` at `open`, the offset after its closing one; a doubled quote is part of the text.
fn quoted(source: &[u8], open: usize, quote: u8) -> Option<usize> {
    let mut i = open + 1;
    loop {
        match (source.get(i), source.get(i + 1)) {
            (None, _) => return None,
            (Some(&a), Some(&b)) if a == quote && b == quote => i += 2,
            (Some(&a), _) if a == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// The closing delimiter of a `q'…'` string, without its `'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Delimiter {
    bytes: [u8; 4],
    len: usize,
}

impl Delimiter {
    pub(crate) fn close(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }
}

/// The delimiter opening at `at`: a paired bracket closes with its partner, anything else with itself. A character
/// that is valid UTF-8 is one delimiter however many bytes it has; any other byte is one on its own, as in a
/// single-byte encoding. Blanks can't delimit.
fn delimiter_at(source: &[u8], at: usize) -> Option<Delimiter> {
    let &open = source.get(at)?;
    let single = |b: u8| Delimiter {
        bytes: [b, 0, 0, 0],
        len: 1,
    };
    Some(match open {
        b'[' => single(b']'),
        b'{' => single(b'}'),
        b'(' => single(b')'),
        b'<' => single(b'>'),
        _ if open.is_ascii_whitespace() => return None,
        _ if open < 0x80 => single(open),
        _ => {
            let width = match open {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => 1,
            };
            match source.get(at..at + width) {
                Some(char_bytes) if width > 1 && std::str::from_utf8(char_bytes).is_ok() => {
                    let mut bytes = [0; 4];
                    for (slot, &b) in bytes.iter_mut().zip(char_bytes) {
                        *slot = b;
                    }
                    Delimiter { bytes, len: width }
                }
                _ => single(open),
            }
        }
    })
}

/// For the `'` at `quote` in `content`, when it opens `q'…'` (or `nq'…'`): the delimiter, and how many bytes the `'`
/// and the opening delimiter take.
pub(crate) fn alternative_delimiter(content: &[u8], quote: usize) -> Option<(Delimiter, usize)> {
    let q = quote.checked_sub(1)?;
    if !matches!(content.get(q), Some(b'q' | b'Q')) {
        return None;
    }
    let starts_word =
        |at: Option<usize>| at.is_none_or(|i| content.get(i).is_none_or(|&b| !is_word_byte(b)));
    let before = q.checked_sub(1);
    let national = before.is_some_and(|n| matches!(content.get(n), Some(b'n' | b'N')))
        && starts_word(q.checked_sub(2));
    if !(starts_word(before) || national) {
        return None;
    }
    let delimiter = delimiter_at(content, quote + 1)?;
    let open_len = if content.get(quote + 1).is_some_and(|b| b"[{(<".contains(b)) {
        1
    } else {
        delimiter.len
    };
    Some((delimiter, 1 + open_len))
}

/// From the `'` of `q'`, the offset after the closing delimiter and `'`.
fn alternative(source: &[u8], quote: usize) -> Option<usize> {
    let delimiter = delimiter_at(source, quote + 1)?;
    let open_len = if source.get(quote + 1).is_some_and(|b| b"[{(<".contains(b)) {
        1
    } else {
        delimiter.len
    };
    let body_start = quote + 1 + open_len;
    let close = delimiter.close();
    let body = source.get(body_start..)?;
    let at = body
        .windows(close.len() + 1)
        .position(|w| w.starts_with(close) && w.last() == Some(&b'\''))?;
    Some(body_start + at + close.len() + 1)
}

/// Digits, an optional fraction, an optional exponent, and an optional `f`/`d` binary-float suffix.
fn number(source: &[u8], start: usize) -> usize {
    let digits = |from: usize| {
        from + source
            .get(from..)
            .unwrap_or_default()
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let mut i = digits(start);
    if source.get(i) == Some(&b'.') && source.get(i + 1) != Some(&b'.') {
        i = digits(i + 1);
    }
    if matches!(source.get(i), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(source.get(i + 1), Some(b'+' | b'-')));
        if source.get(i + 1 + sign).is_some_and(u8::is_ascii_digit) {
            i = digits(i + 1 + sign);
        }
    }
    if matches!(source.get(i), Some(b'f' | b'F' | b'd' | b'D'))
        && !source.get(i + 1).is_some_and(|&b| is_word_byte(b))
    {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(TokenKind, String)> {
        lex(source.as_bytes())
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t.kind,
                    String::from_utf8(t.text(source.as_bytes()).to_vec()).unwrap(),
                )
            })
            .collect()
    }

    use TokenKind::{AltString, Bind, Number, Operator, Punct, QuotedIdent, String as Str, Word};

    #[test]
    fn an_sql_developer_insert_lexes() {
        let source = "Insert into APP.\"AuditLog\" (LOG_ID,DETAIL) values ('1','a;b -- c''d');";
        assert_eq!(
            kinds(source),
            [
                (Word, "Insert".into()),
                (Word, "into".into()),
                (Word, "APP".into()),
                (Punct, ".".into()),
                (QuotedIdent, "\"AuditLog\"".into()),
                (Punct, "(".into()),
                (Word, "LOG_ID".into()),
                (Punct, ",".into()),
                (Word, "DETAIL".into()),
                (Punct, ")".into()),
                (Word, "values".into()),
                (Punct, "(".into()),
                (Str { national: false }, "'1'".into()),
                (Punct, ",".into()),
                (Str { national: false }, "'a;b -- c''d'".into()),
                (Punct, ")".into()),
                (Punct, ";".into()),
            ]
        );
    }

    #[test]
    fn strings_unquote_to_their_content() {
        let source = b"'it''s' N'\xc3\xa4' q'[a ' b]' nQ'!x!' \"A\"\"B\"";
        let tokens = lex(source).unwrap();
        let contents: Vec<Vec<u8>> = tokens.iter().map(|t| t.content(source)).collect();
        assert_eq!(
            contents,
            [
                b"it's".to_vec(),
                "ä".as_bytes().to_vec(),
                b"a ' b".to_vec(),
                b"x".to_vec(),
                b"A\"B".to_vec()
            ]
        );
        assert_eq!(tokens[2].kind, AltString { national: false });
        assert_eq!(tokens[3].kind, AltString { national: true });
    }

    #[test]
    fn a_multibyte_q_delimiter_closes_and_unquotes() {
        let source = "q'§a'b§' x".as_bytes();
        let tokens = lex(source).unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].content(source), b"a'b");
        // In a single-byte encoding a byte above 0x7F is one character on its own.
        let latin1 = b"q'\xa7x\xa7'";
        assert_eq!(lex(latin1).unwrap()[0].content(latin1), b"x");
    }

    #[test]
    fn numbers_binds_operators_and_comments() {
        assert_eq!(
            kinds("1 1.5 .5 1.0E3 2e-1 3f 10E0 :x :1 a||b := => <> != -- c\n/*+ hint */ x"),
            [
                (Number, "1".into()),
                (Number, "1.5".into()),
                (Number, ".5".into()),
                (Number, "1.0E3".into()),
                (Number, "2e-1".into()),
                (Number, "3f".into()),
                (Number, "10E0".into()),
                (Bind, ":x".into()),
                (Bind, ":1".into()),
                (Word, "a".into()),
                (Operator, "||".into()),
                (Word, "b".into()),
                (Operator, ":=".into()),
                (Operator, "=>".into()),
                (Operator, "<>".into()),
                (Operator, "!=".into()),
                (Word, "x".into()),
            ]
        );
        assert_eq!(kinds("x-1")[1], (Punct, "-".into()));
    }

    #[test]
    fn unterminated_text_is_an_error_at_its_start() {
        assert_eq!(
            lex(b"x 'open").unwrap_err(),
            LexError {
                offset: 2,
                message: "unterminated string"
            }
        );
        assert_eq!(lex(b"/* open").unwrap_err().offset, 0);
        assert_eq!(
            lex(b"q'[x]").unwrap_err().message,
            "unterminated q-quoted string"
        );
    }
}

#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::lex;

    proptest! {
        #[test]
        fn tokens_are_ordered_and_never_overlap(input in prop::collection::vec(any::<u8>(), 0..300)) {
            if let Ok(tokens) = lex(&input) {
                let mut last_end = 0;
                for token in tokens {
                    prop_assert!(last_end <= token.start && token.start < token.end && token.end <= input.len());
                    last_end = token.end;
                }
            }
        }
    }
}
