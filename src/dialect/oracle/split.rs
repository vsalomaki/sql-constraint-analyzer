//! Splits an Oracle script into pieces the way its runner does, reading one line at a time and holding only the
//! current piece.
//!
//! It works on bytes. Every byte that matters to the split (quotes, `;`, `/`, `-`, `*`, line ends) is ASCII, and an
//! ASCII-compatible encoding (UTF-8, Windows-1252, ISO 8859) never uses those bytes inside another character, so
//! no decoding is needed and offsets are offsets in the file as stored. UTF-16 is not ASCII-compatible.
//!
//! The two runners, checked against both in Oracle Free:
//! - [`Runner::SqlPlus`] ends a statement at a line whose last non-blank character is `;`, even inside a string or
//!   comment. A `;` before a trailing comment or another statement on the same line doesn't end it.
//! - [`Runner::Sqlcl`] (SQLcl and SQL Developer) ends a statement at any `;` outside strings and comments, so one line
//!   can hold several.
//! - In both, a line holding only `/` ends the statement or block in progress (SQL\*Plus even inside a string), and
//!   with nothing in progress runs the previous statement again. A block (`DECLARE`, `BEGIN`, `CREATE …
//!   PROCEDURE|FUNCTION|PACKAGE|TRIGGER|TYPE|LIBRARY|JAVA`) ends only at a `/` line. A script command is recognised by
//!   its first word where nothing is in progress, and continues onto the next line when it ends with `-`.
//! - Blank lines inside a statement are kept as whitespace. SQL\*Plus by default drops such a statement unrun
//!   (`SET SQLBLANKLINES OFF`); exports don't have them.

use std::collections::VecDeque;
use std::io::{self, BufRead, Read};

use super::lex::{Delimiter, alternative_delimiter, is_word_byte, same_ignoring_case};
use crate::source::{Piece, PieceKind, Span, to_u64};

pub use crate::dialect::Runner;

/// Longest line or piece read before giving up: a file with no terminator would otherwise be read whole into memory.
pub const MAX_PIECE_BYTES: usize = 256 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    Single,
    Alternative(Delimiter),
    Double,
}

#[derive(Debug)]
struct Current {
    kind: PieceKind,
    /// Decided at the first possible end, by which point the first words are in.
    block: Option<bool>,
    start: u64,
    line: u64,
    text: Vec<u8>,
}

impl Current {
    fn is_block(&mut self, more: &[u8]) -> bool {
        let text = &self.text;
        *self.block.get_or_insert_with(|| {
            let kind = if text.is_empty() {
                head_kind(more)
            } else {
                head_kind(&[text.as_slice(), more].concat())
            };
            kind == PieceKind::Block
        })
    }
}

/// One runner's split state, fed one line at a time.
#[derive(Debug)]
struct Machine {
    runner: Runner,
    current: Option<Current>,
    quote: Option<Quote>,
    in_block_comment: bool,
    ended: VecDeque<Piece>,
    /// Offset and number of the line being taken.
    offset: u64,
    line: u64,
    /// Whether a SQL*Plus line that is a whole statement is taken without following its quotes and comments; tests
    /// turn it off to compare.
    whole_lines: bool,
}

impl Machine {
    fn new(runner: Runner) -> Self {
        Self {
            runner,
            current: None,
            quote: None,
            in_block_comment: false,
            ended: VecDeque::new(),
            offset: 0,
            line: 1,
            whole_lines: true,
        }
    }

    fn start(&mut self, kind: PieceKind, at: usize) {
        self.current = Some(Current {
            kind,
            block: None,
            start: self.offset + to_u64(at),
            line: self.line,
            text: Vec::new(),
        });
    }

    fn append(&mut self, bytes: &[u8]) {
        if let Some(current) = &mut self.current {
            current.text.extend_from_slice(bytes);
        }
    }

    /// Ends the current piece at `end`, as a block or statement by its first words.
    fn end_current(&mut self, end: u64) {
        if let Some(mut current) = self.current.take() {
            let kind = if current.kind == PieceKind::Command {
                PieceKind::Command
            } else if current.is_block(b"") {
                PieceKind::Block
            } else {
                PieceKind::Sql
            };
            self.ended.push_back(Piece {
                kind,
                span: Span {
                    start: current.start,
                    end,
                },
                line: current.line,
                text: current.text,
            });
        }
    }

    fn end_of_input(&mut self, end: u64) {
        if let Some(current) = self.current.take() {
            let kind = if current.kind == PieceKind::Command {
                PieceKind::Command
            } else {
                PieceKind::Unterminated
            };
            self.ended.push_back(Piece {
                kind,
                span: Span {
                    start: current.start,
                    end,
                },
                line: current.line,
                text: current.text,
            });
        }
    }

    fn take_line(&mut self, line: &[u8], offset: u64, number: u64) {
        self.offset = offset;
        self.line = number;
        let content = strip_eol(line);
        let eol = line.get(content.len()..).unwrap_or_default();
        if self
            .current
            .as_ref()
            .is_some_and(|c| c.kind == PieceKind::Command)
        {
            self.append(content);
            if continues(content) {
                self.append(eol);
            } else {
                self.end_current(offset + to_u64(content.len()));
            }
            return;
        }

        let trimmed = content.trim_ascii();
        let inside = self.quote.is_some() || self.in_block_comment;
        let indent = leading_blanks(content);
        // SQL*Plus takes a `/` line as one even inside a string or comment; SQLcl only outside them.
        if trimmed == b"/"
            && (!inside || (self.runner == Runner::SqlPlus && self.current.is_some()))
        {
            let slash = offset + to_u64(indent);
            if let Some(current) = &mut self.current {
                trim_trailing_blanks(&mut current.text);
                self.end_current(slash + 1);
            } else {
                self.ended.push_back(Piece {
                    kind: PieceKind::RunAgain,
                    span: Span {
                        start: slash,
                        end: slash + 1,
                    },
                    line: number,
                    text: b"/".to_vec(),
                });
            }
            self.quote = None;
            self.in_block_comment = false;
            return;
        }
        if !inside && self.current.is_none() && is_command(trimmed) {
            self.start(PieceKind::Command, indent);
            self.append(content.get(indent..).unwrap_or_default());
            if continues(content) {
                self.append(eol);
            } else {
                self.end_current(offset + to_u64(content.len()));
            }
            return;
        }
        if !inside && self.whole_statement(content, indent) {
            return;
        }
        self.scan(line, content);
    }

    /// Takes a line that SQL*Plus runs as one statement: it starts one, with a word, and its last non-blank byte is
    /// `;`, which ends it whatever quote or comment comes before. Exports write a row per line this way. A block
    /// ends only at `/`, so its lines are scanned.
    fn whole_statement(&mut self, content: &[u8], indent: usize) -> bool {
        if !self.whole_lines || self.runner != Runner::SqlPlus || self.current.is_some() {
            return false;
        }
        let Some(at) = content
            .trim_ascii_end()
            .len()
            .checked_sub(1)
            .filter(|&i| content.get(i) == Some(&b';'))
        else {
            return false;
        };
        let text = content.get(indent..at).unwrap_or_default();
        if !text.first().is_some_and(u8::is_ascii_alphabetic) || head_kind(text) == PieceKind::Block
        {
            return false;
        }
        self.start(PieceKind::Sql, indent);
        if let Some(current) = &mut self.current {
            current.block = Some(false);
            current.text = text.to_vec();
        }
        self.end_current(self.offset + to_u64(at) + 1);
        true
    }

    /// Scans a line of SQL, ending statements where the runner does.
    fn scan(&mut self, line: &[u8], content: &[u8]) {
        let last_semicolon = content
            .trim_ascii_end()
            .len()
            .checked_sub(1)
            .filter(|&i| content.get(i) == Some(&b';'));
        // Where this line's part of the current piece starts.
        let mut from = self.current.as_ref().map(|_| 0);
        let mut i = 0;
        while let Some(&b) = content.get(i) {
            let next = content.get(i + 1).copied();
            if self.in_block_comment {
                if b == b'*' && next == Some(b'/') {
                    self.in_block_comment = false;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if let Some(quote) = self.quote {
                i += self.inside_quote(quote, content, i);
                continue;
            }
            // Inside a piece only these bytes can change the state.
            if self.current.is_some() && !matches!(b, b'-' | b'/' | b'\'' | b'"' | b';') {
                let rest = content.get(i..).unwrap_or_default();
                i += rest
                    .iter()
                    .position(|&b| matches!(b, b'-' | b'/' | b'\'' | b'"' | b';'))
                    .unwrap_or(rest.len());
                continue;
            }
            match (b, next) {
                (b'-', Some(b'-')) => break,
                (b'/', Some(b'*')) => {
                    self.in_block_comment = true;
                    i += 2;
                    continue;
                }
                _ if b.is_ascii_whitespace() => {}
                _ => {
                    if self.current.is_none() {
                        self.start(PieceKind::Sql, i);
                        from = Some(i);
                    }
                    match b {
                        b'\'' => match alternative_delimiter(content, i) {
                            Some((delimiter, open_len)) => {
                                self.quote = Some(Quote::Alternative(delimiter));
                                i += open_len;
                                continue;
                            }
                            None => self.quote = Some(Quote::Single),
                        },
                        b'"' => self.quote = Some(Quote::Double),
                        b';' if self.semicolon(
                            content,
                            from.unwrap_or(0),
                            i,
                            last_semicolon == Some(i),
                        ) =>
                        {
                            from = None;
                        }
                        _ => {}
                    }
                }
            }
            i += 1;
        }
        if let Some(from) = from {
            self.append(line.get(from..).unwrap_or_default());
        }
        // A line-ending `;` inside a string or comment: SQL*Plus ends the statement there anyway, SQLcl doesn't.
        let Some(at) = last_semicolon else { return };
        let open_sql = self
            .current
            .as_mut()
            .is_some_and(|c| c.kind == PieceKind::Sql && !c.is_block(b""));
        if self.runner == Runner::SqlPlus && open_sql {
            if let Some(current) = &mut self.current
                && let Some(cut) = current.text.iter().rposition(|&b| b == b';')
            {
                current.text.truncate(cut);
            }
            self.quote = None;
            self.in_block_comment = false;
            self.end_current(self.offset + to_u64(at) + 1);
        }
    }

    /// Bytes to step past inside a quote at `i`, closing the quote when it ends there.
    fn inside_quote(&mut self, quote: Quote, content: &[u8], i: usize) -> usize {
        let at = content.get(i..).unwrap_or_default();
        match quote {
            Quote::Single | Quote::Double => {
                let mark = if quote == Quote::Single { b'\'' } else { b'"' };
                match at.iter().position(|&b| b == mark) {
                    None => at.len(),
                    Some(p) if at.get(p + 1) == Some(&mark) => p + 2,
                    Some(p) => {
                        self.quote = None;
                        p + 1
                    }
                }
            }
            Quote::Alternative(delimiter)
                if at.starts_with(delimiter.close())
                    && at.get(delimiter.close().len()) == Some(&b'\'') =>
            {
                self.quote = None;
                delimiter.close().len() + 1
            }
            Quote::Alternative(_) => 1,
        }
    }

    /// A `;` outside strings and comments at `at`; returns whether it ended the piece.
    fn semicolon(&mut self, content: &[u8], from: usize, at: usize, ends_line: bool) -> bool {
        let Some(current) = &mut self.current else {
            return false;
        };
        if current.kind != PieceKind::Sql
            || current.is_block(content.get(from..at).unwrap_or_default())
        {
            return false;
        }
        // SQL*Plus doesn't end a statement at a `;` with more on its line.
        if self.runner == Runner::SqlPlus && !ends_line {
            return false;
        }
        self.append(content.get(from..at).unwrap_or_default());
        self.end_current(self.offset + to_u64(at) + 1);
        true
    }
}

pub struct Splitter<R> {
    reader: R,
    limit: usize,
    buffer: Vec<u8>,
    /// Offset of the next line's first byte, and its number from 1.
    offset: u64,
    line: u64,
    machine: Machine,
    ready: VecDeque<Piece>,
    finished: bool,
}

impl<R: BufRead> Splitter<R> {
    pub fn new(reader: R, runner: Runner) -> Self {
        Self::with_limit(reader, runner, MAX_PIECE_BYTES)
    }

    /// A splitter that follows every byte of every line, to check the whole-line shortcut against.
    #[cfg(test)]
    fn following_every_byte(mut self) -> Self {
        self.machine.whole_lines = false;
        self
    }

    /// A splitter that gives up on a line or piece longer than `limit` bytes.
    pub fn with_limit(reader: R, runner: Runner, limit: usize) -> Self {
        Self {
            reader,
            limit,
            buffer: Vec::new(),
            offset: 0,
            line: 1,
            machine: Machine::new(runner),
            ready: VecDeque::new(),
            finished: false,
        }
    }

    /// Treats the reader as starting `offset` bytes into the file, on line `line`, so pieces carry offsets and lines
    /// in the file as stored (the input layer skips a byte-order mark, or seeks to where an include command ends,
    /// before handing the reader over).
    #[must_use]
    pub fn starting_at(mut self, offset: u64, line: u64) -> Self {
        self.offset = offset;
        self.line = line;
        self
    }

    fn too_long(&self, what: &str, line: u64) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the {what} at line {line} is longer than {} bytes",
                self.limit
            ),
        )
    }

    /// Reads one line, with its line end, holding no more of it than one byte past the limit.
    fn read_bounded_line(&mut self) -> io::Result<()> {
        self.buffer.clear();
        let most = self.limit.saturating_add(2);
        (&mut self.reader)
            .take(u64::try_from(most.saturating_add(1)).unwrap_or(u64::MAX))
            .read_until(b'\n', &mut self.buffer)?;
        if self.buffer.len() > most {
            return Err(self.too_long("line", self.line));
        }
        Ok(())
    }

    fn read_line(&mut self) -> io::Result<()> {
        self.read_bounded_line()?;
        if self.buffer.is_empty() {
            self.finished = true;
            self.machine.end_of_input(self.offset);
            self.ready.extend(self.machine.ended.drain(..));
            return Ok(());
        }
        let line = std::mem::take(&mut self.buffer);
        self.machine.take_line(&line, self.offset, self.line);
        let pending_too_long = self
            .machine
            .current
            .as_ref()
            .filter(|c| c.text.len() > self.limit)
            .map(|c| c.line);
        let ended_too_long = self
            .machine
            .ended
            .iter()
            .find(|p| p.text.len() > self.limit)
            .map(|p| p.line);
        if let Some(start) = ended_too_long.or(pending_too_long) {
            return Err(self.too_long("statement starting", start));
        }
        self.ready.extend(self.machine.ended.drain(..));
        self.offset += to_u64(line.len());
        self.line += 1;
        self.buffer = line;
        Ok(())
    }
}

impl<R: BufRead> Iterator for Splitter<R> {
    type Item = io::Result<Piece>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(piece) = self.ready.pop_front() {
                return Some(Ok(piece));
            }
            if self.finished {
                return None;
            }
            if let Err(error) = self.read_line() {
                self.finished = true;
                return Some(Err(error));
            }
        }
    }
}

fn strip_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn leading_blanks(content: &[u8]) -> usize {
    content
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count()
}

fn trim_trailing_blanks(text: &mut Vec<u8>) {
    while text.last().is_some_and(u8::is_ascii_whitespace) {
        text.pop();
    }
}

/// A script command continues onto the next line when its line ends with `-`.
fn continues(content: &[u8]) -> bool {
    content.trim_ascii_end().ends_with(b"-")
}

/// Whether `rest` is blank up to a line end that it has.
fn ends_line_blank(rest: &[u8]) -> bool {
    rest.iter()
        .position(|&b| b == b'\n')
        .is_some_and(|eol| rest.iter().take(eol).all(u8::is_ascii_whitespace))
}

/// The leading words as written, past blanks and comments.
fn head_words(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut i = 0;
    std::iter::from_fn(move || {
        loop {
            match (text.get(i), text.get(i + 1)) {
                (Some(b), _) if b.is_ascii_whitespace() => i += 1,
                (Some(b'-'), Some(b'-')) => {
                    i += text
                        .get(i..)
                        .and_then(|rest| rest.iter().position(|&b| b == b'\n'))
                        .unwrap_or(text.len());
                }
                (Some(b'/'), Some(b'*')) => {
                    let rest = text.get(i + 2..).unwrap_or_default();
                    i += 2 + rest
                        .windows(2)
                        .position(|w| w == b"*/")
                        .map_or(rest.len(), |p| p + 2);
                }
                // SQL*Plus joins a line ending in `-` to the next before it reads the first words.
                (Some(b'-'), _) if ends_line_blank(text.get(i + 1..).unwrap_or_default()) => i += 1,
                (Some(&b), _) if is_word_byte(b) => {
                    let rest = text.get(i..).unwrap_or_default();
                    let len = rest.iter().take_while(|&&b| is_word_byte(b)).count();
                    i += len;
                    return rest.get(..len);
                }
                _ => return None,
            }
        }
    })
}

fn is(word: &[u8], keyword: &str) -> bool {
    same_ignoring_case(word, keyword.as_bytes())
}

/// Whether a piece is a block, from its first words.
fn head_kind(text: &[u8]) -> PieceKind {
    let mut words = head_words(text);
    let block = match words.next() {
        Some(w) if is(w, "DECLARE") || is(w, "BEGIN") => true,
        Some(w) if is(w, "CREATE") => {
            let mut word = words.next();
            if word.is_some_and(|w| is(w, "OR")) {
                word = words
                    .next()
                    .filter(|w| is(w, "REPLACE"))
                    .and_then(|_| words.next());
            }
            if word.is_some_and(|w| is(w, "EDITIONABLE") || is(w, "NONEDITIONABLE")) {
                word = words.next();
            }
            word.is_some_and(|w| {
                [
                    "PROCEDURE",
                    "FUNCTION",
                    "PACKAGE",
                    "TRIGGER",
                    "TYPE",
                    "LIBRARY",
                    "JAVA",
                ]
                .iter()
                .any(|k| is(w, k))
            })
        }
        _ => false,
    };
    if block {
        PieceKind::Block
    } else {
        PieceKind::Sql
    }
}

/// SQL\*Plus commands as (shortest accepted form, full name); every prefix of the full name at least as long as the
/// shortest form is accepted, as SQL\*Plus documents `PRO[MPT]`.
const COMMANDS: &[(&str, &str)] = &[
    ("ACC", "ACCEPT"),
    ("A", "APPEND"),
    ("ARCHIVE", "ARCHIVE"),
    ("ATTR", "ATTRIBUTE"),
    ("BRE", "BREAK"),
    ("BTI", "BTITLE"),
    ("C", "CHANGE"),
    ("CL", "CLEAR"),
    ("COL", "COLUMN"),
    ("COMP", "COMPUTE"),
    ("CONN", "CONNECT"),
    ("COPY", "COPY"),
    ("DEF", "DEFINE"),
    ("DEL", "DEL"),
    ("DESC", "DESCRIBE"),
    ("DISC", "DISCONNECT"),
    ("ED", "EDIT"),
    ("EXEC", "EXECUTE"),
    ("EXIT", "EXIT"),
    ("GET", "GET"),
    ("HELP", "HELP"),
    ("HIST", "HISTORY"),
    ("HO", "HOST"),
    ("I", "INPUT"),
    ("L", "LIST"),
    ("PASSW", "PASSWORD"),
    ("PAU", "PAUSE"),
    ("PRINT", "PRINT"),
    ("PRO", "PROMPT"),
    ("QUIT", "QUIT"),
    ("R", "RUN"),
    ("RECOVER", "RECOVER"),
    ("REM", "REMARK"),
    ("REPF", "REPFOOTER"),
    ("REPH", "REPHEADER"),
    ("SAV", "SAVE"),
    ("SET", "SET"),
    ("SHO", "SHOW"),
    ("SHUTDOWN", "SHUTDOWN"),
    ("SPO", "SPOOL"),
    ("STA", "START"),
    ("STARTUP", "STARTUP"),
    ("STORE", "STORE"),
    ("TIMI", "TIMING"),
    ("TTI", "TTITLE"),
    ("UNDEF", "UNDEFINE"),
    ("VAR", "VARIABLE"),
    ("WHENEVER", "WHENEVER"),
    ("XQUERY", "XQUERY"),
];

/// By initial letter, the range of `COMMANDS` that starts with it; every line asks.
#[expect(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    reason = "built at compile time over a list of fewer than 256 commands in order, each with an initial A to Z"
)]
const COMMAND_RANGES: [(u8, u8); 26] = {
    let mut ranges = [(0, 0); 26];
    let mut i = 0;
    while i < COMMANDS.len() {
        let letter = (COMMANDS[i].1.as_bytes()[0] - b'A') as usize;
        if ranges[letter].1 == 0 {
            ranges[letter].0 = i as u8;
        }
        ranges[letter].1 = i as u8 + 1;
        i += 1;
    }
    ranges
};

/// A script command, from the first word of a line with nothing in progress.
fn is_command(trimmed: &[u8]) -> bool {
    if trimmed.starts_with(b"@") || trimmed.starts_with(b"!") {
        return true;
    }
    let mut words = head_words(trimmed);
    let Some(first) = words.next() else {
        return false;
    };
    // `SET TRANSACTION`, `SET ROLE` and `SET CONSTRAINT[S]` are SQL.
    if is(first, "SET")
        && words.next().is_some_and(|w| {
            ["TRANSACTION", "ROLE", "CONSTRAINT", "CONSTRAINTS"]
                .iter()
                .any(|k| is(w, k))
        })
    {
        return false;
    }
    command(first).is_some()
}

/// The full name of the SQL\*Plus command `word` names, in any case and any accepted abbreviation.
pub(crate) fn command(word: &[u8]) -> Option<&'static str> {
    let initial = word.first()?.to_ascii_uppercase();
    let &(from, to) = COMMAND_RANGES.get(usize::from(initial.checked_sub(b'A')?))?;
    let same = COMMANDS
        .get(usize::from(from)..usize::from(to))
        .unwrap_or_default();
    same.iter().find_map(|&(shortest, full)| {
        (word.len() >= shortest.len()
            && full
                .as_bytes()
                .get(..word.len())
                .is_some_and(|prefix| same_ignoring_case(prefix, word)))
        .then_some(full)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_in_order_for_the_lookup_by_initial() {
        let initials: Vec<u8> = COMMANDS
            .iter()
            .map(|(_, full)| full.as_bytes()[0])
            .collect();
        assert!(initials.is_sorted(), "{initials:?}");
    }
    use PieceKind::{Block, Command, RunAgain, Sql, Unterminated};

    fn split_with(runner: Runner, text: &str) -> Vec<(PieceKind, u64, String)> {
        Splitter::new(text.as_bytes(), runner)
            .map(|p| {
                let p = p.unwrap();
                (p.kind, p.line, String::from_utf8(p.text).unwrap())
            })
            .collect()
    }

    fn split(text: &str) -> Vec<(PieceKind, u64, String)> {
        split_with(Runner::SqlPlus, text)
    }

    #[test]
    fn offsets_and_lines_start_where_the_reader_was_positioned() {
        let pieces: Vec<_> = Splitter::new("select 1;\n".as_bytes(), Runner::SqlPlus)
            .starting_at(3, 1)
            .map(|p| p.unwrap().span)
            .collect();
        assert_eq!(pieces, [Span { start: 3, end: 12 }]);
        // The rest of an include command's line, then the next statement.
        let pieces: Vec<_> = Splitter::new("\r\nselect 1;\n".as_bytes(), Runner::SqlPlus)
            .starting_at(40, 4)
            .map(|p| p.unwrap())
            .map(|p| (p.span, p.line))
            .collect();
        assert_eq!(pieces, [(Span { start: 42, end: 51 }, 5)]);
    }

    #[test]
    fn sql_plus_ends_a_statement_only_at_a_line_ending_semicolon() {
        assert_eq!(
            split(
                "insert into t values (1);\ninsert into t values (2); -- note\nselect 1 from dual;\nselect 2 from dual; select 3 from dual;\n"
            ),
            [
                (Sql, 1, "insert into t values (1)".into()),
                (
                    Sql,
                    2,
                    "insert into t values (2); -- note\nselect 1 from dual".into()
                ),
                (Sql, 4, "select 2 from dual; select 3 from dual".into()),
            ]
        );
    }

    #[test]
    fn sqlcl_ends_a_statement_at_any_semicolon_outside_strings_and_comments() {
        assert_eq!(
            split_with(
                Runner::Sqlcl,
                "insert into t values (1); -- note\nselect 2 from dual; select 3 from dual;\n"
            ),
            [
                (Sql, 1, "insert into t values (1)".into()),
                (Sql, 2, "select 2 from dual".into()),
                (Sql, 2, "select 3 from dual".into()),
            ]
        );
    }

    #[test]
    fn a_line_ending_semicolon_inside_a_string_or_comment_splits_only_in_sql_plus() {
        let text = "insert into t values ('a;\nb');\ninsert into t values (1)\n-- finish;\ninsert into t values (2);\n";
        assert_eq!(
            split(text),
            [
                (Sql, 1, "insert into t values ('a".into()),
                (Sql, 2, "b')".into()),
                (Sql, 3, "insert into t values (1)\n-- finish".into()),
                (Sql, 5, "insert into t values (2)".into()),
            ]
        );
        assert_eq!(
            split_with(Runner::Sqlcl, text),
            [
                (Sql, 1, "insert into t values ('a;\nb')".into()),
                (
                    Sql,
                    3,
                    "insert into t values (1)\n-- finish;\ninsert into t values (2)".into()
                ),
            ]
        );
    }

    #[test]
    fn a_slash_line_ends_a_block_or_a_statement_or_runs_the_last_again() {
        let text = "BEGIN\n  NULL;\nEND;\n/\nselect 1 from dual\n/\ninsert into t values (1);\n/\n";
        for runner in [Runner::SqlPlus, Runner::Sqlcl] {
            assert_eq!(
                split_with(runner, text),
                [
                    (Block, 1, "BEGIN\n  NULL;\nEND;".into()),
                    (Sql, 5, "select 1 from dual".into()),
                    (Sql, 7, "insert into t values (1)".into()),
                    (RunAgain, 8, "/".into()),
                ]
            );
        }
        let in_string = "insert into t values ('x\n/\ny');\n";
        assert_eq!(
            split(in_string).iter().map(|p| p.1).collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(
            split_with(Runner::Sqlcl, in_string),
            [(Sql, 1, "insert into t values ('x\n/\ny')".into())]
        );
    }

    #[test]
    fn create_or_replace_trigger_over_several_lines_is_a_block() {
        let text = "  CREATE OR REPLACE EDITIONABLE\n  TRIGGER \"APP\".\"T\" BEFORE INSERT ON t FOR EACH ROW\nBEGIN\n  NULL;\nEND;\n/\nALTER TRIGGER \"APP\".\"T\" ENABLE;\n";
        let pieces = split(text);
        assert_eq!(
            pieces.iter().map(|p| (p.0, p.1)).collect::<Vec<_>>(),
            [(Block, 1), (Sql, 7)]
        );
        assert!(pieces[0].2.ends_with("END;"));
        assert_eq!(
            split("create type t as object (a number);\n/\n")[0].0,
            Block
        );
        assert_eq!(
            split("create or replace view v as select 1 x from dual;\n")[0].0,
            Sql
        );
    }

    #[test]
    fn quotes_and_comments_mid_line_hide_terminators() {
        let text = "insert into t values ('a; b', q'[x; ]', \"C;\", 'it''s');\n/* ; */\n/* x\n*/ select 1 from dual;\n";
        for runner in [Runner::SqlPlus, Runner::Sqlcl] {
            let pieces = split_with(runner, text);
            assert_eq!(
                pieces.iter().map(|p| p.1).collect::<Vec<_>>(),
                [1, 4],
                "{runner:?}"
            );
            assert_eq!(pieces[1].2, "select 1 from dual");
        }
        assert_eq!(
            split("select q'{a'}b}' from dual;\n")[0].2,
            "select q'{a'}b}' from dual"
        );
        assert_eq!(split("select nq'!x!' from dual;\n").len(), 1);
    }

    #[test]
    fn a_multibyte_q_delimiter_closes() {
        let text = "insert into t values (q'§a; b§');\ninsert into t values (2);\n";
        for runner in [Runner::SqlPlus, Runner::Sqlcl] {
            assert_eq!(
                split_with(runner, text)
                    .iter()
                    .map(|p| (p.0, p.1))
                    .collect::<Vec<_>>(),
                [(Sql, 1), (Sql, 2)]
            );
        }
    }

    #[test]
    fn script_commands_are_whole_lines_with_abbreviations_and_continuation() {
        let text = "REM INSERTING into APP.T\nSET DEFINE OFF;\n@@app/x.sql\nPROM one -\ntwo\nset transaction read only;\nPROMPTS x;\n";
        assert_eq!(
            split(text),
            [
                (Command, 1, "REM INSERTING into APP.T".into()),
                (Command, 2, "SET DEFINE OFF;".into()),
                (Command, 3, "@@app/x.sql".into()),
                (Command, 4, "PROM one -\ntwo".into()),
                (Sql, 6, "set transaction read only".into()),
                (Sql, 7, "PROMPTS x".into()),
            ]
        );
        assert!(
            is_command(b"spo off")
                && is_command(b"WHENEVER SQLERROR EXIT")
                && !is_command(b"DELETE FROM t")
        );
        assert!(!is_command(b"SAVEPOINT a") && !is_command(b"COMMIT") && !is_command(b"PR x"));
    }

    #[test]
    fn an_open_statement_at_the_end_is_unterminated() {
        assert_eq!(
            split_with(Runner::Sqlcl, "insert into t values ('open\n"),
            [(Unterminated, 1, "insert into t values ('open\n".into())]
        );
        assert_eq!(
            split("select 1 from dual"),
            [(Unterminated, 1, "select 1 from dual".into())]
        );
    }

    #[test]
    fn spans_and_lines_follow_the_bytes_as_stored() {
        let text = "REM x\r\n\r\n  insert into t values ('ä');\r\nBEGIN NULL; END;\r\n/\r\n";
        let pieces: Vec<Piece> = Splitter::new(text.as_bytes(), Runner::SqlPlus)
            .map(Result::unwrap)
            .collect();
        let spans: Vec<(u64, u64, u64)> = pieces
            .iter()
            .map(|p| (p.line, p.span.start, p.span.end))
            .collect();
        let at = |s: &str| u64::try_from(text.find(s).unwrap()).unwrap();
        assert_eq!(
            spans,
            [
                (1, 0, 5),
                (3, at("insert"), at("');") + 3),
                (4, at("BEGIN"), at("/\r\n") + 1)
            ]
        );
        assert_eq!(pieces[1].text, "insert into t values ('ä')".as_bytes());
    }
}

#[cfg(test)]
mod properties {
    use proptest::prelude::*;

    use super::{Runner, Splitter};
    use crate::source::PieceKind;

    /// Bytes that matter to the split, often, and anything else now and then.
    fn script() -> impl Strategy<Value = Vec<u8>> {
        let interesting = prop::sample::select(b"';/\n\r-*\"qQnN []{}()<>!@ BEGIN REM x1".to_vec());
        prop::collection::vec(prop_oneof![8 => interesting, 1 => any::<u8>()], 0..400)
    }

    proptest! {
        #[test]
        fn pieces_sit_in_order_where_their_bytes_are(input in script(), sqlcl in any::<bool>()) {
            let runner = if sqlcl { Runner::Sqlcl } else { Runner::SqlPlus };
            let mut last_end = 0;
            for piece in Splitter::new(input.as_slice(), runner) {
                let piece = piece.unwrap();
                let (start, end) = (usize::try_from(piece.span.start).unwrap(), usize::try_from(piece.span.end).unwrap());
                prop_assert!(last_end <= start && start <= end && end <= input.len(), "{:?}", piece.span);
                prop_assert!(input[start..].starts_with(&piece.text));
                let lines_before = input[..start].split(|&b| b == b'\n').count() - 1;
                prop_assert_eq!(piece.line, u64::try_from(lines_before + 1).unwrap());
                // The fix pass cuts at `end`: right after the terminator, or at the end of the command's line.
                match piece.kind {
                    PieceKind::Sql | PieceKind::Block => prop_assert!(matches!(input[end - 1], b';' | b'/')),
                    PieceKind::RunAgain => prop_assert_eq!(input[end - 1], b'/'),
                    PieceKind::Command => prop_assert!(end == input.len() || matches!(input[end], b'\n' | b'\r')),
                    PieceKind::Unterminated => prop_assert_eq!(end, input.len()),
                }
                last_end = end;
            }
        }

        #[test]
        fn whole_lines_split_as_following_every_byte_does(input in script(), lines in prop::collection::vec(row_line(), 0..8)) {
            // Exported rows, mixed with anything.
            let mut input = input;
            for line in lines {
                input.extend_from_slice(&line);
            }
            let split = |every_byte: bool| -> Vec<String> {
                let splitter = Splitter::new(input.as_slice(), Runner::SqlPlus);
                let splitter = if every_byte { splitter.following_every_byte() } else { splitter };
                splitter
                    .map(|piece| match piece {
                        Ok(piece) => format!("{piece:?}"),
                        Err(error) => format!("error: {error}"),
                    })
                    .collect()
            };
            prop_assert_eq!(split(false), split(true));
        }
    }

    /// A line like an exported row: a word first, `;` last, and quotes, comments and `;` anywhere between.
    fn row_line() -> impl Strategy<Value = Vec<u8>> {
        let middle = prop::sample::select(b"';/-*\"qQ[] (),aN1".to_vec());
        (
            prop::sample::select(vec![
                &b"Insert"[..],
                b"  insert",
                b"x",
                b"BEGIN",
                b"create procedure",
                b"REM",
                b"/",
            ]),
            prop::collection::vec(middle, 0..60),
            prop::sample::select(vec![&b";\n"[..], b";\r\n", b"; \n", b";", b"\n"]),
        )
            .prop_map(|(head, middle, end)| [head, &middle, end].concat())
    }
}

#[cfg(test)]
mod limit {
    use super::{Runner, Splitter};

    #[test]
    fn a_piece_with_no_end_is_given_up_on_at_the_limit() {
        let text = "insert into t values ('open\n".repeat(10);
        let mut pieces = Splitter::with_limit(text.as_bytes(), Runner::SqlPlus, 100);
        let error = pieces.find_map(Result::err).unwrap();
        assert!(
            error
                .to_string()
                .contains("statement starting at line 1 is longer than 100 bytes"),
            "{error}"
        );
        assert!(pieces.next().is_none());
    }
}

#[cfg(test)]
mod review {
    use super::{Runner, Splitter};

    #[test]
    fn a_long_line_is_refused_before_it_is_read_whole() {
        let error = Splitter::with_limit("select 123456789012345;\n".as_bytes(), Runner::Sqlcl, 10)
            .find_map(Result::err)
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("line at line 1 is longer than 10 bytes"),
            "{error}"
        );
        let no_newline = "x".repeat(50);
        assert!(
            Splitter::with_limit(no_newline.as_bytes(), Runner::SqlPlus, 10).any(|p| p.is_err())
        );
    }

    #[test]
    fn each_runner_splits_a_string_with_line_ending_semicolons_its_own_way() {
        let text = "insert into t values (1); insert into t values ('x;\ny');\n";
        let lines = |runner| -> Vec<u64> {
            Splitter::new(text.as_bytes(), runner)
                .map(|p| p.unwrap().line)
                .collect()
        };
        // SQL*Plus: one buffer cut inside the string at line 1, then `y')`; SQLcl: two statements.
        assert_eq!(lines(Runner::SqlPlus), [1, 2]);
        assert_eq!(lines(Runner::Sqlcl), [1, 1]);
    }

    #[test]
    fn a_hyphen_continued_header_is_still_a_block() {
        let text = "CREATE OR REPLACE -\nPROCEDURE p AS\nBEGIN\nNULL;\nEND;\n/\n";
        let kinds: Vec<_> = Splitter::new(text.as_bytes(), Runner::SqlPlus)
            .map(|p| p.unwrap().kind)
            .collect();
        assert_eq!(kinds, [crate::source::PieceKind::Block]);
    }
}
