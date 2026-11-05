//! Oracle, as SQL Developer and SQL\*Plus write it.

#[cfg(test)]
#[cfg(feature = "gen")]
mod check;
mod cursor;
pub mod ddl;
pub mod eval;
pub mod insert;
pub mod lex;
pub mod script;
pub mod split;
#[cfg(feature = "gen")]
pub(crate) mod synth;

use super::{Fold, Rules, UniqueNulls};

pub const RULES: Rules = Rules {
    fold: Fold::Upper,
    ident_quote: '"',
    empty_string_is_null: true,
    char_ignores_trailing_spaces: true,
    unique_nulls: UniqueNulls::PartialKeysConflict,
    deferrable_foreign_keys: true,
};
