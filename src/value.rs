//! Values the checker compares: what a dialect evaluates an expression to, and its canonical form for a column of a
//! given type family under the dialect's rules. Keys are compared by these canonical forms.

use crate::dialect::Rules;
use crate::statement::{DataType, Decimal, TypeFamily};

/// A value after the dialect evaluated it. Text is decoded by then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    Null,
    /// A number as decimal text, not yet normalized: `100`, `-1.50`, `1.0E2`.
    Number(String),
    Text(String),
    /// Days since 1970-01-01.
    Date(i64),
    /// Seconds since 1970-01-01, and nanoseconds.
    Timestamp {
        seconds: i64,
        nanos: u32,
    },
    /// A value the checker can't know, such as `SYSDATE` or a sequence; the reason, for the warning.
    Unknown(String),
}

/// A value as keys compare it. Two values of a column are equal exactly when their canonical forms are.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Canonical {
    Null,
    Value(String),
    /// The value can't be known; a key containing one is unverifiable, never a violation.
    Unknown,
}

/// `literal` as keys of a `family` column compare it, under the dialect's `rules`.
///
/// Numbers are decimal text (`100`, `-0.5`); dates and timestamps are `YYYY-MM-DD HH:MM:SS`, timestamps with nine
/// fraction digits. A value the column couldn't hold, such as text that isn't a number in a number column, is
/// unknown: the database would have refused the row, which is not a key question. Text in a date column is read as
/// ISO, an assumption [`assumption`] reports.
pub fn canonical(literal: &Literal, column: &DataType, rules: &Rules) -> Canonical {
    let value = match literal {
        Literal::Null => return Canonical::Null,
        Literal::Unknown(_) => return Canonical::Unknown,
        Literal::Text(text) if text.is_empty() && rules.empty_string_is_null => {
            return Canonical::Null;
        }
        _ => match column.family {
            TypeFamily::Number => number_value(literal).and_then(|n| stored(n, column.decimal)),
            // Blank-padded comparison: `'FI'` and `'FI '` are equal, and `' '` is a value, not NULL.
            TypeFamily::FixedText if rules.char_ignores_trailing_spaces => {
                text_value(literal).map(|mut text| {
                    text.truncate(text.trim_end_matches(' ').len());
                    text
                })
            }
            TypeFamily::Text | TypeFamily::FixedText => text_value(literal),
            TypeFamily::Date => instant(literal, false),
            TypeFamily::Timestamp => instant(literal, true),
            TypeFamily::Binary | TypeFamily::Other => None,
        },
    };
    value.map_or(Canonical::Unknown, Canonical::Value)
}

/// Why a canonical value rests on something the input doesn't show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assumption {
    /// Text in a date column, read as ISO; Oracle reads it with the session's date format (`NLS_DATE_FORMAT`).
    DateFormat,
}

/// What `canonical` assumed to give `literal` a value in a `family` column, if anything.
pub fn assumption(literal: &Literal, family: TypeFamily) -> Option<Assumption> {
    let timestamp = match family {
        TypeFamily::Date => false,
        TypeFamily::Timestamp => true,
        _ => return None,
    };
    match literal {
        Literal::Text(_) if instant(literal, timestamp).is_some() => Some(Assumption::DateFormat),
        _ => None,
    }
}

/// A canonical number as a decimal column stores it: rounded half away from zero to its scale; `None` when it has
/// more digits than the precision allows, which the database refuses.
fn stored(number: String, decimal: Option<Decimal>) -> Option<String> {
    let Some(Decimal { precision, scale }) = decimal else {
        return Some(number);
    };
    let rounded = round(number, scale)?;
    let unsigned = rounded.strip_prefix('-').unwrap_or(&rounded);
    let (int, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    // The position of the first significant digit: 3 for 123, 0 for 0.5, -2 for 0.004.
    let magnitude = if int == "0" {
        -i64::try_from(fraction.bytes().take_while(|&b| b == b'0').count()).ok()?
    } else {
        i64::try_from(int.len()).ok()?
    };
    let fits = rounded == "0"
        || precision.is_none_or(|precision| magnitude <= i64::from(precision) - i64::from(scale));
    fits.then_some(rounded)
}

/// A canonical number rounded half away from zero to `scale` digits after the point (before it when negative).
fn round(number: String, scale: i16) -> Option<String> {
    let (negative, unsigned) = match number.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, number.as_str()),
    };
    let (int, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if usize::try_from(scale).is_ok_and(|scale| fraction.len() <= scale) {
        return Some(number);
    }
    let mut digits: Vec<u8> = int
        .bytes()
        .chain(fraction.bytes())
        .map(|b| b - b'0')
        .collect();
    let mut point = i64::try_from(int.len()).ok()?;
    let keep = point + i64::from(scale);
    let Ok(keep_digits) = usize::try_from(keep) else {
        return Some("0".to_owned());
    };
    let up = digits.get(keep_digits).is_some_and(|&d| d >= 5);
    digits.truncate(keep_digits);
    if up {
        let mut carry = true;
        for digit in digits.iter_mut().rev() {
            if *digit == 9 {
                *digit = 0;
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            digits.insert(0, 1);
            point += 1;
        }
    }
    let point = usize::try_from(point).ok()?;
    let text: String = digits.iter().map(|d| char::from(b'0' + d)).collect();
    let (whole, part) = if text.len() >= point {
        let (whole, part) = text.split_at_checked(point)?;
        (whole.to_owned(), part.to_owned())
    } else {
        (
            format!("{text}{}", "0".repeat(point - text.len())),
            String::new(),
        )
    };
    let sign = if negative { "-" } else { "" };
    normalize_number(&format!("{sign}{whole}.{part}"))
}

fn number_value(literal: &Literal) -> Option<String> {
    match literal {
        Literal::Number(text) => normalize_number(text),
        // Implicit conversion allows blanks around the number.
        Literal::Text(text) => normalize_number(text.trim_matches(' ')),
        _ => None,
    }
}

fn text_value(literal: &Literal) -> Option<String> {
    match literal {
        Literal::Text(text) => Some(text.clone()),
        // A number converted to text has no zero before the point: `.5`, `-.5`.
        Literal::Number(text) => {
            let number = normalize_number(text)?;
            Some(match number.strip_prefix("0.") {
                Some(fraction) => format!(".{fraction}"),
                None => number.replacen("-0.", "-.", 1),
            })
        }
        _ => None,
    }
}

/// Decimal text in canonical form: no exponent, no leading or trailing zeros, `0` for zero, `-` only on a negative
/// number. `None` when it isn't a number, or is beyond the range of a 38-digit decimal type (1E-130 up to 1E126).
fn normalize_number(text: &str) -> Option<String> {
    // Most exported numbers are plain integers, already canonical.
    let plain = text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
        && (1..=126).contains(&text.len());
    if plain {
        return Some(text.to_owned());
    }
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, parse_exponent(exponent)?),
        None => (unsigned, 0),
    };
    let (int, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let leading = digits.bytes().take_while(|&b| b == b'0').count();
    let significant = digits.get(leading..)?.trim_end_matches('0');
    if significant.is_empty() {
        return Some("0".to_owned());
    }
    // The value is 0.<significant> × 10^point.
    let point = i64::try_from(int.len())
        .ok()?
        .checked_add(exponent)?
        .checked_sub(i64::try_from(leading).ok()?)?;
    if !(-129..=126).contains(&point) {
        return None;
    }
    let len = i64::try_from(significant.len()).ok()?;
    let zeros = |n: i64| "0".repeat(usize::try_from(n).unwrap_or(0));
    let body = if point <= 0 {
        format!("0.{}{significant}", zeros(-point))
    } else if point >= len {
        format!("{significant}{}", zeros(point - len))
    } else {
        let (whole, part) = significant.split_at_checked(usize::try_from(point).ok()?)?;
        format!("{whole}.{part}")
    };
    Some(if negative { format!("-{body}") } else { body })
}

fn parse_exponent(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

const SECONDS_PER_DAY: i64 = 86_400;

/// `YYYY-MM-DD HH:MM:SS`, with `.fffffffff` for a timestamp. A date holds whole seconds, so a fraction in a date
/// column is unknown rather than guessed at.
fn instant(literal: &Literal, timestamp: bool) -> Option<String> {
    let (seconds, nanos) = match literal {
        Literal::Date(days) => (days.checked_mul(SECONDS_PER_DAY)?, 0),
        Literal::Timestamp { seconds, nanos } => (*seconds, *nanos),
        // Read as ISO, which `assumption` reports.
        Literal::Text(text) => return instant(&parse_iso(text)?, timestamp),
        _ => return None,
    };
    let (year, month, day) = civil_from_days(seconds.div_euclid(SECONDS_PER_DAY));
    let time = seconds.rem_euclid(SECONDS_PER_DAY);
    let base = format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    );
    if timestamp {
        Some(format!("{base}.{nanos:09}"))
    } else {
        (nanos == 0).then_some(base)
    }
}

/// `YYYY-MM-DD` as a date, or `YYYY-MM-DD HH:MM:SS[.f…]` (up to nine fraction digits) as a timestamp.
pub fn parse_iso(text: &str) -> Option<Literal> {
    let (date, time) = match text.split_once(' ') {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let [year, month, day] = fields(date, '-', [4, 2, 2])?;
    let days = days_from_date(i64::from(year), month, day)?;
    let Some(time) = time else {
        return Some(Literal::Date(days));
    };
    let (clock, fraction) = match time.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (time, None),
    };
    let [hour, minute, second] = fields(clock, ':', [2, 2, 2])?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let nanos = match fraction {
        Some(digits) => fraction_nanos(digits)?,
        None => 0,
    };
    Some(Literal::Timestamp {
        seconds: days * SECONDS_PER_DAY + i64::from(hour * 3600 + minute * 60 + second),
        nanos,
    })
}

/// Three `separator`-separated runs of exactly `widths` digits.
fn fields(text: &str, separator: char, widths: [usize; 3]) -> Option<[u32; 3]> {
    let mut parts = text.split(separator);
    let mut out = [0; 3];
    for (slot, width) in out.iter_mut().zip(widths) {
        let part = parts.next()?;
        if part.len() != width || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Up to nine digits of a second's fraction, as nanoseconds.
fn fraction_nanos(digits: &str) -> Option<u32> {
    if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    format!("{digits:0<9}").parse().ok()
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar; `None` when there is no such day.
pub fn days_from_date(year: i64, month: u32, day: u32) -> Option<i64> {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > days_in_month {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// (year, month 1–12, day 1–31) of a day counted from 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::DialectId;

    fn oracle() -> &'static Rules {
        DialectId::Oracle.rules()
    }

    fn value(text: &str) -> Canonical {
        Canonical::Value(text.to_owned())
    }

    #[test]
    fn numbers_normalize_to_one_decimal_form() {
        let table = [
            ("100", Some("100")),
            ("'100'", None),
            ("0100", Some("100")),
            ("100.0", Some("100")),
            ("100.", Some("100")),
            ("1.0E2", Some("100")),
            ("1e+2", Some("100")),
            ("10E0", Some("10")),
            ("+7", Some("7")),
            ("-0", Some("0")),
            ("-0.0E5", Some("0")),
            ("000", Some("0")),
            (".5", Some("0.5")),
            ("-.50", Some("-0.5")),
            ("7.50", Some("7.5")),
            ("1.5E-3", Some("0.0015")),
            ("123.456E1", Some("1234.56")),
            ("12345E-2", Some("123.45")),
            ("-1E3", Some("-1000")),
            (
                "12345678901234567890123456789012345678",
                Some("12345678901234567890123456789012345678"),
            ),
            ("1E125", Some(&*format!("1{}", "0".repeat(125)))),
            ("1E126", None),
            ("1E-130", Some(&*format!("0.{}1", "0".repeat(129)))),
            ("1E-131", None),
            ("1E99999999999999999999", None),
            ("", None),
            ("-", None),
            (".", None),
            ("1.2.3", None),
            ("1e", None),
            ("e5", None),
            ("1,5", None),
            ("0x10", None),
            ("--1", None),
            ("1 ", None),
        ];
        for (input, expected) in table {
            assert_eq!(normalize_number(input).as_deref(), expected, "{input}");
        }
    }

    #[test]
    fn a_number_column_compares_numbers_and_numeric_text_alike() {
        let rules = oracle();
        for literal in [
            Literal::Number("100".into()),
            Literal::Number("1.0E2".into()),
            Literal::Text("100".into()),
            Literal::Text(" 0100 ".into()),
            Literal::Text("100.0".into()),
        ] {
            assert_eq!(
                canonical(&literal, &DataType::of(TypeFamily::Number), rules),
                value("100"),
                "{literal:?}"
            );
        }
        assert_eq!(
            canonical(
                &Literal::Text("abc".into()),
                &DataType::of(TypeFamily::Number),
                rules
            ),
            Canonical::Unknown
        );
        assert_eq!(
            canonical(&Literal::Date(0), &DataType::of(TypeFamily::Number), rules),
            Canonical::Unknown
        );
    }

    #[test]
    fn empty_text_is_null_where_the_rules_say_so() {
        let rules = oracle();
        let empty = Literal::Text(String::new());
        for family in [
            TypeFamily::Text,
            TypeFamily::FixedText,
            TypeFamily::Number,
            TypeFamily::Date,
        ] {
            assert_eq!(
                canonical(&empty, &DataType::of(family), rules),
                Canonical::Null
            );
        }
        let strict = Rules {
            empty_string_is_null: false,
            char_ignores_trailing_spaces: false,
            ..*rules
        };
        assert_eq!(
            canonical(&empty, &DataType::of(TypeFamily::Text), &strict),
            value("")
        );
        assert_eq!(
            canonical(&Literal::Null, &DataType::of(TypeFamily::Text), &strict),
            Canonical::Null
        );
    }

    #[test]
    fn fixed_text_ignores_trailing_spaces_and_text_keeps_them() {
        let rules = oracle();
        let padded = Literal::Text("FI ".into());
        assert_eq!(
            canonical(&padded, &DataType::of(TypeFamily::FixedText), rules),
            value("FI")
        );
        assert_eq!(
            canonical(&padded, &DataType::of(TypeFamily::Text), rules),
            value("FI ")
        );
        // A blank is a value, not NULL: only the empty string is.
        assert_eq!(
            canonical(
                &Literal::Text("  ".into()),
                &DataType::of(TypeFamily::FixedText),
                rules
            ),
            value("")
        );
        let strict = Rules {
            char_ignores_trailing_spaces: false,
            ..*rules
        };
        assert_eq!(
            canonical(&padded, &DataType::of(TypeFamily::FixedText), &strict),
            value("FI ")
        );
    }

    #[test]
    fn numbers_in_text_columns_read_as_converted_text() {
        let rules = oracle();
        let text = |n: &str| {
            canonical(
                &Literal::Number(n.into()),
                &DataType::of(TypeFamily::Text),
                rules,
            )
        };
        assert_eq!(text("100"), value("100"));
        assert_eq!(text("1.0E2"), value("100"));
        assert_eq!(text("0.5"), value(".5"));
        assert_eq!(text("-0.5"), value("-.5"));
        assert_eq!(text("x"), Canonical::Unknown);
    }

    #[test]
    fn dates_and_timestamps_print_by_the_column_family() {
        let rules = oracle();
        let day = days_from_date(2003, 6, 17).unwrap();
        let date = Literal::Date(day);
        let noon = Literal::Timestamp {
            seconds: day * SECONDS_PER_DAY + 12 * 3600 + 5,
            nanos: 0,
        };
        let fraction = Literal::Timestamp {
            seconds: day * SECONDS_PER_DAY,
            nanos: 916_140_000,
        };
        assert_eq!(
            canonical(&date, &DataType::of(TypeFamily::Date), rules),
            value("2003-06-17 00:00:00")
        );
        assert_eq!(
            canonical(&date, &DataType::of(TypeFamily::Timestamp), rules),
            value("2003-06-17 00:00:00.000000000")
        );
        assert_eq!(
            canonical(&noon, &DataType::of(TypeFamily::Date), rules),
            value("2003-06-17 12:00:05")
        );
        assert_eq!(
            canonical(&fraction, &DataType::of(TypeFamily::Timestamp), rules),
            value("2003-06-17 00:00:00.916140000")
        );
        assert_eq!(
            canonical(&fraction, &DataType::of(TypeFamily::Date), rules),
            Canonical::Unknown
        );
        assert_eq!(
            canonical(&Literal::Date(-1), &DataType::of(TypeFamily::Date), rules),
            value("1969-12-31 00:00:00")
        );
        assert_eq!(
            canonical(&date, &DataType::of(TypeFamily::Text), rules),
            Canonical::Unknown
        );
    }

    #[test]
    fn text_in_a_date_column_is_known_only_in_iso_form() {
        let rules = oracle();
        let date = |t: &str| {
            canonical(
                &Literal::Text(t.into()),
                &DataType::of(TypeFamily::Date),
                rules,
            )
        };
        let timestamp = |t: &str| {
            canonical(
                &Literal::Text(t.into()),
                &DataType::of(TypeFamily::Timestamp),
                rules,
            )
        };
        assert_eq!(date("2024-02-29"), value("2024-02-29 00:00:00"));
        assert_eq!(date("2024-02-29 23:59:59"), value("2024-02-29 23:59:59"));
        assert_eq!(
            timestamp("2024-02-29 23:59:59.5"),
            value("2024-02-29 23:59:59.500000000")
        );
        for bad in [
            "17-JUN-03",
            "2023-02-29",
            "2024-13-01",
            "2024-1-01",
            "2024-01-01 24:00:00",
            "2024-01-01 10:00",
            "2024-01-01 10:00:00.",
            "2024-01-01 10:00:00.1234567890",
            "2024-01-01-01",
        ] {
            assert_eq!(timestamp(bad), Canonical::Unknown, "{bad}");
        }
    }

    #[test]
    fn iso_text_in_a_date_column_is_an_assumption_about_the_date_format() {
        let iso = Literal::Text("2024-01-31".into());
        assert_eq!(
            assumption(&iso, TypeFamily::Date),
            Some(Assumption::DateFormat)
        );
        assert_eq!(
            assumption(&iso, TypeFamily::Timestamp),
            Some(Assumption::DateFormat)
        );
        assert_eq!(assumption(&iso, TypeFamily::Text), None);
        // What `to_date(…)` and `to_timestamp(…)` evaluate to carries its own mask.
        let day = days_from_date(2024, 1, 31).unwrap();
        assert_eq!(assumption(&Literal::Date(day), TypeFamily::Date), None);
        assert_eq!(
            assumption(
                &Literal::Timestamp {
                    seconds: day * SECONDS_PER_DAY,
                    nanos: 0
                },
                TypeFamily::Timestamp
            ),
            None
        );
        assert_eq!(
            assumption(&Literal::Text(String::new()), TypeFamily::Date),
            None
        );
        let other = Literal::Text("31-JAN-24".into());
        assert_eq!(
            canonical(&other, &DataType::of(TypeFamily::Date), oracle()),
            Canonical::Unknown
        );
        assert_eq!(assumption(&other, TypeFamily::Date), None);
        // A fraction a date column can't hold is unknown, so nothing was assumed.
        let fraction = Literal::Text("2024-01-31 10:00:00.5".into());
        assert_eq!(assumption(&fraction, TypeFamily::Date), None);
    }

    #[test]
    fn unknown_and_untyped_columns_are_unknown() {
        let rules = oracle();
        assert_eq!(
            canonical(
                &Literal::Unknown("x".into()),
                &DataType::of(TypeFamily::Text),
                rules
            ),
            Canonical::Unknown
        );
        assert_eq!(
            canonical(
                &Literal::Text("ab".into()),
                &DataType::of(TypeFamily::Binary),
                rules
            ),
            Canonical::Unknown
        );
        assert_eq!(
            canonical(&Literal::Null, &DataType::of(TypeFamily::Other), rules),
            Canonical::Null
        );
    }

    #[test]
    fn the_calendar_round_trips() {
        for days in [-719_162, -1, 0, 59, 10_957, 11_016, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_date(y, m, d), Some(days));
        }
        assert_eq!(days_from_date(1970, 1, 1), Some(0));
        assert_eq!(days_from_date(2000, 2, 29), Some(11_016));
        assert_eq!(days_from_date(1900, 2, 29), None);
        assert_eq!(days_from_date(2001, 4, 31), None);
        assert_eq!(days_from_date(2001, 0, 1), None);
        assert_eq!(fraction_nanos("5"), Some(500_000_000));
        assert_eq!(fraction_nanos("000000001"), Some(1));
    }

    #[test]
    fn decimal_columns_round_half_away_from_zero_and_refuse_too_many_digits() {
        let stored = |number: &str, precision: Option<u8>, scale: i16| {
            let column = DataType {
                decimal: Some(Decimal { precision, scale }),
                ..DataType::of(TypeFamily::Number)
            };
            canonical(&Literal::Number(number.into()), &column, oracle())
        };
        let cases: [(&str, u8, i16, Option<&str>); 16] = [
            ("1.1", 5, 0, Some("1")),
            ("1.5", 5, 0, Some("2")),
            ("-1.5", 5, 0, Some("-2")),
            ("2.45", 5, 1, Some("2.5")),
            ("0.4", 5, 0, Some("0")),
            ("-0.4", 5, 0, Some("0")),
            ("9.99", 5, 1, Some("10")),
            ("12345", 5, 0, Some("12345")),
            ("123456", 5, 0, None),
            ("99999.5", 5, 0, None),
            ("1250", 2, -2, Some("1300")),
            ("1249", 2, -2, Some("1200")),
            ("0.00099", 2, 5, Some("0.00099")),
            ("0.001", 2, 5, None),
            ("0.000004", 2, 5, Some("0")),
            ("1E2", 3, 0, Some("100")),
        ];
        for (number, precision, scale, expected) in cases {
            let expected = expected.map_or(Canonical::Unknown, |v| Canonical::Value(v.into()));
            assert_eq!(
                stored(number, Some(precision), scale),
                expected,
                "{number} as NUMBER({precision},{scale})"
            );
        }
        // NUMBER(*,0), INTEGER and DECIMAL round to their scale but have no precision of their own.
        assert_eq!(stored("1E38", Some(38), 0), Canonical::Unknown);
        assert_eq!(
            stored("1E38", None, 0),
            Canonical::Value(format!("1{}", "0".repeat(38)))
        );
        assert_eq!(stored("1.5", None, 0), Canonical::Value("2".into()));
        let any = canonical(
            &Literal::Number("1.1".into()),
            &DataType::of(TypeFamily::Number),
            oracle(),
        );
        assert_eq!(
            any,
            Canonical::Value("1.1".into()),
            "NUMBER without a precision keeps every digit"
        );
    }
}
