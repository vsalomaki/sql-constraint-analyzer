use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser;
use sqlca::dialect::DialectId;
use sqlca::synth::{self, Encoding, Layout, LineEnding, Noise, Options, Placement};

/// Writes SQL Developer–style Oracle exports with planted constraint violations, and a manifest of the expected
/// findings (`expected.json`).
#[derive(Debug, Parser)]
#[command(name = "sqlca-gen", version)]
struct Cli {
    /// Folder to write into; must not exist yet, or be empty.
    #[arg(long)]
    out: PathBuf,
    /// SQL dialect to write.
    #[arg(long, value_enum, default_value_t = DialectId::Oracle)]
    dialect: DialectId,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Number of employees; the other tables scale from it (about 10 rows per employee in all).
    #[arg(long, default_value_t = 100)]
    rows: u64,
    #[arg(long, value_enum, default_value_t = Layout::Separate)]
    layout: Layout,
    #[arg(long, value_enum, default_value_t = Placement::BeforeData)]
    constraints: Placement,
    /// Write the main schema's data out of dependency order.
    #[arg(long)]
    shuffle_order: bool,
    /// Rate of extra planted violations per normal row, on top of one of each kind.
    #[arg(long, default_value_t = 0.001, value_parser = parse_rate)]
    violations: f64,
    /// Plant no missing parent on the deferred foreign key (it would roll back a whole Oracle transaction).
    #[arg(long)]
    no_deferred_violations: bool,
    /// Presets `sqldev`, `all`, `none` and switch names, comma-separated.
    #[arg(long, default_value = "sqldev", value_parser = Noise::parse)]
    noise: Noise,
    #[arg(long, value_enum, default_value_t = LineEnding::Lf)]
    line_endings: LineEnding,
    #[arg(long, value_enum, default_value_t = Encoding::Utf8)]
    encoding: Encoding,
    /// Leave out a table's data, e.g. `APP.PROJECTS`; repeatable.
    #[arg(long)]
    omit_data: Vec<String>,

    /// Leave out the COMMIT after a table's rows, e.g. `APP.PROJECTS`; repeatable. Rows of the main schema that
    /// nothing commits after are a finding; the reference schema's files are reference files, which need none.
    #[arg(long)]
    omit_commit: Vec<String>,

    /// Rows per data file: once a data file has N rows, its rows go on in `<name>_2.sql`, `<name>_3.sql`, …, each
    /// part ending with the COMMIT a table's rows end with. Not with `--layout single`.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    rows_per_file: Option<u64>,

    /// Hang rows from bad rows: children, and theirs, that the database rejects once their parent is rejected,
    /// though they have no finding of their own, and duplicates of bad rows, which the database accepts. The manifest
    /// lists them. Not with `--omit-data`.
    #[arg(long)]
    cascades: bool,

    /// With `--cascades`, the length of the manager chain under each tree's employee: `fix` removes one of its rows
    /// per round.
    #[arg(long, value_name = "N", default_value_t = 3, requires = "cascades")]
    manager_chain: u64,
}

fn parse_rate(text: &str) -> Result<f64, String> {
    let rate: f64 = text
        .parse()
        .map_err(|_| format!("`{text}` is not a number"))?;
    if (0.0..=1.0).contains(&rate) {
        Ok(rate)
    } else {
        Err("must be between 0 and 1".into())
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // A failed write to stderr has nowhere left to be reported.
            drop(writeln!(io::stderr().lock(), "error: {err:#}"));
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the rate is checked to be within 0..=1, so the product fits a u32"
    )]
    let violations_ppm = (cli.violations * 1_000_000.0).round() as u32;
    let options = Options {
        dialect: cli.dialect,
        seed: cli.seed,
        rows: cli.rows,
        layout: cli.layout,
        constraints: cli.constraints,
        shuffle_order: cli.shuffle_order,
        violations_ppm,
        deferred_violations: !cli.no_deferred_violations,
        noise: cli.noise,
        line_ending: cli.line_endings,
        encoding: cli.encoding,
        omit_data: cli.omit_data,
        omit_commit: cli.omit_commit,
        rows_per_file: cli.rows_per_file,
        cascades: cli.cascades,
        manager_chain: if cli.cascades { cli.manager_chain } else { 0 },
    };
    let manifest = synth::generate(&options, &cli.out)
        .with_context(|| format!("writing {}", cli.out.display()))?;
    writeln!(
        io::stderr().lock(),
        "sqlca-gen: wrote {} SQL files with {} expected findings to {}",
        manifest.files.len(),
        manifest.findings.len(),
        cli.out.display()
    )?;
    Ok(())
}
