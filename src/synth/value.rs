//! Column values: how each is written as an Oracle literal, and its canonical form for keys.

use super::rng::Rng;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Null,
    Int(i64),
    /// A decimal in its canonical text, such as `7.5`.
    Decimal(String),
    Text(String),
    /// A `CHAR(n)` value; trailing spaces don't count when comparing.
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

    /// The value as the key comparison sees it; `None` is NULL, and Oracle stores `''` as NULL.
    pub(crate) fn canonical(&self) -> Option<String> {
        match self {
            Self::Null | Self::NextVal(..) => None,
            Self::Int(n) => Some(n.to_string()),
            Self::Decimal(s) => Some(s.clone()),
            Self::Text(s) => (!s.is_empty()).then(|| s.clone()),
            Self::Char(s) => {
                let trimmed = s.trim_end_matches(' ');
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
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

/// Literal styles the noise options switch on.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LiteralStyle {
    pub(crate) quoted_numbers: bool,
    pub(crate) numeric_forms: bool,
    pub(crate) q_quote: bool,
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

impl LiteralStyle {
    pub(crate) fn render(self, value: &Value, rng: &mut Rng) -> String {
        match value {
            Value::Null => "null".into(),
            Value::Int(n) => self.number(&n.to_string(), rng),
            Value::Decimal(s) => self.number(s, rng),
            Value::Text(s) | Value::Char(s) => self.string(s, rng),
            Value::Date(days) => {
                let (y, m, d) = civil_from_days(*days);
                format!(
                    "to_date('{d:02}-{}-{:02}','DD-MON-RR')",
                    month(m),
                    y.rem_euclid(100)
                )
            }
            Value::Timestamp(secs) => {
                let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
                let s = secs.rem_euclid(86_400);
                let (h24, min, sec) = (s / 3600, s % 3600 / 60, s % 60);
                let (h12, ampm) = match h24 {
                    0 => (12, "AM"),
                    1..=11 => (h24, "AM"),
                    12 => (12, "PM"),
                    _ => (h24 - 12, "PM"),
                };
                format!(
                    "to_timestamp('{d:02}-{}-{:02} {h12:02}.{min:02}.{sec:02}.000000000 {ampm}','DD-MON-RR HH.MI.SSXFF AM')",
                    month(m),
                    y.rem_euclid(100)
                )
            }
            Value::NextVal(schema, seq) => format!("{schema}.{seq}.NEXTVAL"),
        }
    }

    fn number(self, digits: &str, rng: &mut Rng) -> String {
        if self.numeric_forms {
            match rng.below(5) {
                0 => digits.to_owned(),
                2 if !digits.contains('.') => format!("{digits}.0"),
                3 if !digits.contains('.') => format!("{digits}E0"),
                4 if !digits.contains('.') && !digits.starts_with('-') => format!("'0{digits}'"),
                _ => format!("'{digits}'"),
            }
        } else if self.quoted_numbers {
            format!("'{digits}'")
        } else {
            digits.to_owned()
        }
    }

    fn string(self, s: &str, rng: &mut Rng) -> String {
        if self.q_quote && s.contains('\'') && !s.contains("]'") && rng.below(2) == 0 {
            format!("q'[{s}]'")
        } else {
            format!("'{}'", s.replace('\'', "''"))
        }
    }
}

fn month(m: u32) -> &'static str {
    MONTHS
        .get(usize::try_from(m.saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or("JAN")
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
    fn literals_look_like_sql_developer_writes_them() {
        let mut rng = Rng::new(1, 1);
        let sqldev = LiteralStyle {
            quoted_numbers: true,
            ..LiteralStyle::default()
        };
        assert_eq!(sqldev.render(&Value::Int(100), &mut rng), "'100'");
        assert_eq!(
            sqldev.render(&Value::text("O'Brien"), &mut rng),
            "'O''Brien'"
        );
        assert_eq!(
            sqldev.render(&Value::Date(days_from_civil(2003, 6, 17)), &mut rng),
            "to_date('17-JUN-03','DD-MON-RR')"
        );
        let ts = days_from_civil(2005, 9, 21) * 86_400 + 13 * 3600 + 2 * 60 + 3;
        assert_eq!(
            sqldev.render(&Value::Timestamp(ts), &mut rng),
            "to_timestamp('21-SEP-05 01.02.03.000000000 PM','DD-MON-RR HH.MI.SSXFF AM')"
        );
        assert_eq!(
            LiteralStyle::default().render(&Value::Null, &mut rng),
            "null"
        );
    }

    #[test]
    fn canonical_forms_follow_oracle_comparison() {
        assert_eq!(Value::text("").canonical(), None);
        assert_eq!(Value::Char("FI ".into()).canonical().as_deref(), Some("FI"));
        assert_eq!(Value::Int(10).canonical().as_deref(), Some("10"));
    }

    #[test]
    fn every_numeric_form_means_the_same_number() {
        let style = LiteralStyle {
            numeric_forms: true,
            ..LiteralStyle::default()
        };
        let mut rng = Rng::new(9, 9);
        let forms: std::collections::BTreeSet<String> = (0..200)
            .map(|_| style.render(&Value::Int(10), &mut rng))
            .collect();
        assert_eq!(
            forms.into_iter().collect::<Vec<_>>(),
            ["'010'", "'10'", "10", "10.0", "10E0"]
        );
        let negative: std::collections::BTreeSet<String> = (0..200)
            .map(|_| style.render(&Value::Int(-3), &mut rng))
            .collect();
        assert_eq!(
            negative.into_iter().collect::<Vec<_>>(),
            ["'-3'", "-3", "-3.0", "-3E0"]
        );
    }
}
