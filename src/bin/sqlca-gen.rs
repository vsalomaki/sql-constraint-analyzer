use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

/// Writes SQL Developer–style Oracle exports with planted constraint violations, and a manifest of the expected
/// findings.
#[derive(Debug, Parser)]
#[command(name = "sqlca-gen", version)]
struct Cli {}

fn main() -> ExitCode {
    let _cli = Cli::parse();
    // A failed write to stderr has nowhere left to be reported.
    drop(writeln!(
        io::stderr().lock(),
        "error: `sqlca-gen` is not implemented yet"
    ));
    ExitCode::from(2)
}
