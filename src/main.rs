use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Checks Oracle SQL exports for rows that violate the declared constraints.
#[derive(Debug, Parser)]
#[command(name = "sqlca", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report rows that violate constraints; never writes files.
    Check,
    /// Write fixed files to a folder or in place, with a report of every change.
    Fix,
    /// Show the files that would be read, their roles and their order, then exit.
    Inputs,
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Fix => "fix",
            Self::Inputs => "inputs",
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            report_error(&err);
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<ExitCode> {
    anyhow::bail!("`sqlca {}` is not implemented yet", cli.command.name())
}

fn report_error(err: &anyhow::Error) {
    // A failed write to stderr has nowhere left to be reported.
    drop(writeln!(io::stderr().lock(), "error: {err:#}"));
}
