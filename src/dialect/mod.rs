//! What differs between SQL dialects. The rules every dialect answers live here; each dialect's syntax lives in its
//! own module. Adding a dialect is one more module and one more `DialectId` value.

pub mod oracle;

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DialectId {
    #[default]
    Oracle,
}

impl DialectId {
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
    }
}
