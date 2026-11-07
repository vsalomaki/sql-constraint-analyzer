//! One piece of an Oracle script as a neutral statement. The schema pass takes table and index DDL; the rows pass
//! takes inserts and names everything else as skipped, saying why the checker has no use for it.

use super::cursor::leading_words;
use super::split::command;
use super::{ddl, insert};
use crate::dialect::Pass;
use crate::source::{Encoding, Piece, PieceKind};
use crate::statement::{ParseError, Skip, Statement};

/// Statements that change rows or tables in ways the checker doesn't replay; `CALL` runs a procedure that may, and
/// `ROLLBACK` discards rows since the last commit.
const NOT_FOLLOWED: [&str; 7] = [
    "UPDATE", "DELETE", "MERGE", "TRUNCATE", "DROP", "CALL", "ROLLBACK",
];

/// DDL, which Oracle commits before and after, so it ends the transaction as `COMMIT` does. `ALTER SESSION` and
/// `ALTER SYSTEM` aren't DDL.
const COMMITTING: [&str; 7] = [
    "CREATE", "ALTER", "GRANT", "REVOKE", "COMMENT", "RENAME", "AUDIT",
];

/// Words between `CREATE` and the kind of object, left out of a skip's name.
const MODIFIERS: [&str; 6] = [
    "OR",
    "REPLACE",
    "EDITIONABLE",
    "NONEDITIONABLE",
    "FORCE",
    "NOFORCE",
];

pub fn parse<'a>(
    piece: &'a Piece,
    encoding: Encoding,
    pass: Pass,
    memo: Option<&'a mut insert::Memo>,
) -> Result<Option<Statement<'a>>, ParseError> {
    match pass {
        Pass::Schema => ddl::parse_ddl(piece, encoding),
        Pass::Rows => rows(piece, encoding, memo).map(Some),
    }
}

/// `EXIT` or `QUIT`: SQL*Plus stops there, so nothing after it runs, and it may roll back.
fn is_exit(word: &str) -> bool {
    word == "EXIT" || word == "QUIT"
}

fn rows<'a>(
    piece: &'a Piece,
    encoding: Encoding,
    memo: Option<&'a mut insert::Memo>,
) -> Result<Statement<'a>, ParseError> {
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
        && let Some(statement) = insert::parse_insert_with(piece, encoding, memo)?
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
    let first = words.first().map_or("", String::as_str);
    let second = words.get(1).map_or("", String::as_str);
    let commits = match piece.kind {
        PieceKind::Sql => {
            first == "COMMIT"
                || (COMMITTING.contains(&first)
                    && !(first == "ALTER" && ["SESSION", "SYSTEM"].contains(&second)))
        }
        // A stored unit (`CREATE TRIGGER …`) is DDL too.
        PieceKind::Block => first == "CREATE",
        PieceKind::Command | PieceKind::RunAgain | PieceKind::Unterminated => false,
    };
    if commits {
        return Ok(Statement::Commit);
    }
    let changes_data = match piece.kind {
        PieceKind::Sql => words
            .first()
            .is_some_and(|w| NOT_FOLLOWED.contains(&w.as_str())),
        // An anonymous block can insert anything; a stored unit (`CREATE TRIGGER …`) only defines code.
        PieceKind::Block => words
            .first()
            .is_some_and(|w| w == "BEGIN" || w == "DECLARE"),
        // `EXEC[UTE]` runs a procedure, which may change rows; nothing after `EXIT` runs.
        PieceKind::Command => words
            .first()
            .is_some_and(|w| command(w.as_bytes()) == Some("EXECUTE") || is_exit(w)),
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
    fn read(piece: &Piece) -> Result<Statement<'static>, ParseError> {
        let schema = parse(piece, Encoding::Utf8, Pass::Schema, None)?;
        let rows = parse(piece, Encoding::Utf8, Pass::Rows, None)?;
        match (schema, rows) {
            (Some(s), None | Some(Statement::Commit)) | (None, Some(s)) => Ok(s.into_owned()),
            other => panic!("{other:?}"),
        }
    }

    fn sql(text: &str) -> Statement<'static> {
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
        assert_eq!(
            skip(PieceKind::Sql, "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY'"),
            unused("ALTER SESSION")
        );
        assert_eq!(skip(PieceKind::Sql, "rollback"), not_followed("ROLLBACK"));
        assert_eq!(
            skip(PieceKind::Command, "EXIT ROLLBACK"),
            not_followed("EXIT ROLLBACK")
        );
        assert_eq!(skip(PieceKind::Command, "quit"), not_followed("QUIT"));
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
    fn a_commit_and_ddl_end_the_transaction() {
        let rows = |kind, text| {
            parse(&piece(kind, text), Encoding::Utf8, Pass::Rows, None)
                .unwrap()
                .map(Statement::into_owned)
        };
        for text in [
            "commit",
            "COMMIT WORK",
            "CREATE SEQUENCE \"APP\".\"S\"",
            "ALTER TRIGGER X ENABLE",
            "CREATE TABLE T (A NUMBER)",
            "CREATE GLOBAL TEMPORARY TABLE T (A NUMBER)",
            "ALTER TABLE T ADD PRIMARY KEY (A)",
            "CREATE UNIQUE INDEX I ON T (A)",
            "CREATE BITMAP INDEX I ON T (A)",
            "GRANT SELECT ON T TO U",
        ] {
            assert_eq!(
                rows(PieceKind::Sql, text),
                Some(Statement::Commit),
                "{text}"
            );
        }
        assert_eq!(
            rows(
                PieceKind::Block,
                "CREATE OR REPLACE TRIGGER X BEFORE INSERT ON T"
            ),
            Some(Statement::Commit)
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
        let read = |text: &str| -> Vec<Result<Statement<'static>, ParseError>> {
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
