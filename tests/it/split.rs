use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use sqlca::dialect::oracle::split::{Runner, Splitter};
use sqlca::source::{Piece, PieceKind};

#[expect(
    clippy::unwrap_used,
    reason = "an unreadable file fails the test that split it"
)]
pub(crate) fn pieces(path: &Path) -> Vec<Piece> {
    Splitter::new(BufReader::new(File::open(path).unwrap()), Runner::SqlPlus)
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The line a piece ends on.
pub(crate) fn last_line(piece: &Piece) -> u64 {
    piece.line + u64::try_from(piece.text.split(|&b| b == b'\n').count() - 1).unwrap_or(0)
}

#[cfg(feature = "gen")]
mod generated {
    use super::*;
    use sqlca::synth::{self, Encoding, Layout, LineEnding, Noise, Options, Placement};

    #[test]
    fn generated_exports_split_into_the_statements_the_generator_wrote() {
        let variants = [
            (
                Layout::Single,
                Placement::BeforeData,
                LineEnding::Lf,
                Encoding::Utf8,
            ),
            (
                Layout::Separate,
                Placement::AfterData,
                LineEnding::Crlf,
                Encoding::Windows1252,
            ),
            (
                Layout::Type,
                Placement::BeforeData,
                LineEnding::Crlf,
                Encoding::Utf8,
            ),
        ];
        for (seed, (layout, constraints, line_ending, encoding)) in (1..).zip(variants) {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("export");
            let options = Options {
                seed,
                rows: 60,
                layout,
                constraints,
                line_ending,
                encoding,
                noise: Noise::all(),
                ..Options::default()
            };
            let manifest = synth::generate(&options, &root).unwrap();
            for file in &manifest.files {
                let pieces = pieces(&root.join(&file.path));
                let odd: Vec<&Piece> = pieces
                    .iter()
                    .filter(|p| matches!(p.kind, PieceKind::Unterminated | PieceKind::RunAgain))
                    .collect();
                assert!(odd.is_empty(), "{}: {odd:?}", file.path);
                assert_eq!(
                    u64::try_from(pieces.len()).unwrap(),
                    file.statements,
                    "{} ({layout:?})",
                    file.path
                );
                for finding in manifest
                    .findings
                    .iter()
                    .filter(|f| f.file.as_ref() == Some(&file.path))
                {
                    let line = finding.line.unwrap();
                    let piece = pieces
                        .iter()
                        .find(|p| p.line <= line && line <= last_line(p))
                        .unwrap();
                    let insert_all = piece
                        .text
                        .get(..10)
                        .is_some_and(|t| t.eq_ignore_ascii_case(b"INSERT ALL"));
                    assert!(
                        piece.line == line || insert_all,
                        "{}:{line} falls inside the piece at line {}",
                        file.path,
                        piece.line
                    );
                }
            }
        }
    }
}
