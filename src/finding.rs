//! A constraint violation in an export: what the analyzer reports, and what the generator's manifest expects.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// The foreign key's parent row exists nowhere in the input.
    FkMissingParent,
    /// The parent row exists, but is inserted after the child while the foreign key is already enforced.
    FkLoadOrder,
    /// A primary key, unique constraint or unique index value inserted a second time.
    DuplicateKey,
    /// NULL, or `''`, in a NOT NULL column.
    NotNull,
    UnknownTable,
    UnknownColumn,
    ValueCountMismatch,
    /// The foreign key's parent table is defined but has no rows; one finding per foreign key, with a row count.
    ParentTableEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Location {
    pub file: String,
    pub line: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    /// `SCHEMA.NAME` of the constraint or unique index; absent for structure errors and unnamed NOT NULLs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
    /// `SCHEMA.TABLE` of the row.
    pub table: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The line the row starts on: the statement's first line, or its `INTO` line in an `INSERT ALL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// Canonical values of `columns`; `None` is NULL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<Option<String>>,
    /// For a duplicate, the row that inserted the key first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<Location>,
    /// For a summary finding, the number of rows it stands for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

/// An identifier as Oracle stores it, quoted when it isn't a plain upper-case name.
pub fn display_ident(name: &str) -> Cow<'_, str> {
    let mut chars = name.chars();
    let plain = chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '_' | '$' | '#'));
    if plain {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(format!("\"{}\"", name.replace('"', "\"\"")))
    }
}

pub fn qualified_name(schema: &str, name: &str) -> String {
    format!("{}.{}", display_ident(schema), display_ident(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_stay_unquoted_and_others_are_quoted() {
        assert_eq!(qualified_name("APP", "EMPLOYEES"), "APP.EMPLOYEES");
        assert_eq!(qualified_name("APP", "AuditLog"), "APP.\"AuditLog\"");
        assert_eq!(display_ident("1ABC"), "\"1ABC\"");
        assert_eq!(display_ident("A\"B"), "\"A\"\"B\"");
    }

    #[test]
    fn optional_fields_are_left_out_of_json() {
        let finding = Finding {
            kind: FindingKind::UnknownTable,
            constraint: None,
            table: "APP.NOPE".into(),
            columns: vec![],
            file: Some("app/data.sql".into()),
            line: Some(3),
            key: vec![],
            first: None,
            count: None,
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"unknown_table","table":"APP.NOPE","columns":[],"file":"app/data.sql","line":3}"#
        );
        assert_eq!(serde_json::from_str::<Finding>(&json).unwrap(), finding);
    }
}
