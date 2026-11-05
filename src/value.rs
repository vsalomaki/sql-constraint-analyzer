//! Values the checker compares: what a dialect evaluates an expression to, and its canonical form for a column of a
//! given type family under the dialect's rules. Keys are compared by these canonical forms.

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
