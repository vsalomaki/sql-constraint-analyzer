//! Column values and their canonical form for keys; each dialect writes them as literals.

use crate::dialect::Rules;

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

/// Days since 1970-01-01 → (year, month 1–12, day 1–31), proleptic Gregorian (Howard Hinnant's algorithm).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// (year, month, day) → days since 1970-01-01.
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_through_the_civil_calendar() {
        for days in [-1, 0, 59, 10_957, 11_016, 20_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2003, 6, 17)), (2003, 6, 17));
    }

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
