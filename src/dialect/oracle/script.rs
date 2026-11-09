//! The files an Oracle master script runs: its `@file`, `@@file` and `START file` commands, in order.

use std::io::{self, BufRead};

use super::split::{Splitter, command};
use crate::dialect::{Include, Runner};
use crate::source::Encoding;
use crate::source::{Piece, PieceKind, to_u64};

/// The include commands of the script in `reader`, which starts `offset` bytes into its file, in order. Everything
/// else in it is left alone.
pub fn includes(
    reader: impl BufRead,
    runner: Runner,
    offset: u64,
    encoding: Encoding,
) -> io::Result<Vec<Include>> {
    let mut found = Vec::new();
    for piece in Splitter::new(reader, runner).starting_at(offset, 1) {
        let piece = piece?;
        if piece.kind == PieceKind::Command
            && let Some(include) = parse(&piece, encoding)
        {
            found.push(include);
        }
    }
    Ok(found)
}

fn parse(piece: &Piece, encoding: Encoding) -> Option<Include> {
    let first_line = piece.text.split(|&b| b == b'\n').next().unwrap_or_default();
    let first_line = encoding.decode(first_line);
    let line = first_line.trim();
    let (beside_script, rest) = if let Some(rest) = line.strip_prefix("@@") {
        (true, rest)
    } else if let Some(rest) = line.strip_prefix('@') {
        (false, rest)
    } else {
        let (word, rest) = line.split_once(char::is_whitespace)?;
        if command(word.as_bytes()) != Some("START") {
            return None;
        }
        (false, rest)
    };
    let name = file_name(rest.trim_start())?;
    // A command ending in `-` goes on onto the next line, which its text keeps.
    let lines = to_u64(piece.text.split(|&b| b == b'\n').count());
    Some(Include {
        line: piece.line,
        span: piece.span,
        last_line: piece.line + lines.saturating_sub(1),
        beside_script,
        name,
    })
}

/// The first word, or the first quoted string; SQL*Plus passes the rest as arguments to the script.
fn file_name(rest: &str) -> Option<String> {
    let name = match rest.chars().next()? {
        quote @ ('"' | '\'') => rest.get(1..)?.split(quote).next()?,
        _ => rest
            .split(char::is_whitespace)
            .next()?
            .trim_end_matches(';'),
    };
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Span;

    fn found(text: &str) -> Vec<(u64, bool, String)> {
        includes(text.as_bytes(), Runner::SqlPlus, 0, Encoding::Utf8)
            .unwrap()
            .into_iter()
            .map(|i| (i.line, i.beside_script, i.name))
            .collect()
    }

    #[test]
    fn at_double_at_and_start_are_includes_in_order() {
        let text = "@a\n@@b.sql\nSTART c\nsta d x y\n@ e;\n";
        assert_eq!(
            found(text),
            [
                (1, false, "a".into()),
                (2, true, "b.sql".into()),
                (3, false, "c".into()),
                (4, false, "d".into()),
                (5, false, "e".into()),
            ]
        );
    }

    #[test]
    fn quotes_arguments_and_other_commands_are_handled() {
        let text = "SET DEFINE OFF;\nselect 1 from dual;\n@\"my dir/a b.sql\" arg1\n@'x.sql'\nSTARTUP\nPROMPT @nope\n@\n";
        assert_eq!(
            found(text),
            [
                (3, false, "my dir/a b.sql".into()),
                (4, false, "x.sql".into()),
            ]
        );
    }

    #[test]
    fn an_include_inside_a_statement_is_not_a_command() {
        assert_eq!(found("insert into t values ('\n@a\n');\n"), []);
    }

    #[test]
    fn an_include_has_its_place_in_the_file_as_stored() {
        let text = "select 1 from dual;\r\n@@a -\r\nb.sql\r\n@c\n";
        let found: Vec<_> = includes(text.as_bytes(), Runner::SqlPlus, 3, Encoding::Utf8)
            .unwrap()
            .into_iter()
            .map(|i| (i.span, i.line, i.last_line))
            .collect();
        assert_eq!(
            found,
            [
                (Span { start: 24, end: 36 }, 2, 3),
                (Span { start: 38, end: 40 }, 4, 4)
            ]
        );
    }
}
