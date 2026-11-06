//! Oracle, as SQL Developer and SQL\*Plus write it.

#[cfg(test)]
#[cfg(feature = "gen")]
mod check;
mod cursor;
pub mod ddl;
pub mod eval;
pub mod insert;
pub mod lex;
pub mod parse;
pub mod script;
pub mod split;
#[cfg(feature = "gen")]
pub(crate) mod synth;

use super::{Fold, Rules, UniqueNulls};

/// Whether `name` is a legal unquoted identifier, case aside: a letter, then letters, digits, `_`, `$` or `#`. The
/// lexer is looser, as it needn't reject what an export never writes.
fn unquoted(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(char::is_alphabetic)
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '#'))
}

/// The statement that commits, as a script line without its line ending.
pub const COMMIT: &str = "COMMIT;";

pub const RULES: Rules = Rules {
    fold: Fold::Upper,
    unquoted,
    ident_quote: '"',
    empty_string_is_null: true,
    char_ignores_trailing_spaces: true,
    unique_nulls: UniqueNulls::PartialKeysConflict,
    deferrable_foreign_keys: true,
};
