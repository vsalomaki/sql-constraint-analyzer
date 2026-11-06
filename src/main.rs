use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand, ValueEnum};
use sqlca::check;
use sqlca::dialect::{DialectId, Runner};
use sqlca::input::{self, InputSpec, Resolved};
use sqlca::source::Encoding;
use sqlca::{fix, progress, report};

/// Checks Oracle SQL exports for rows that violate the declared constraints.
///
/// Findings and listings go to stdout; the header, progress, warnings and errors go to stderr.
/// Exit codes: 0 clean, 1 violations found, 2 usage, I/O or fatal errors.
#[derive(Debug, Parser)]
#[command(name = "sqlca", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report rows that violate constraints; never writes files.
    Check(CheckArgs),
    /// Write fixed files to a folder or in place, with a report of every change.
    Fix(FixArgs),
    /// Show the files that would be read, their roles and their order, then exit.
    Inputs(InputsArgs),
}

/// Where the SQL comes from. The order of the inputs is the load order, so it is always explicit: give one of
/// PATHS, --files-from or --script, not several.
#[derive(Debug, Args)]
struct InputArgs {
    /// Files or glob patterns, e.g. 'export/*.sql' 'data/**/*.sql'.
    ///
    /// Patterns are expanded by sqlca, not the shell, so quote them. `*`, `?`, `[...]` and `**` work. Each
    /// pattern's matches are version-sorted (`2_a.sql` before `10_a.sql`); patterns keep the order given. A
    /// pattern that matches nothing, or a missing file, is an error.
    #[arg(value_name = "PATHS", group = "form")]
    paths: Vec<String>,

    /// Read the file list from LIST, or from stdin with `-`: one path per line, read in that order.
    ///
    /// For an order a glob can't express, e.g. `find ... -print0 | sqlca inputs --files-from - -0`. Entries are
    /// literal paths, never globs, relative to the working directory; blank lines are skipped and CRLF line
    /// ends are accepted.
    #[arg(long, value_name = "LIST|-", group = "form")]
    files_from: Option<String>,

    /// The --files-from list is separated by NUL bytes instead of newlines.
    #[arg(short = '0', long, requires = "files_from", conflicts_with_all = ["paths", "script"])]
    null: bool,

    /// A SQL*Plus master script: its @file, @@file and START file lines are followed in order, recursively.
    ///
    /// `@` and START look in the working directory, then in each SQLPATH entry; `@@` looks beside the script that
    /// holds it; `.sql` is added to a name without an extension. The script itself is listed first, so its own
    /// statements load before what it includes. A script that includes itself is refused.
    #[arg(long, value_name = "FILE", group = "form")]
    script: Option<PathBuf>,

    /// DDL files or globs, counted as loaded before everything else.
    ///
    /// Give them after the other inputs, or write `--schema=FILE`: this option takes every value that follows.
    #[arg(long, value_name = "PATHS", num_args = 1..)]
    schema: Vec<String>,

    /// Another schema's DDL and data, files or globs: they supply parent keys and are not checked.
    ///
    /// Give them after the other inputs, or write `--reference=FILE`: this option takes every value that follows.
    #[arg(long, value_name = "PATHS", num_args = 1..)]
    reference: Vec<String>,

    /// The encoding of the input files. A UTF-8 byte-order mark is skipped; UTF-16 is refused.
    #[arg(long, value_name = "LABEL", value_enum, default_value_t)]
    encoding: Encoding,

    /// The program whose rules decide where statements end.
    ///
    /// sqlplus ends a statement at a line whose last character is `;`; sqlcl (and SQL Developer) at any `;`
    /// outside strings and comments.
    #[arg(long, value_enum, default_value_t)]
    runner: Runner,
}

impl InputArgs {
    fn spec(&self) -> InputSpec {
        InputSpec {
            paths: self.paths.clone(),
            files_from: self.files_from.clone(),
            null: self.null,
            script: self.script.clone(),
            schema: self.schema.clone(),
            reference: self.reference.clone(),
            dialect: DialectId::default(),
            runner: self.runner,
            encoding: self.encoding,
            sqlpath: std::env::var_os("SQLPATH")
                .map(|v| std::env::split_paths(&v).collect())
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[command(flatten)]
    input: InputArgs,

    /// The schema of unqualified table names. Folded like an unquoted name unless written in double quotes.
    ///
    /// Without it, an unqualified name means the only table of that name in any schema, and a name several
    /// schemas have is reported as ambiguous.
    #[arg(long, value_name = "NAME")]
    default_schema: Option<String>,

    /// Also report children inserted before their parent, judging rows in the order the files run.
    ///
    /// Without it, the check is of the end state: a parent anywhere in the input satisfies its children.
    #[arg(long)]
    load_order: bool,

    /// text (default), or JSON Lines: one finding per line, then a summary line.
    #[arg(long, value_enum, default_value_t)]
    format: report::Format,

    /// Show at most N findings per constraint (0: all); the counts stay exact.
    #[arg(long, value_name = "N", default_value_t = 100)]
    max_per_constraint: u64,

    /// How the run shows its progress on stderr: auto is a live line on a terminal and a line per data file
    /// otherwise; never shows only the summary.
    #[arg(long, value_enum, default_value_t)]
    progress: ProgressArg,

    /// Show only the result line on stderr.
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,

    /// Also list the catalog (tables, keys and their states) and every statement the check doesn't follow.
    #[arg(short, long)]
    verbose: bool,
}

impl CheckArgs {
    /// The default schema as the dialect stores it: quoted names as written, others folded.
    fn default_schema(&self, dialect: DialectId) -> Option<String> {
        let rules = dialect.rules();
        let name = self.default_schema.as_deref()?;
        let quote = rules.ident_quote;
        Some(
            name.strip_prefix(quote)
                .and_then(|n| n.strip_suffix(quote))
                .map_or_else(|| rules.fold(name), str::to_owned),
        )
    }

    fn verbosity(&self) -> report::Verbosity {
        if self.quiet {
            report::Verbosity::Quiet
        } else if self.verbose {
            report::Verbosity::Verbose
        } else {
            report::Verbosity::Normal
        }
    }
}

#[derive(Debug, Args)]
struct FixArgs {
    #[command(flatten)]
    check: CheckArgs,

    /// Write fixed copies under DIR, a new or empty folder, keeping the inputs' paths relative to the folder they
    /// share; the originals stay untouched. Unchanged files are hard links where the file system allows.
    #[arg(long, value_name = "DIR")]
    out: PathBuf,

    /// A row whose parent key exists nowhere in the input: drop it, or keep it.
    #[arg(long, value_enum, default_value_t)]
    on_fk: fix::OnRow,

    /// The later rows of a duplicate primary or unique key: drop them (keep-first), or keep them all.
    #[arg(long, value_enum, default_value_t)]
    on_duplicate: fix::OnDuplicate,

    /// A row with NULL in a NOT NULL column: drop it, or keep it.
    #[arg(long, value_enum, default_value_t)]
    on_not_null: fix::OnRow,
}

#[derive(Debug, Args)]
struct InputsArgs {
    #[command(flatten)]
    input: InputArgs,

    /// text (default), or JSON Lines: one object per file with position, role, path, bytes and order.
    #[arg(long, value_enum, default_value_t)]
    format: Format,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum ProgressArg {
    #[default]
    Auto,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum Format {
    #[default]
    Text,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli, &mut io::stdout().lock(), &mut io::stderr().lock()) {
        Ok(code) => code,
        Err(err) => {
            // A failed write to stderr has nowhere left to be reported.
            drop(writeln!(io::stderr().lock(), "error: {err:#}"));
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> anyhow::Result<ExitCode> {
    match &cli.command {
        Command::Inputs(args) => {
            let resolved = input::resolve(&args.input.spec(), &mut io::stdin().lock())?;
            write_header(&resolved, err)?;
            input::write_listing(&resolved.files, args.format == Format::Json, out)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Check(args) => check(args, out, err),
        Command::Fix(args) => fix(args, out, err),
    }
}

fn check(args: &CheckArgs, out: &mut dyn Write, err: &mut dyn Write) -> anyhow::Result<ExitCode> {
    let spec = args.input.spec();
    let resolved = input::resolve(&spec, &mut io::stdin().lock())?;
    resolved.require_data()?;
    let verbosity = args.verbosity();
    if verbosity != report::Verbosity::Quiet {
        let mode = if args.load_order {
            "load-order mode"
        } else {
            "end-state mode"
        };
        writeln!(err, "sqlca {} · check · {mode}", env!("CARGO_PKG_VERSION"))?;
        write_header(&resolved, err)?;
    }
    let options = check::Options {
        dialect: spec.dialect,
        runner: spec.runner,
        encoding: spec.encoding,
        default_schema: args.default_schema(spec.dialect),
        load_order: args.load_order,
    };
    let display = match (args.progress, verbosity) {
        (_, report::Verbosity::Quiet) | (ProgressArg::Never, _) => progress::Display::Off,
        (ProgressArg::Auto, _) if io::stderr().is_terminal() => progress::Display::Live {
            width: terminal_width(),
        },
        (ProgressArg::Auto, _) => progress::Display::Plain,
    };
    let mut progress =
        progress::Progress::new(display, verbosity == report::Verbosity::Verbose, err);
    let result = check::run_with(&resolved.files, &options, &mut progress)?;
    let phases = progress.finish();
    let omitted =
        report::write_findings(&result.findings, args.format, args.max_per_constraint, out)?;
    if args.format == report::Format::Json {
        report::write_json_summary(&result, &omitted, out)?;
    }
    report::write_run(&result, Some(&phases), &omitted, verbosity, err)?;
    Ok(ExitCode::from(report::exit_code(&result)))
}

/// The terminal's width from `COLUMNS` when the shell exports it, else 80.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&w| w > 0)
        .unwrap_or(80)
}

fn fix(args: &FixArgs, out: &mut dyn Write, err: &mut dyn Write) -> anyhow::Result<ExitCode> {
    let check = &args.check;
    let spec = check.input.spec();
    let resolved = input::resolve(&spec, &mut io::stdin().lock())?;
    resolved.require_data()?;
    let verbosity = check.verbosity();
    let quiet = verbosity == report::Verbosity::Quiet;
    if !quiet {
        let mode = if check.load_order {
            "load-order mode"
        } else {
            "end-state mode"
        };
        writeln!(err, "sqlca {} · fix · {mode}", env!("CARGO_PKG_VERSION"))?;
        write_header(&resolved, err)?;
        writeln!(
            err,
            "Output     fixed copies under {}, originals untouched",
            args.out.display()
        )?;
    }
    let options = check::Options {
        dialect: spec.dialect,
        runner: spec.runner,
        encoding: spec.encoding,
        default_schema: check.default_schema(spec.dialect),
        load_order: check.load_order,
    };
    let policy = fix::Policy {
        on_fk: args.on_fk,
        on_duplicate: args.on_duplicate,
        on_not_null: args.on_not_null,
    };
    let result = fix::fix(&resolved.files, &options, policy, &args.out)?;
    let folder = args.out.join(fix::REPORT_FOLDER);
    // A new folder only: on a case-insensitive file system an input's copied folder can already have this name.
    std::fs::create_dir(&folder).with_context(|| {
        format!(
            "cannot make the report folder {}: something in the output already has that name",
            folder.display()
        )
    })?;
    fix::write_file(&folder.join("rejects.sql"), |w| {
        fix::write_rejects(&result.removed, w)
    })?;
    fix::write_file(&folder.join("fix-report.txt"), |w| {
        fix::write_report(&result, &args.out, w)
    })?;

    let omitted = report::write_findings(
        &result.output.findings,
        check.format,
        check.max_per_constraint,
        out,
    )?;
    if check.format == report::Format::Json {
        report::write_json_summary(&result.output, &omitted, out)?;
    }
    if !quiet {
        writeln!(err, "{}", fix::summary(&result))?;
        for left in &result.left {
            writeln!(
                err,
                "warning: {}: left in place: {}",
                report::describe(&left.finding),
                left.why
            )?;
        }
        writeln!(err, "Report     {}", folder.display())?;
    }
    report::write_run(&result.output, None, &omitted, verbosity, err)?;
    Ok(ExitCode::from(report::exit_code(&result.output)))
}

fn write_header(resolved: &Resolved, err: &mut dyn Write) -> io::Result<()> {
    for warning in &resolved.warnings {
        writeln!(err, "warning: {warning}")?;
    }
    let count = |role| resolved.files.iter().filter(|f| f.role == role).count();
    let bytes: u64 = resolved.files.iter().map(|f| f.bytes).sum();
    writeln!(
        err,
        "Inputs     {} files, {}: {} schema, {} data, {} reference",
        resolved.files.len(),
        progress::size(bytes),
        count(input::Role::Schema),
        count(input::Role::Data),
        count(input::Role::Reference),
    )
}
