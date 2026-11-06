//! Column values and their canonical form for keys; each dialect writes them as literals.

use crate::dialect::Rules;
use crate::value::civil_from_days;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Null,
    Int(i64),
    /// A decimal in its canonical text, such as `7.5`.
    Decimal(String),
    Text(String),
    /// A `CHAR(n)` value; whether trailing spaces count is a dialect rule.
    Char(String),
    /// Days since 1970-01-01, midnight.
    Date(i64),
    /// Seconds since 1970-01-01.
    Timestamp(i64),
    NextVal(&'static str, &'static str),
}

impl Value {
    pub(crate) fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// The value as the dialect compares it in keys; `None` is NULL.
    pub(crate) fn canonical(&self, rules: &Rules) -> Option<String> {
        match self {
            Self::Null | Self::NextVal(..) => None,
            Self::Int(n) => Some(n.to_string()),
            Self::Decimal(s) => Some(s.clone()),
            Self::Text(s) => (!(s.is_empty() && rules.empty_string_is_null)).then(|| s.clone()),
            Self::Char(s) => {
                let s = if rules.char_ignores_trailing_spaces {
                    s.trim_end_matches(' ')
                } else {
                    s
                };
                (!(s.is_empty() && rules.empty_string_is_null)).then(|| s.to_owned())
            }
            Self::Date(days) => {
                let (y, m, d) = civil_from_days(*days);
                Some(format!("{y:04}-{m:02}-{d:02} 00:00:00"))
            }
            Self::Timestamp(secs) => {
                let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
                let s = secs.rem_euclid(86_400);
                Some(format!(
                    "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.000000000",
                    s / 3600,
                    s % 3600 / 60,
                    s % 60
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_forms_follow_the_dialect_rules() {
        let oracle = crate::dialect::DialectId::Oracle.rules();
        assert_eq!(Value::text("").canonical(oracle), None);
        assert_eq!(
            Value::Char("FI ".into()).canonical(oracle).as_deref(),
            Some("FI")
        );
        assert_eq!(Value::Int(10).canonical(oracle).as_deref(), Some("10"));
        let strict = Rules {
            empty_string_is_null: false,
            char_ignores_trailing_spaces: false,
            ..*oracle
        };
        assert_eq!(Value::text("").canonical(&strict).as_deref(), Some(""));
        assert_eq!(
            Value::Char("FI ".into()).canonical(&strict).as_deref(),
            Some("FI ")
        );
    }
}
