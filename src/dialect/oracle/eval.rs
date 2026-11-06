//! What an Oracle value expression means: literals, negation, and the conversion functions SQL Developer writes,
//! with the date masks it uses. Anything that depends on the session (`SYSDATE`, NLS settings, a sequence) is
//! unknown.

use std::borrow::Cow;

use super::RULES;
use super::lex::same_ignoring_case;
use crate::source::Encoding;
use crate::statement::Expr;
use crate::value::{self, Literal};

pub fn evaluate<'a>(expr: &Expr<'a>, encoding: Encoding) -> Literal<'a> {
    match expr {
        Expr::Null => Literal::Null,
        Expr::Number(text) if text.ends_with(['f', 'F', 'd', 'D']) => {
            Literal::Unknown(format!("the binary floating-point literal {text}"))
        }
        Expr::Number(text) => Literal::Number(text.clone()),
        Expr::String { bytes, .. } => Literal::Text(match bytes {
            Cow::Borrowed(bytes) => encoding.decode(bytes),
            Cow::Owned(bytes) => Cow::Owned(encoding.decode(bytes).into_owned()),
        }),
        Expr::Typed { type_name, text } => typed(type_name, text),
        Expr::Call { function, args } => call(function, args, encoding),
        Expr::Negate(inner) => negate(evaluate(inner, encoding)),
        Expr::NextValue(_) => Literal::Unknown("a sequence's next value".to_owned()),
        Expr::Default => Literal::Unknown("DEFAULT".to_owned()),
        Expr::Other(text) => Literal::Unknown(format!("`{text}`")),
    }
}

fn typed(type_name: &str, text: &str) -> Literal<'static> {
    match (type_name, value::parse_iso(text)) {
        ("DATE", Some(Literal::Date(days))) => Literal::Date(days),
        ("TIMESTAMP", Some(Literal::Date(days))) => Literal::Timestamp {
            seconds: days * SECONDS_PER_DAY,
            nanos: 0,
        },
        ("TIMESTAMP", Some(timestamp @ Literal::Timestamp { .. })) => timestamp,
        _ => Literal::Unknown(format!("{type_name} '{text}'")),
    }
}

fn negate(literal: Literal<'_>) -> Literal<'_> {
    match literal {
        Literal::Number(text) | Literal::Text(text) if !text.is_empty() => {
            Literal::Number(match text {
                Cow::Borrowed(text) => match text.strip_prefix('-') {
                    Some(positive) => Cow::Borrowed(positive),
                    None => Cow::Owned(format!("-{}", text.strip_prefix('+').unwrap_or(text))),
                },
                Cow::Owned(text) => Cow::Owned(match text.strip_prefix('-') {
                    Some(positive) => positive.to_owned(),
                    None => format!("-{}", text.strip_prefix('+').unwrap_or(&text)),
                }),
            })
        }
        Literal::Null | Literal::Text(_) => Literal::Null,
        Literal::Unknown(reason) => Literal::Unknown(reason),
        _ => Literal::Unknown("a negated date".to_owned()),
    }
}

fn call<'a>(function: &str, args: &[Expr<'a>], encoding: Encoding) -> Literal<'a> {
    match (function, args) {
        ("TO_DATE", [text, mask]) => convert(text, mask, Conversion::Date, encoding),
        ("TO_TIMESTAMP", [text, mask]) => convert(text, mask, Conversion::Timestamp, encoding),
        ("TO_NUMBER", [text]) => match argument(text, encoding) {
            Literal::Text(text) => Literal::Number(text),
            literal @ (Literal::Number(_) | Literal::Null | Literal::Unknown(_)) => literal,
            _ => Literal::Unknown("TO_NUMBER of a date".to_owned()),
        },
        _ => Literal::Unknown(format!("{function}(…)")),
    }
}

/// A function's argument; `''` passed to a function is NULL.
fn argument<'a>(expr: &Expr<'a>, encoding: Encoding) -> Literal<'a> {
    match evaluate(expr, encoding) {
        Literal::Text(text) if text.is_empty() && RULES.empty_string_is_null => Literal::Null,
        literal => literal,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conversion {
    Date,
    Timestamp,
}

fn convert(
    text: &Expr<'_>,
    mask: &Expr<'_>,
    conversion: Conversion,
    encoding: Encoding,
) -> Literal<'static> {
    match (argument(text, encoding), argument(mask, encoding)) {
        (Literal::Null, _) | (_, Literal::Null) => Literal::Null,
        (Literal::Text(text), Literal::Text(mask)) => {
            datetime(&text, &mask, conversion).unwrap_or_else(Literal::Unknown)
        }
        _ => Literal::Unknown("a date conversion of a value that isn't text".to_owned()),
    }
}

const SECONDS_PER_DAY: i64 = 86_400;

/// A date format element; `Separator` is any punctuation or blank, which matches any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Element {
    Day,
    Month,
    MonthAbbreviation,
    MonthName,
    Year,
    /// `RR`/`RRRR`: a two-digit year 00–49 is 20xx, 50–99 is 19xx; more digits are the year as written.
    RoundedYear,
    Hour24,
    Hour12,
    Minute,
    Second,
    /// Up to this many fraction digits.
    Fraction(usize),
    /// `X`, the radix character: `.` or `,` by the session's settings.
    Radix,
    Meridian,
    Separator,
}

/// The element names starting with the upper-case `letter`, longer names first, so `HH24` isn't read as `HH` and
/// `MONTH` not as `MON`.
fn named(letter: u8) -> &'static [(&'static str, Element)] {
    match letter {
        b'H' => &[
            ("HH24", Element::Hour24),
            ("HH12", Element::Hour12),
            ("HH", Element::Hour12),
        ],
        b'M' => &[
            ("MONTH", Element::MonthName),
            ("MON", Element::MonthAbbreviation),
            ("MM", Element::Month),
            ("MI", Element::Minute),
        ],
        b'D' => &[("DD", Element::Day)],
        b'Y' => &[("YYYY", Element::Year)],
        b'R' => &[("RRRR", Element::RoundedYear), ("RR", Element::RoundedYear)],
        b'S' => &[("SS", Element::Second)],
        b'F' => &[
            ("FF1", Element::Fraction(1)),
            ("FF2", Element::Fraction(2)),
            ("FF3", Element::Fraction(3)),
            ("FF4", Element::Fraction(4)),
            ("FF5", Element::Fraction(5)),
            ("FF6", Element::Fraction(6)),
            ("FF7", Element::Fraction(7)),
            ("FF8", Element::Fraction(8)),
            ("FF9", Element::Fraction(9)),
            ("FF", Element::Fraction(9)),
        ],
        b'X' => &[("X", Element::Radix)],
        b'A' => &[("A.M.", Element::Meridian), ("AM", Element::Meridian)],
        b'P' => &[("P.M.", Element::Meridian), ("PM", Element::Meridian)],
        _ => &[],
    }
}

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];

const MONTH_NAMES: [&str; 12] = [
    "JANUARY",
    "FEBRUARY",
    "MARCH",
    "APRIL",
    "MAY",
    "JUNE",
    "JULY",
    "AUGUST",
    "SEPTEMBER",
    "OCTOBER",
    "NOVEMBER",
    "DECEMBER",
];

/// AM at even indexes, PM at odd ones.
const MERIDIANS: [&str; 4] = ["A.M.", "P.M.", "AM", "PM"];

fn is_separator(b: u8) -> bool {
    (b.is_ascii_punctuation() && b != b'"') || b == b' '
}

/// The mask's elements in order; `None` for a part that is no element.
fn elements(mask: &str) -> impl Iterator<Item = Option<Element>> {
    let mut rest = mask.as_bytes();
    std::iter::from_fn(move || {
        let &b = rest.first()?;
        let first = b.to_ascii_uppercase();
        let named = named(first).iter().find(|(name, _)| {
            rest.get(..name.len())
                .is_some_and(|head| same_ignoring_case(head, name.as_bytes()))
        });
        let (element, len) = match named {
            Some((name, element)) => (Some(*element), name.len()),
            None if is_separator(b) => (Some(Element::Separator), 1),
            None => (None, rest.len()),
        };
        rest = rest.get(len..).unwrap_or_default();
        Some(element)
    })
}

/// Up to `max` digits from the start of `rest`, and how many there were.
fn digits(rest: &mut &[u8], max: usize) -> Option<(u32, usize)> {
    let count = rest
        .iter()
        .take(max)
        .take_while(|b| b.is_ascii_digit())
        .count();
    let (digits, tail) = rest.split_at_checked(count)?;
    if digits.is_empty() {
        return None;
    }
    *rest = tail;
    Some((
        digits.iter().fold(0, |n, &b| n * 10 + u32::from(b - b'0')),
        count,
    ))
}

/// The index of the one of `words` that `rest` starts with, in any case.
fn word(rest: &mut &[u8], words: &[&str]) -> Option<usize> {
    let (index, len) = words.iter().enumerate().find_map(|(i, w)| {
        rest.get(..w.len())
            .is_some_and(|head| same_ignoring_case(head, w.as_bytes()))
            .then_some((i, w.len()))
    })?;
    *rest = rest.get(len..).unwrap_or_default();
    Some(index)
}

fn month(rest: &mut &[u8], names: &[&str]) -> Option<u32> {
    word(rest, names).and_then(|index| u32::try_from(index + 1).ok())
}

/// `rest` after its first byte, when that byte is one `accept` takes.
fn one(rest: &[u8], accept: impl Fn(u8) -> bool) -> Option<&[u8]> {
    rest.split_first()
        .filter(|(b, _)| accept(**b))
        .map(|(_, tail)| tail)
}

#[derive(Debug, Default)]
struct Parts {
    year: Option<i64>,
    month: Option<u32>,
    day: Option<u32>,
    hour: u32,
    hour12: bool,
    pm: Option<bool>,
    minute: u32,
    second: u32,
    nanos: u32,
    time: bool,
    fraction: bool,
}

/// `text` read by `mask`, as Oracle's `TO_DATE` or `TO_TIMESTAMP` would; the error is the reason it's unknown.
fn datetime(text: &str, mask: &str, conversion: Conversion) -> Result<Literal<'static>, String> {
    let mismatch = || format!("`{text}` read with the date mask `{mask}`");
    let mut parts = Parts::default();
    let mut rest = text.as_bytes();
    // The whole mask is checked before any of the text is read.
    if elements(mask).any(|e| e.is_none()) {
        return Err(format!("the date mask `{mask}`"));
    }
    for element in elements(mask).flatten() {
        let read = |rest: &mut &[u8], max| digits(rest, max).ok_or_else(mismatch);
        match element {
            Element::Separator => rest = one(rest, is_separator).ok_or_else(mismatch)?,
            Element::Radix => {
                rest = one(rest, |b| b == b'.' || b == b',').ok_or_else(mismatch)?;
            }
            Element::Day => parts.day = Some(read(&mut rest, 2)?.0),
            Element::Month => parts.month = Some(read(&mut rest, 2)?.0),
            Element::MonthAbbreviation => {
                parts.month = Some(month(&mut rest, &MONTHS).ok_or_else(mismatch)?);
            }
            Element::MonthName => {
                parts.month = Some(month(&mut rest, &MONTH_NAMES).ok_or_else(mismatch)?);
            }
            Element::Year => parts.year = Some(i64::from(read(&mut rest, 4)?.0)),
            Element::RoundedYear => {
                let (year, count) = read(&mut rest, 4)?;
                parts.year = Some(i64::from(match (count, year) {
                    (3.., _) => year,
                    (_, 0..50) => 2000 + year,
                    _ => 1900 + year,
                }));
            }
            Element::Hour24 | Element::Hour12 => {
                parts.hour = read(&mut rest, 2)?.0;
                parts.hour12 = element == Element::Hour12;
                parts.time = true;
            }
            Element::Minute => {
                parts.minute = read(&mut rest, 2)?.0;
                parts.time = true;
            }
            Element::Second => {
                parts.second = read(&mut rest, 2)?.0;
                parts.time = true;
            }
            Element::Fraction(max) => {
                let (fraction, count) = read(&mut rest, max)?;
                parts.nanos = fraction
                    * 10_u32.pow(u32::try_from(9_usize.saturating_sub(count)).unwrap_or(0));
                parts.time = true;
                parts.fraction = true;
            }
            Element::Meridian => {
                parts.pm = Some(word(&mut rest, &MERIDIANS).ok_or_else(mismatch)? % 2 == 1);
            }
        }
    }
    if !rest.is_empty() {
        return Err(mismatch());
    }
    if conversion == Conversion::Date && parts.fraction {
        return Err(format!("TO_DATE with fractional seconds in `{mask}`"));
    }
    let (Some(year), Some(month), Some(day)) = (parts.year, parts.month, parts.day) else {
        // A missing part defaults to the current year or month: the session's clock.
        return Err(format!("the date mask `{mask}` without a full date"));
    };
    let hour = match (parts.hour12, parts.pm) {
        (true, _) if !(1..=12).contains(&parts.hour) => return Err(mismatch()),
        (true, Some(pm)) => parts.hour % 12 + if pm { 12 } else { 0 },
        (false, Some(_)) => return Err(mismatch()),
        _ => parts.hour,
    };
    if year == 0 || hour > 23 || parts.minute > 59 || parts.second > 59 {
        return Err(mismatch());
    }
    let days = value::days_from_date(year, month, day).ok_or_else(mismatch)?;
    let seconds =
        days * SECONDS_PER_DAY + i64::from(hour * 3600 + parts.minute * 60 + parts.second);
    Ok(match conversion {
        Conversion::Date if !parts.time => Literal::Date(days),
        Conversion::Date => Literal::Timestamp { seconds, nanos: 0 },
        Conversion::Timestamp => Literal::Timestamp {
            seconds,
            nanos: parts.nanos,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statement::Name;

    fn text(s: &str) -> Expr<'static> {
        Expr::String {
            bytes: s.as_bytes().to_vec().into(),
            national: false,
        }
    }

    fn call(function: &str, args: Vec<Expr<'static>>) -> Expr<'static> {
        Expr::Call {
            function: function.to_owned().into(),
            args,
        }
    }

    fn eval<'a>(expr: &Expr<'a>) -> Literal<'a> {
        evaluate(expr, Encoding::Utf8)
    }

    fn to_date(t: &str, mask: &str) -> Literal<'static> {
        eval(&call("TO_DATE", vec![text(t), text(mask)]))
    }

    fn to_timestamp(t: &str, mask: &str) -> Literal<'static> {
        eval(&call("TO_TIMESTAMP", vec![text(t), text(mask)]))
    }

    fn date(y: i64, m: u32, d: u32) -> Literal<'static> {
        Literal::Date(value::days_from_date(y, m, d).unwrap())
    }

    fn at(
        y: i64,
        m: u32,
        d: u32,
        hour: i64,
        minute: i64,
        second: i64,
        nanos: u32,
    ) -> Literal<'static> {
        Literal::Timestamp {
            seconds: value::days_from_date(y, m, d).unwrap() * SECONDS_PER_DAY
                + hour * 3600
                + minute * 60
                + second,
            nanos,
        }
    }

    fn is_unknown(literal: &Literal) -> bool {
        matches!(literal, Literal::Unknown(_))
    }

    #[test]
    fn literals_evaluate_to_their_values() {
        assert_eq!(eval(&Expr::Null), Literal::Null);
        assert_eq!(
            eval(&Expr::Number("1.0E2".into())),
            Literal::Number("1.0E2".into())
        );
        assert_eq!(eval(&text("")), Literal::Text("".into()));
        assert!(is_unknown(&eval(&Expr::Number("3f".into()))));
        let latin1 = Expr::String {
            bytes: b"M\xe4kinen".to_vec().into(),
            national: true,
        };
        assert_eq!(
            evaluate(&latin1, Encoding::Windows1252),
            Literal::Text("Mäkinen".into())
        );
    }

    #[test]
    fn negation_flips_the_sign_of_numbers_and_numeric_text() {
        let neg = |e: Expr<'static>| eval(&Expr::Negate(Box::new(e))).into_owned();
        assert_eq!(
            neg(Expr::Number("1E0".into())),
            Literal::Number("-1E0".into())
        );
        assert_eq!(neg(text("5")), Literal::Number("-5".into()));
        assert_eq!(
            neg(Expr::Negate(Box::new(Expr::Number("2".into())))),
            Literal::Number("2".into())
        );
        assert_eq!(neg(Expr::Null), Literal::Null);
        assert_eq!(neg(text("")), Literal::Null);
        assert!(is_unknown(&neg(Expr::Other("SYSDATE".into()))));
        assert!(is_unknown(&neg(call(
            "TO_DATE",
            vec![text("2003-06-17"), text("YYYY-MM-DD")]
        ))));
    }

    #[test]
    fn every_sql_developer_mask_reads() {
        assert_eq!(to_date("17-JUN-03", "DD-MON-RR"), date(2003, 6, 17));
        assert_eq!(to_date("17-jun-2003", "DD-MON-YYYY"), date(2003, 6, 17));
        assert_eq!(to_date("2003-06-17", "YYYY-MM-DD"), date(2003, 6, 17));
        assert_eq!(to_date("17.06.2003", "DD.MM.YYYY"), date(2003, 6, 17));
        assert_eq!(
            to_date("2003-06-17 13:45:09", "YYYY-MM-DD HH24:MI:SS"),
            at(2003, 6, 17, 13, 45, 9, 0)
        );
        assert_eq!(
            to_timestamp(
                "21-SEP-05 01.02.03.000000000 PM",
                "DD-MON-RR HH.MI.SSXFF AM"
            ),
            at(2005, 9, 21, 13, 2, 3, 0)
        );
        assert_eq!(
            to_timestamp(
                "05-NOV-26 08.18.44.916140000 PM",
                "DD-MON-RR HH.MI.SSXFF AM"
            ),
            at(2026, 11, 5, 20, 18, 44, 916_140_000)
        );
        assert_eq!(to_date("17 June 2003", "DD MONTH YYYY"), date(2003, 6, 17));
        assert_eq!(
            to_timestamp("2003-06-17 10:00:00.5", "YYYY-MM-DD HH24:MI:SS.FF3"),
            at(2003, 6, 17, 10, 0, 0, 500_000_000)
        );
    }

    #[test]
    fn the_radix_is_a_point_or_a_comma() {
        // SQL Developer writes the session's radix character, `,` in many locales.
        assert_eq!(
            to_timestamp(
                "21-SEP-05 12.00.00,000000000 AM",
                "DD-MON-RR HH.MI.SSXFF AM"
            ),
            at(2005, 9, 21, 0, 0, 0, 0)
        );
        assert!(is_unknown(&to_timestamp(
            "21-SEP-05 12.00.00:0 AM",
            "DD-MON-RR HH.MI.SSXFF AM"
        )));
    }

    #[test]
    fn twelve_am_is_midnight_and_twelve_pm_noon() {
        let mask = "DD-MON-RR HH.MI.SSXFF AM";
        assert_eq!(
            to_timestamp("01-JAN-20 12.00.00.0 AM", mask),
            at(2020, 1, 1, 0, 0, 0, 0)
        );
        assert_eq!(
            to_timestamp("01-JAN-20 12.30.00.0 PM", mask),
            at(2020, 1, 1, 12, 30, 0, 0)
        );
        assert_eq!(
            to_timestamp("01-JAN-20 11.59.59.0 p.m.", "DD-MON-RR HH.MI.SSXFF A.M."),
            at(2020, 1, 1, 23, 59, 59, 0)
        );
        assert!(is_unknown(&to_timestamp("01-JAN-20 13.00.00.0 PM", mask)));
        assert!(is_unknown(&to_timestamp("01-JAN-20 00.00.00.0 AM", mask)));
    }

    #[test]
    fn rr_puts_00_to_49_in_this_century_and_50_to_99_in_the_last() {
        assert_eq!(to_date("01-JAN-00", "DD-MON-RR"), date(2000, 1, 1));
        assert_eq!(to_date("01-JAN-49", "DD-MON-RR"), date(2049, 1, 1));
        assert_eq!(to_date("01-JAN-50", "DD-MON-RR"), date(1950, 1, 1));
        assert_eq!(to_date("01-JAN-99", "DD-MON-RR"), date(1999, 1, 1));
        assert_eq!(to_date("01-JAN-1949", "DD-MON-RR"), date(1949, 1, 1));
        assert_eq!(to_date("01-JAN-49", "DD-MON-RRRR"), date(2049, 1, 1));
    }

    #[test]
    fn what_oracle_would_refuse_or_the_session_decides_is_unknown() {
        for (t, mask) in [
            ("31-FEB-03", "DD-MON-RR"),
            ("29-FEB-2023", "DD-MON-YYYY"),
            ("17-JUNE-03", "DD-MON-RR"),
            ("17-XYZ-03", "DD-MON-RR"),
            ("17-JUN-03 x", "DD-MON-RR"),
            ("17-JUN", "DD-MON-RR"),
            ("2003-06-17 24:00:00", "YYYY-MM-DD HH24:MI:SS"),
            ("2003-06-17 10:60:00", "YYYY-MM-DD HH24:MI:SS"),
            ("0000-01-01", "YYYY-MM-DD"),
            ("17-JUN-03", "DD-MON-YY"),
            ("2003", "YYYY"),
            ("17/06/2003", "DD/MM/YYYY\"x\""),
            ("17-JUN-03 10.00.00.5 AM", "DD-MON-RR HH.MI.SSXFF AM"),
            ("2003-06-17 10 AM", "YYYY-MM-DD HH24 AM"),
        ] {
            assert!(is_unknown(&to_date(t, mask)), "{t} {mask}");
        }
        assert!(is_unknown(&eval(&call("TO_DATE", vec![text("17-JUN-03")]))));
        assert_eq!(
            eval(&call("TO_DATE", vec![Expr::Null, text("DD-MON-RR")])),
            Literal::Null
        );
        assert_eq!(to_date("", "DD-MON-RR"), Literal::Null);
    }

    #[test]
    fn date_and_timestamp_literals_are_iso() {
        let typed = |type_name: &str, t: &str| {
            eval(&Expr::Typed {
                type_name: type_name.to_owned().into(),
                text: t.to_owned().into(),
            })
        };
        assert_eq!(typed("DATE", "2003-06-17"), date(2003, 6, 17));
        assert_eq!(
            typed("TIMESTAMP", "2003-06-17 10:11:12.25"),
            at(2003, 6, 17, 10, 11, 12, 250_000_000)
        );
        assert_eq!(
            typed("TIMESTAMP", "2003-06-17"),
            at(2003, 6, 17, 0, 0, 0, 0)
        );
        assert!(is_unknown(&typed("DATE", "2003-06-17 10:11:12")));
        assert!(is_unknown(&typed("DATE", "17-JUN-03")));
        assert!(is_unknown(&typed("INTERVAL", "1")));
    }

    #[test]
    fn to_number_takes_text_as_a_number() {
        assert_eq!(
            eval(&call("TO_NUMBER", vec![text("0100")])),
            Literal::Number("0100".into())
        );
        assert_eq!(
            eval(&call("TO_NUMBER", vec![Expr::Number("5".into())])),
            Literal::Number("5".into())
        );
        assert_eq!(eval(&call("TO_NUMBER", vec![text("")])), Literal::Null);
        assert!(is_unknown(&eval(&call(
            "TO_NUMBER",
            vec![text("1,000"), text("9G999")]
        ))));
    }

    #[test]
    fn session_values_sequences_and_other_calls_are_unknown() {
        for expr in [
            Expr::Other("SYSDATE".into()),
            Expr::Other("SYSTIMESTAMP".into()),
            Expr::Default,
            Expr::NextValue(Name {
                schema: None,
                name: crate::statement::Ident {
                    value: "SEQ".into(),
                    quoted: false,
                },
            }),
            call("EMPTY_CLOB", vec![]),
            call("UNISTR", vec![text("\\00e4")]),
        ] {
            assert!(is_unknown(&eval(&expr)), "{expr:?}");
        }
    }
}
