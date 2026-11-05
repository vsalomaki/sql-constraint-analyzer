//! What differs between SQL dialects. The rules every dialect answers live here; each dialect's syntax lives in its
//! own module. Adding a dialect is one more module and one more `DialectId` value.

pub mod oracle;

use std::borrow::Cow;
use std::io::{self, BufRead};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DialectId {
    #[default]
    Oracle,
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
        encoding: crate::source::Encoding,
    ) -> io::Result<Vec<Include>> {
        match self {
            Self::Oracle => oracle::script::includes(reader, runner, encoding),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    pub fold: Fold,
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

    /// An identifier as the dialect stores it, quoted when it isn't a plain name in the folded case.
    pub fn display_ident<'a>(&self, name: &'a str) -> Cow<'a, str> {
        let folded = |c: char| match self.fold {
            Fold::Upper => c.is_ascii_uppercase(),
            Fold::Lower => c.is_ascii_lowercase(),
        };
        let mut chars = name.chars();
        let plain = chars.next().is_some_and(folded)
            && chars.all(|c| folded(c) || c.is_ascii_digit() || matches!(c, '_' | '$' | '#'));
        if plain {
            Cow::Borrowed(name)
        } else {
            let q = self.ident_quote;
            Cow::Owned(format!("{q}{}{q}", name.replace(q, &format!("{q}{q}"))))
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
        let lower = Rules {
            fold: Fold::Lower,
            ..*rules
        };
        assert_eq!(lower.qualified("app", "EMPLOYEES"), "app.\"EMPLOYEES\"");
        assert_eq!(rules.fold("Emp_ä"), "EMP_Ä");
        assert_eq!(lower.fold("Emp"), "emp");
    }
}
