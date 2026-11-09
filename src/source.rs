//! What a dialect's splitter yields: the pieces of a script, with their place in the file as stored.

/// Byte offsets in the file as stored: `start` inclusive, `end` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

/// Where a statement runs: the segment of the run it is in, then its offset in that segment's file. Segments are
/// numbered in the order the run reaches them, so a later `RunAt` always runs later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RunAt {
    pub segment: usize,
    pub offset: u64,
}

/// A length or index as a byte offset.
pub(crate) fn to_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    /// A SQL statement.
    Sql,
    /// A procedural block (`BEGIN … END;`, `CREATE TRIGGER …`), which ends only at its own terminator line.
    Block,
    /// A line the script runner handles itself (`REM`, `SET DEFINE OFF`, `@file`), never sent to the database.
    Command,
    /// A run-again line with nothing pending: the runner executes the previous statement a second time.
    RunAgain,
    /// Text the input ended in before its terminator: an open statement, string or comment.
    Unterminated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub kind: PieceKind,
    /// From the piece's first character through its terminator, if it has one.
    pub span: Span,
    /// The line the piece starts on, from 1.
    pub line: u64,
    /// The piece's bytes without its terminator, as stored (not decoded).
    pub text: Vec<u8>,
}

/// How a file's bytes encode text. Both are ASCII-compatible, which the splitters rely on: UTF-16 is refused before
/// a file is split.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    clap::ValueEnum,
)]
pub enum Encoding {
    #[default]
    #[value(name = "utf-8")]
    #[serde(rename = "utf-8")]
    Utf8,
    #[value(name = "windows-1252")]
    #[serde(rename = "windows-1252")]
    Windows1252,
}

impl Encoding {
    /// The text of `bytes`; a byte sequence that isn't valid becomes U+FFFD.
    pub fn decode(self, bytes: &[u8]) -> std::borrow::Cow<'_, str> {
        match self {
            // `from_utf8` checks ASCII a word at a time; the lossy walk is only for text that needs it.
            Self::Utf8 => std::str::from_utf8(bytes).map_or_else(
                |_| String::from_utf8_lossy(bytes),
                std::borrow::Cow::Borrowed,
            ),
            Self::Windows1252 => {
                encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(bytes)
                    .0
            }
        }
    }
}
