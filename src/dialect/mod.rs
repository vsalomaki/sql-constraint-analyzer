//! What differs between SQL dialects. The rules every dialect answers live here; each dialect's syntax lives in its
//! own module. Adding a dialect is one more module and one more `DialectId` value.

pub mod oracle;

use std::borrow::Cow;
use std::io::{self, BufRead};

use serde::{Deserialize, Serialize};

use crate::source::{Encoding, Piece};
use crate::statement::{Expr, ParseError, Statement};
use crate::value::Literal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DialectId {
    #[default]
    Oracle,
}

/// Which pass reads a script: the schema pass wants table and index DDL only, the rows pass everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Schema,
    Rows,
}

/// The program that runs the script; its rules decide where statements end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Runner {
    #[default]
    #[value(name = "sqlplus")]
    SqlPlus,
    /// SQLcl, and SQL Developer's script engine.
    #[value(name = "sqlcl")]
    Sqlcl,
}

/// One include command of a master script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    /// The line the command starts on, from 1.
    pub line: u64,
    /// The name is looked up beside the script that holds the command, not in the working directory.
    pub beside_script: bool,
    /// The file name as written, quotes and arguments removed; a default extension is not added yet.
    pub name: String,
}

impl DialectId {
    /// The include commands of the script in `reader`, in order.
    pub fn script_includes(
        self,
        reader: impl BufRead,
        runner: Runner,
        encoding: Encoding,
    ) -> io::Result<Vec<Include>> {
        match self {
            Self::Oracle => oracle::script::includes(reader, runner, encoding),
        }
    }

    /// The pieces of the script in `reader`, which starts `offset` bytes into its file, split by `runner`'s rules.
    pub fn pieces<'r>(
        self,
        reader: impl BufRead + 'r,
        runner: Runner,
        offset: u64,
    ) -> Box<dyn Iterator<Item = io::Result<Piece>> + 'r> {
        match self {
            Self::Oracle => {
                Box::new(oracle::split::Splitter::new(reader, runner).starting_at(offset))
            }
        }
    }

    /// What a piece of a script does for `pass`, as a neutral statement; `None` when it belongs to the other pass.
    pub fn parse(
        self,
        piece: &Piece,
        encoding: Encoding,
        pass: Pass,
    ) -> Result<Option<Statement<'_>>, ParseError> {
        match self {
            Self::Oracle => oracle::parse::parse(piece, encoding, pass),
        }
    }

    /// The value an expression in an `INSERT` stands for.
    pub fn evaluate<'a>(self, expr: &Expr<'a>, encoding: Encoding) -> Literal<'a> {
        match self {
            Self::Oracle => oracle::eval::evaluate(expr, encoding),
        }
    }

    /// The statement that commits, as a script line without its line ending.
    pub fn commit_statement(self) -> &'static str {
        match self {
            Self::Oracle => oracle::COMMIT,
        }
    }

    pub fn rules(self) -> &'static Rules {
        match self {
            Self::Oracle => &oracle::RULES,
        }
    }

    #[cfg(feature = "gen")]
    pub(crate) fn render(self, noise: crate::synth::Noise) -> Box<dyn crate::synth::Render> {
        match self {
            Self::Oracle => Box::new(oracle::synth::Render::new(noise)),
        }
    }
}

/// The case an unquoted identifier folds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Upper,
    Lower,
}

/// How unique keys with NULL parts compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueNulls {
    /// A key with some NULLs conflicts with another whose non-NULL parts are equal; all-NULL keys never conflict.
    PartialKeysConflict,
    /// A key with any NULL part never conflicts.
    NeverConflict,
}

/// The semantics a dialect gives the things the checker and the generator compare.
#[derive(Debug, Clone, Copy)]
pub struct Rules {
    pub fold: Fold,
    /// Whether a name can be written unquoted, case aside.
    pub unquoted: fn(&str) -> bool,
    pub ident_quote: char,
    pub empty_string_is_null: bool,
    /// `CHAR` values compare as if padded, so trailing spaces don't count.
    pub char_ignores_trailing_spaces: bool,
    pub unique_nulls: UniqueNulls,
    pub deferrable_foreign_keys: bool,
}

impl Rules {
    /// An unquoted identifier as the dialect stores it.
    pub fn fold(&self, name: &str) -> String {
        match self.fold {
            Fold::Upper => name.to_uppercase(),
            Fold::Lower => name.to_lowercase(),
        }
    }

    /// `fold`, borrowing `name` when it is ASCII already in the folded case, as exports mostly write names.
    pub fn folded<'a>(&self, name: Cow<'a, str>) -> Cow<'a, str> {
        let other_case = |b: &u8| match self.fold {
            Fold::Upper => b.is_ascii_lowercase(),
            Fold::Lower => b.is_ascii_uppercase(),
        };
        if name.is_ascii() && !name.as_bytes().iter().any(other_case) {
            name
        } else {
            Cow::Owned(self.fold(&name))
        }
    }

    /// An identifier as the dialect stores it, quoted when it isn't a plain name in the folded case.
    pub fn display_ident<'a>(&self, name: &'a str) -> Cow<'a, str> {
        // Unquoted, a name folds, so it shows bare only when folding keeps it: `ß` folds to `SS`.
        let plain = (self.unquoted)(name) && *self.folded(Cow::Borrowed(name)) == *name;
        if plain {
            Cow::Borrowed(name)
        } else {
            let q = self.ident_quote;
            Cow::Owned(format!("{q}{}{q}", name.replace(q, &format!("{q}{q}"))))
        }
    }

    /// A folder's name as a schema: folded when it could be written unquoted, as written otherwise, as if quoted.
    pub fn folder_schema(&self, folder: &str) -> String {
        if (self.unquoted)(folder) {
            self.fold(folder)
        } else {
            folder.to_owned()
        }
    }

    pub fn qualified(&self, schema: &str, name: &str) -> String {
        format!(
            "{}.{}",
            self.display_ident(schema),
            self.display_ident(name)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_stay_unquoted_and_others_are_quoted() {
        let rules = DialectId::Oracle.rules();
        assert_eq!(rules.qualified("APP", "EMPLOYEES"), "APP.EMPLOYEES");
        assert_eq!(rules.qualified("APP", "AuditLog"), "APP.\"AuditLog\"");
        assert_eq!(rules.display_ident("1ABC"), "\"1ABC\"");
        assert_eq!(rules.display_ident("A\"B"), "\"A\"\"B\"");
        assert_eq!(rules.display_ident("EMP_Ä"), "EMP_Ä");
        assert_eq!(rules.display_ident("ä"), "\"ä\"");
        assert_eq!(rules.display_ident("ß"), "\"ß\"");
        assert_eq!(rules.display_ident("_X"), "\"_X\"");
        let lower = Rules {
            fold: Fold::Lower,
            ..*rules
        };
        assert_eq!(lower.qualified("app", "EMPLOYEES"), "app.\"EMPLOYEES\"");
        assert_eq!(rules.fold("Emp_ä"), "EMP_Ä");
        assert_eq!(lower.fold("Emp"), "emp");
    }

    #[test]
    fn a_folder_names_a_schema_folded_when_it_could_be_written_unquoted() {
        let rules = DialectId::Oracle.rules();
        assert_eq!(rules.folder_schema("app"), "APP");
        assert_eq!(rules.folder_schema("Hr_2$"), "HR_2$");
        assert_eq!(rules.folder_schema("Äpp"), "ÄPP");
        for odd in ["my-schema", "2024", "a b", "_app", "€x"] {
            assert_eq!(rules.folder_schema(odd), odd);
        }
    }

    #[test]
    fn a_folder_folds_as_its_name_unquoted_in_sql_does() {
        let dialect = DialectId::Oracle;
        let in_sql = |schema: &str| {
            let piece = Piece {
                kind: crate::source::PieceKind::Sql,
                span: crate::source::Span { start: 0, end: 0 },
                line: 1,
                text: format!("CREATE TABLE {schema}.T (A NUMBER)").into_bytes(),
            };
            let Ok(Some(Statement::CreateTable { name, .. })) =
                dialect.parse(&piece, Encoding::Utf8, Pass::Schema)
            else {
                panic!("{schema}");
            };
            name.schema.unwrap().value.into_owned()
        };
        for folder in ["ä", "app", "Äpp", "ß"] {
            assert_eq!(
                dialect.rules().folder_schema(folder),
                in_sql(folder),
                "{folder}"
            );
        }
        assert_eq!(dialect.rules().folder_schema("ä"), "Ä");
    }
}
