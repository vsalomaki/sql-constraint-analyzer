use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
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
#[expect(clippy::struct_excessive_bools, reason = "each is a command-line flag")]
struct CheckArgs {
    #[command(flatten)]
    input: InputArgs,

    #[command(flatten)]
    names: NameArgs,

    /// Also report children inserted before their parent, judging rows in the order the files run.
    ///
    /// Without it, the check is of the end state: a parent anywhere in the input satisfies its children.
    #[arg(long)]
    load_order: bool,

    /// Don't report rows at the end of a data file that no commit follows.
    ///
    /// By default, every data file's rows must be followed by a COMMIT, or by a statement that commits, such as
    /// DDL in Oracle. `fix` adds a COMMIT at the end of a file that lacks one, unless a constraint is checked only at
    /// commit (deferred): then an early COMMIT could roll the load back, so it leaves the finding for review.
    #[arg(long)]
    no_commit_check: bool,

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

/// How unqualified table names find their table.
#[derive(Debug, Args)]
struct NameArgs {
    /// The schema of unqualified table names. Folded like an unquoted name unless written in double quotes.
    ///
    /// Without it, an unqualified name means its folder's schema (see --folder-schema) when that has the table, else
    /// the only table of that name in any schema; a name several schemas have is reported as ambiguous.
    #[arg(long, value_name = "NAME")]
    default_schema: Option<String>,

    /// Which folder names the schema of an unqualified table name, below the folder all inputs share.
    ///
    /// `a/x.sql` with `immediate` and `a/tables/x.sql` with `top` are in schema A; a file in the shared folder itself
    /// has no folder schema. A folder name that couldn't be written unquoted is the schema's name as written.
    #[arg(long, value_enum, default_value_t)]
    folder_schema: check::FolderSchema,
}

impl NameArgs {
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

    /// The check's options for these inputs, with the rest at their defaults.
    fn options(&self, spec: &InputSpec) -> check::Options {
        check::Options {
            dialect: spec.dialect,
            runner: spec.runner,
            encoding: spec.encoding,
            default_schema: self.default_schema(spec.dialect),
            folder_schema: self.folder_schema,
            ..check::Options::default()
        }
    }
}

impl CheckArgs {
    fn options(&self, spec: &InputSpec) -> check::Options {
        check::Options {
            load_order: self.load_order,
            commit_check: !self.no_commit_check,
            ..self.names.options(spec)
        }
    }

    /// `sqlca <version> · <command> · <mode>`, then the inputs.
    fn write_mode_header(
        &self,
        command: &str,
        resolved: &Resolved,
        err: &mut dyn Write,
    ) -> io::Result<()> {
        let mode = if self.load_order {
            "load-order mode"
        } else {
            "end-state mode"
        };
        writeln!(
            err,
            "sqlca {} · {command} · {mode}",
            env!("CARGO_PKG_VERSION")
        )?;
        write_header(resolved, err)
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
#[command(group(ArgGroup::new("destination").required(true).args(["out", "in_place"])))]
#[expect(clippy::struct_excessive_bools, reason = "each is a command-line flag")]
struct FixArgs {
    #[command(flatten)]
    check: CheckArgs,

    /// Write fixed copies under DIR, a new or empty folder, keeping the inputs' paths relative to the folder they
    /// share; the originals stay untouched. Unchanged files are hard links where the file system allows.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// Rewrite the input files themselves. Each changed file is written beside the original and renamed over it,
    /// so an interrupted run leaves the old file or the new one, never half of each.
    #[arg(long)]
    in_place: bool,

    /// With --in-place: keep each changed original as <file><SUFFIX> first (default .orig).
    #[arg(
        long,
        value_name = "SUFFIX",
        value_parser = backup_suffix,
        conflicts_with_all = ["out", "no_backup"]
    )]
    backup: Option<String>,

    /// With --in-place: keep no backups.
    #[arg(long, conflicts_with = "out")]
    no_backup: bool,

    /// Run the fix and print the report it would write to stdout (JSON with --format json); write nothing.
    #[arg(long)]
    dry_run: bool,

    /// Where rejects.sql and the fix reports go, a new or empty folder. Default: DIR/sqlca-report with --out,
    /// ./sqlca-fix-<UTC time> with --in-place.
    #[arg(long, value_name = "DIR", conflicts_with = "no_report")]
    report_dir: Option<PathBuf>,

    /// Write no report folder. Refused with --in-place --no-backup: the removed rows would be kept nowhere.
    #[arg(long)]
    no_report: bool,

    /// A row whose parent key exists nowhere in the input: drop it, set the foreign key's columns to NULL, or keep
    /// it.
    ///
    /// With null, a row is still dropped when a column of the key can't hold NULL (NOT NULL, part of the primary key,
    /// an identity or DEFAULT ON NULL column) or the row leaves it out, so its default applies; the report says which.
    #[arg(long, value_enum, default_value_t)]
    on_fk: fix::OnFk,

    /// The rows of a duplicate primary or unique key: drop the later ones (keep-first), drop all but the last in the
    /// order the files run (keep-last), or keep them all.
    #[arg(long, value_enum, default_value_t)]
    on_duplicate: fix::OnDuplicate,

    /// A row with NULL in a NOT NULL column: drop it, or keep it.
    #[arg(long, value_enum, default_value_t)]
    on_not_null: fix::OnRow,

    /// Also act on findings whose key rests on an assumption: a date written as plain text, read as YYYY-MM-DD.
    ///
    /// Off by default: such findings are left in place and listed for review, since the text may mean another date.
    #[arg(long)]
    fix_assumed: bool,

    /// Stop after N rounds that remove rows. Each round takes one more level of a foreign-key chain; what a further
    /// round would remove is left in place and listed for review.
    #[arg(long, value_name = "N", default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..))]
    max_rounds: u16,

    /// Leave a one-line comment where each row or statement was removed, saying why and pointing to rejects.sql.
    #[arg(long)]
    annotate: bool,
}

impl FixArgs {
    fn target(&self) -> fix::Target {
        let destination = match &self.out {
            Some(out) => fix::Destination::Out(out.clone()),
            None => fix::Destination::InPlace {
                backup: (!self.no_backup)
                    .then(|| self.backup.clone().unwrap_or_else(|| ".orig".to_owned())),
            },
        };
        let report = match (&self.report_dir, self.no_report) {
            (_, true) => fix::ReportPlace::Off,
            (Some(dir), false) => fix::ReportPlace::At(dir.clone()),
            (None, false) => fix::ReportPlace::Default,
        };
        fix::Target {
            destination,
            report,
            dry_run: self.dry_run,
            annotate: self.annotate,
        }
    }
}

/// A backup suffix makes a new name in the same folder.
fn backup_suffix(suffix: &str) -> Result<String, String> {
    if suffix.is_empty() || suffix.contains(['/', '\\']) {
        return Err("give a suffix such as .orig: not empty, without / or \\".to_owned());
    }
    Ok(suffix.to_owned())
}

#[derive(Debug, Args)]
struct InputsArgs {
    #[command(flatten)]
    input: InputArgs,

    #[command(flatten)]
    names: NameArgs,

    /// List the data files parents-first: each after the data files holding rows its foreign keys point at.
    ///
    /// Reads every data file. A file with table DDL keeps its place and no file moves across it, as where a
    /// constraint is declared decides which rows it judges. Files that don't depend on each other keep the order
    /// given; a cycle is broken at the earliest file given in it. A key whose parent rows still come later is named in
    /// a warning. A foreign key that is disabled, checked only at commit, or points at its own table doesn't order
    /// files. A statement it can't read is an error and exits 1, as the order may miss what it holds; a file it can't
    /// read to the end exits 2.
    #[arg(long)]
    dependency_order: bool,

    /// text (default); JSON Lines, one object per file with position, role, path, bytes and order; or paths, the
    /// data files' paths alone, one per line, for --files-from (give --schema and --reference files again).
    #[arg(long, value_enum, default_value_t)]
    format: Format,

    /// Like --format paths, with each path ended by a NUL byte, for --files-from - -0.
    #[arg(long, conflicts_with = "format")]
    print0: bool,
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
    Paths,
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
        Command::Inputs(args) => inputs(args, out, err),
        Command::Check(args) => check(args, out, err),
        Command::Fix(args) => fix(args, out, err),
    }
}

fn inputs(args: &InputsArgs, out: &mut dyn Write, err: &mut dyn Write) -> anyhow::Result<ExitCode> {
    let spec = args.input.spec();
    let mut resolved = input::resolve(&spec, &mut io::stdin().lock())?;
    write_header(&resolved, err)?;
    let mut code = 0;
    if args.dependency_order {
        resolved.require_data()?;
        let display = if io::stderr().is_terminal() {
            progress::Display::Live {
                width: terminal_width(),
            }
        } else {
            progress::Display::Off
        };
        let mut progress = progress::Progress::new(display, false, err);
        let ordered =
            check::dependency_order(&resolved.files, &args.names.options(&spec), &mut progress);
        progress.finish();
        let ordered = ordered?;
        for error in ordered.unreadable.iter().chain(&ordered.errors) {
            writeln!(err, "error: {}", report::problem(error))?;
        }
        if !ordered.unreadable.is_empty() {
            return Ok(ExitCode::from(2));
        }
        for issue in &ordered.catalog {
            writeln!(err, "warning: {}", report::problem(issue))?;
        }
        for unmet in &ordered.unmet {
            writeln!(err, "warning: {unmet}")?;
        }
        resolved.files = ordered.files;
        code = u8::from(!ordered.errors.is_empty());
    }
    let listing = match (args.print0, args.format) {
        (true, _) => input::Listing::Paths { end: b'\0' },
        (false, Format::Paths) => input::Listing::Paths { end: b'\n' },
        (false, Format::Json) => input::Listing::Json,
        (false, Format::Text) => input::Listing::Text,
    };
    input::write_listing(&resolved.files, listing, out)?;
    Ok(ExitCode::from(code))
}

fn check(args: &CheckArgs, out: &mut dyn Write, err: &mut dyn Write) -> anyhow::Result<ExitCode> {
    let spec = args.input.spec();
    let resolved = input::resolve(&spec, &mut io::stdin().lock())?;
    resolved.require_data()?;
    let verbosity = args.verbosity();
    if verbosity != report::Verbosity::Quiet {
        args.write_mode_header("check", &resolved, err)?;
    }
    let options = args.options(&spec);
    let mut progress = progress::Progress::new(
        display(args.progress, verbosity),
        verbosity == report::Verbosity::Verbose,
        err,
    );
    let result = check::run_with(&resolved.files, &options, &mut progress);
    let phases = progress.finish();
    let result = result?;
    let omitted =
        report::write_findings(&result.findings, args.format, args.max_per_constraint, out)?;
    if args.format == report::Format::Json {
        report::write_json_summary(&result, &omitted, out)?;
    }
    report::write_run(&result, Some(&phases), &omitted, verbosity, err)?;
    Ok(ExitCode::from(report::exit_code(&result)))
}

fn display(progress: ProgressArg, verbosity: report::Verbosity) -> progress::Display {
    match (progress, verbosity) {
        (_, report::Verbosity::Quiet) | (ProgressArg::Never, _) => progress::Display::Off,
        (ProgressArg::Auto, _) if io::stderr().is_terminal() => progress::Display::Live {
            width: terminal_width(),
        },
        (ProgressArg::Auto, _) => progress::Display::Plain,
    }
}

/// The width of the terminal stderr writes to, else `COLUMNS` when the shell exports it, else 80.
fn terminal_width() -> usize {
    terminal_size::terminal_size_of(io::stderr())
        .map(|(width, _)| usize::from(width.0))
        .or_else(|| std::env::var("COLUMNS").ok()?.parse().ok())
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
    let target = args.target();
    if !quiet {
        check.write_mode_header("fix", &resolved, err)?;
        writeln!(err, "Output     {}", fix::destination_text(&target))?;
        if target.dry_run {
            writeln!(
                err,
                "Dry run    nothing is written; the report goes to stdout"
            )?;
        }
    }
    let options = check.options(&spec);
    let policy = fix::Policy {
        on_fk: args.on_fk,
        on_duplicate: args.on_duplicate,
        on_not_null: args.on_not_null,
        fix_assumed: args.fix_assumed,
        max_rounds: usize::from(args.max_rounds),
    };
    let mut progress = progress::Progress::new(display(check.progress, verbosity), false, err);
    let result = fix::fix_with(&resolved.files, &options, policy, &target, &mut progress);
    progress.finish();
    let result = result?;

    let omitted = if target.dry_run {
        match check.format {
            report::Format::Text => fix::write_report(&result, out)?,
            report::Format::Json => fix::write_json_report(&result, out)?,
        }
        report::Omitted::default()
    } else {
        let omitted = report::write_findings(
            &result.output.findings,
            check.format,
            check.max_per_constraint,
            out,
        )?;
        if check.format == report::Format::Json {
            report::write_json_summary(&result.output, &omitted, out)?;
        }
        omitted
    };
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
        if let Some(folder) = &result.report {
            let would = if target.dry_run { " (would be)" } else { "" };
            writeln!(err, "Report     {}{would}", folder.display())?;
        }
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
