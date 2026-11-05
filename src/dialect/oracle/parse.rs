//! One piece of an Oracle script as a neutral statement. The schema pass takes table and index DDL; the rows pass
//! takes inserts and names everything else as skipped, saying why the checker has no use for it.

use super::cursor::leading_words;
use super::{ddl, insert};
use crate::dialect::Pass;
use crate::source::{Encoding, Piece, PieceKind};
use crate::statement::{ParseError, Skip, Statement};

/// Statements that change rows or tables in ways the checker doesn't replay; `CALL` runs a procedure that may.
const NOT_FOLLOWED: [&str; 6] = ["UPDATE", "DELETE", "MERGE", "TRUNCATE", "DROP", "CALL"];

/// Words between `CREATE` and the kind of object, left out of a skip's name.
const MODIFIERS: [&str; 6] = [
    "OR",
    "REPLACE",
    "EDITIONABLE",
    "NONEDITIONABLE",
    "FORCE",
    "NOFORCE",
];

pub fn parse(
    piece: &Piece,
    encoding: Encoding,
    pass: Pass,
) -> Result<Option<Statement>, ParseError> {
    match pass {
        Pass::Schema => ddl::parse_ddl(piece, encoding),
        Pass::Rows if piece.kind == PieceKind::Sql && ddl::is_table_ddl(&piece.text) => Ok(None),
        Pass::Rows => rows(piece, encoding).map(Some),
    }
}

fn rows(piece: &Piece, encoding: Encoding) -> Result<Statement, ParseError> {
    if piece.kind == PieceKind::Unterminated {
        return Err(ParseError {
            offset: piece.span.start,
            line: piece.line,
            message:
                "the file ends before this statement's terminator, so the runner doesn't run it"
                    .to_owned(),
        });
    }
    if piece.kind == PieceKind::Sql
        && let Some(statement) = insert::parse_insert(piece, encoding)?
    {
        return Ok(statement);
    }
    let words: Vec<String> = leading_words(&piece.text, 6)
        .into_iter()
        .enumerate()
        .filter(|(i, w)| *i == 0 || !MODIFIERS.contains(&w.as_str()))
        .map(|(_, w)| w)
        .take(2)
        .collect();
    let head = words.join(" ");
    let changes_data = match piece.kind {
        PieceKind::Sql => words
            .first()
            .is_some_and(|w| NOT_FOLLOWED.contains(&w.as_str())),
        // An anonymous block can insert anything; a stored unit (`CREATE TRIGGER …`) only defines code.
        PieceKind::Block => words
            .first()
            .is_some_and(|w| w == "BEGIN" || w == "DECLARE"),
        // `EXEC[UTE]` runs a procedure, which may change rows.
        PieceKind::Command => words
            .first()
            .is_some_and(|w| w.len() >= 4 && "EXECUTE".starts_with(w.as_str())),
        // `/` runs the previous statement a second time.
        PieceKind::RunAgain => true,
        PieceKind::Unterminated => false,
    };
    Ok(Statement::Skipped(if changes_data {
        Skip::NotFollowed(head)
    } else {
        Skip::Unused(head)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Span;

    fn piece(kind: PieceKind, text: &str) -> Piece {
        Piece {
            kind,
            span: Span { start: 0, end: 0 },
            line: 1,
            text: text.as_bytes().to_vec(),
        }
    }

    /// What the schema pass makes of a piece, or else the rows pass; exactly one of them takes it.
    fn read(piece: &Piece) -> Result<Statement, ParseError> {
        let schema = parse(piece, Encoding::Utf8, Pass::Schema)?;
        let rows = parse(piece, Encoding::Utf8, Pass::Rows)?;
        match (schema, rows) {
            (Some(s), None) | (None, Some(s)) => Ok(s),
            other => panic!("{other:?}"),
        }
    }

    fn sql(text: &str) -> Statement {
        read(&piece(PieceKind::Sql, text)).unwrap()
    }

    #[test]
    fn inserts_and_table_ddl_go_to_their_parsers() {
        assert!(matches!(
            sql("insert into t values (1)"),
            Statement::Insert(_)
        ));
        assert!(matches!(
            sql("CREATE TABLE T (A NUMBER)"),
            Statement::CreateTable { .. }
        ));
        assert!(matches!(
            sql("ALTER TABLE T ADD PRIMARY KEY (A)"),
            Statement::AlterTable { .. }
        ));
        assert!(matches!(
            sql("CREATE UNIQUE INDEX I ON T (A)"),
            Statement::CreateIndex { .. }
        ));
    }

    #[test]
    fn the_rest_is_skipped_by_whether_it_changes_data() {
        let skip = |kind, text| match read(&piece(kind, text)).unwrap() {
            Statement::Skipped(skip) => skip,
            other => panic!("{other:?}"),
        };
        let unused = |s: &str| Skip::Unused(s.to_owned());
        let not_followed = |s: &str| Skip::NotFollowed(s.to_owned());
        assert_eq!(skip(PieceKind::Sql, "commit"), unused("COMMIT"));
        assert_eq!(
            skip(PieceKind::Sql, "CREATE SEQUENCE \"APP\".\"S\""),
            unused("CREATE SEQUENCE")
        );
        assert_eq!(
            skip(PieceKind::Sql, "ALTER TRIGGER X ENABLE"),
            unused("ALTER TRIGGER")
        );
        assert_eq!(
            skip(PieceKind::Sql, "-- why\nupdate t set a = 1"),
            not_followed("UPDATE T")
        );
        assert_eq!(
            skip(PieceKind::Sql, "DELETE FROM T"),
            not_followed("DELETE FROM")
        );
        assert_eq!(
            skip(PieceKind::Sql, "INSERT INTO T SELECT * FROM U"),
            not_followed("INSERT … SELECT")
        );
        assert_eq!(
            skip(PieceKind::Block, "BEGIN\n  INSERT INTO T VALUES (1);\nEND;"),
            not_followed("BEGIN INSERT")
        );
        assert_eq!(
            skip(
                PieceKind::Block,
                "CREATE OR REPLACE EDITIONABLE TRIGGER X BEFORE INSERT ON T"
            ),
            unused("CREATE TRIGGER")
        );
        assert_eq!(
            skip(PieceKind::Command, "SET DEFINE OFF"),
            unused("SET DEFINE")
        );
        assert_eq!(
            skip(PieceKind::Command, "exec app.load_rows"),
            not_followed("EXEC APP")
        );
        assert_eq!(
            skip(PieceKind::Command, "EXECUTE load_rows"),
            not_followed("EXECUTE LOAD_ROWS")
        );
        assert_eq!(
            skip(PieceKind::Sql, "CALL app.load_rows()"),
            not_followed("CALL APP")
        );
    }

    #[test]
    fn errors_come_from_the_parser_the_statement_belongs_to() {
        let insert = read(&piece(PieceKind::Sql, "INSERT INTO T VALUES (1"));
        assert!(insert.unwrap_err().message.contains("value list"));
        let ddl = read(&piece(PieceKind::Sql, "CREATE TABLE T (A NUMBER"));
        assert!(ddl.is_err());
    }

    /// Through the splitter: a `/` after a statement runs it again, and a file cut off inside a statement is an
    /// error at its line.
    #[test]
    fn run_again_and_a_cut_off_statement_are_not_silent() {
        let read = |text: &str| -> Vec<Result<Statement, ParseError>> {
            crate::dialect::oracle::split::Splitter::new(
                text.as_bytes(),
                crate::dialect::Runner::SqlPlus,
            )
            .map(|p| read(&p.unwrap()))
            .collect()
        };
        let pieces = read("INSERT INTO T VALUES (1);\n/\n");
        assert!(matches!(
            pieces.as_slice(),
            [
                Ok(Statement::Insert(_)),
                Ok(Statement::Skipped(Skip::NotFollowed(_)))
            ]
        ));
        let pieces = read("COMMIT;\nINSERT INTO T VALUES ('broken\n");
        let [Ok(_), Err(error)] = pieces.as_slice() else {
            panic!("{pieces:?}");
        };
        assert_eq!((error.line, error.offset), (2, 8));
        assert!(error.message.contains("ends before"));
    }
}
