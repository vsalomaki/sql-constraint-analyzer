//! The run as it goes, on stderr: a live line on a terminal, or a line per finished file elsewhere, and how long each
//! phase took, for the summary.

use std::io::Write;
use std::time::{Duration, Instant};

use crate::catalog::{Catalog, Kind, Parent, Table};
use crate::check::Observer;
use crate::dialect::Pass;

/// How the run shows its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// A line redrawn in place, for a terminal `width` columns wide; it is cut to fit, so it never wraps.
    Live { width: usize },
    /// A line per finished file, for a log.
    Plain,
    /// Nothing until the summary.
    Off,
}

/// How long a pass took and how much it read.
#[derive(Debug, Clone, Copy, Default)]
pub struct Phase {
    pub files: usize,
    pub bytes: u64,
    pub took: Duration,
}

/// What the summary shows about the phases.
#[derive(Debug, Clone, Default)]
pub struct Phases {
    pub schema: Phase,
    pub rows: Phase,
    pub resolve: Duration,
    /// The catalog, one line per table and key, when asked for.
    pub catalog: Vec<String>,
}

pub struct Progress<'a> {
    display: Display,
    list_catalog: bool,
    err: &'a mut dyn Write,
    phases: Phases,
    /// The stage of a fix, which takes the place of the check's `n/3`.
    stage: Option<String>,
    pass: Option<(Pass, Instant)>,
    resolving: Option<Instant>,
    /// Bytes of the pass read before the current file, the current file's name, size and start.
    done: u64,
    file: Option<(String, u64, Instant)>,
    offset: u64,
    drawn: Option<Instant>,
}

/// Redraws the live line at most this often.
const REDRAW: Duration = Duration::from_millis(100);

impl<'a> Progress<'a> {
    pub fn new(display: Display, list_catalog: bool, err: &'a mut dyn Write) -> Self {
        Self {
            display,
            list_catalog,
            err,
            phases: Phases::default(),
            stage: None,
            pass: None,
            resolving: None,
            done: 0,
            file: None,
            offset: 0,
            drawn: None,
        }
    }

    /// Ends the run's display and returns the phases for the summary.
    pub fn finish(mut self) -> Phases {
        self.end_file();
        self.end_pass();
        if let Some(started) = self.resolving.take() {
            self.phases.resolve = started.elapsed();
        }
        self.clear();
        self.phases
    }

    fn phase(&mut self, pass: Pass) -> &mut Phase {
        match pass {
            Pass::Schema => &mut self.phases.schema,
            Pass::Rows => &mut self.phases.rows,
        }
    }

    fn end_pass(&mut self) {
        if let Some((pass, started)) = self.pass.take() {
            self.phase(pass).took = started.elapsed();
        }
    }

    /// `[2/3]` in a check, `[check 2]` in a fix.
    fn step(&self) -> String {
        match (&self.stage, self.pass) {
            (Some(stage), _) => format!("[{stage}]"),
            (None, Some((Pass::Schema, _))) => "[1/3]".to_owned(),
            (None, _) => "[2/3]".to_owned(),
        }
    }

    /// Writes the finished file's line in plain display: a file the rows pass read, or one a stage outside any pass
    /// wrote.
    fn end_file(&mut self) {
        let Some((name, bytes, started)) = self.file.take() else {
            return;
        };
        self.done += bytes;
        let listed = match self.pass {
            Some((pass, _)) => pass == Pass::Rows,
            None => self.stage.is_some(),
        };
        if self.display == Display::Plain && listed {
            // A write that fails here has nowhere better to go; the summary will fail too.
            drop(writeln!(
                self.err,
                "{} {name}  {}  {}",
                self.step(),
                size(bytes),
                seconds(started.elapsed())
            ));
        }
    }

    fn clear(&mut self) {
        if matches!(self.display, Display::Live { .. }) && self.drawn.is_some() {
            drop(write!(self.err, "\r\x1b[2K"));
            drop(self.err.flush());
            self.drawn = None;
        }
    }

    fn draw(&mut self) {
        let Display::Live { width } = self.display else {
            return;
        };
        if self.drawn.is_some_and(|at| at.elapsed() < REDRAW) {
            return;
        }
        let name = self.file.as_ref().map_or("", |(n, _, _)| n.as_str());
        let name: String = name
            .chars()
            .rev()
            .take(24)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let line = match self.pass {
            Some((pass, started)) => self.bar(pass, started, &name),
            None if self.stage.is_some() => format!("{} {name}", self.step()),
            None => return,
        };
        let memory = memory_stats::memory_stats().map_or(String::new(), |memory| {
            format!(" · mem {}", size(memory.physical_mem as u64))
        });
        // One column short of the width, so the cursor never wraps to the next line; the memory is cut last.
        let room = width.saturating_sub(1);
        let line: String = line
            .chars()
            .take(room.saturating_sub(memory.chars().count()))
            .chain(memory.chars())
            .take(room)
            .collect();
        drop(write!(self.err, "\r\x1b[2K{line}"));
        drop(self.err.flush());
        self.drawn = Some(Instant::now());
    }

    /// `[2/3] Rows    ███░░░░░░░  30%  a.sql · 40.0 MB/s · ETA 12 s`, before the memory in use.
    fn bar(&self, pass: Pass, started: Instant, name: &str) -> String {
        let label = match pass {
            Pass::Schema => "Schema",
            Pass::Rows => "Rows",
        };
        let total = match pass {
            Pass::Schema => self.phases.schema.bytes,
            Pass::Rows => self.phases.rows.bytes,
        };
        let read = self.done + self.offset;
        let share = if total == 0 {
            1.0
        } else {
            ratio(read, total).min(1.0)
        };
        let filled = (share * 10.0).round();
        let bar: String = (0..10)
            .map(|i| if f64::from(i) < filled { '█' } else { '░' })
            .collect();
        let elapsed = started.elapsed().as_secs_f64();
        let rate = if elapsed > 0.0 {
            ratio(read, 1) / elapsed
        } else {
            0.0
        };
        let eta = if rate > 0.0 {
            format!(
                " · ETA {:.0} s",
                ratio(total.saturating_sub(read), 1) / rate
            )
        } else {
            String::new()
        };
        format!(
            "{} {label:<7} {bar} {:>3.0}%  {name} · {}/s{eta}",
            self.step(),
            share * 100.0,
            size(rate_bytes(rate)),
        )
    }
}

impl Observer for Progress<'_> {
    fn pass(&mut self, pass: Pass, files: usize, bytes: u64) {
        self.end_file();
        self.end_pass();
        *self.phase(pass) = Phase {
            files,
            bytes,
            took: Duration::ZERO,
        };
        self.pass = Some((pass, Instant::now()));
        self.done = 0;
        self.offset = 0;
    }

    fn file(&mut self, _index: usize, name: &str, bytes: u64) {
        self.end_file();
        self.file = Some((name.to_owned(), bytes, Instant::now()));
        self.offset = 0;
        self.draw();
    }

    fn advance(&mut self, offset: u64) {
        self.offset = offset;
        self.draw();
    }

    fn schema(&mut self, catalog: &Catalog) {
        if self.list_catalog {
            self.phases.catalog = listing(catalog);
        }
    }

    fn resolve(&mut self) {
        self.end_file();
        self.end_pass();
        self.resolving = Some(Instant::now());
    }

    fn stage(&mut self, stage: &str) {
        self.end_file();
        self.end_pass();
        self.clear();
        self.stage = Some(stage.to_owned());
        self.done = 0;
        self.offset = 0;
    }
}

fn ratio(a: u64, b: u64) -> f64 {
    // Exact below 2^53 bytes, which is far beyond any input.
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display ratio, exact below 2^53"
    )]
    let (a, b) = (a as f64, b as f64);
    if b == 0.0 { 0.0 } else { a / b }
}

fn rate_bytes(rate: f64) -> u64 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative display rate, rounded"
    )]
    let bytes = rate.max(0.0).round() as u64;
    bytes
}

/// `133.5 MB`: a size in decimal units, as the header and summary show it.
pub fn size(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = ratio(bytes, 1000);
    let mut unit = "kB";
    for next in ["MB", "GB", "TB"] {
        if value < 1000.0 {
            break;
        }
        value /= 1000.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

/// `827,620`: a count with thousands separators.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `3.8 s`.
pub fn seconds(took: Duration) -> String {
    format!("{:.1} s", took.as_secs_f64())
}

/// The catalog as `-v` shows it: each table with its column count, and each key with its columns, parent and state.
fn listing(catalog: &Catalog) -> Vec<String> {
    let mut lines = Vec::new();
    for table in catalog.tables() {
        lines.push(format!(
            "  {}  {} columns",
            catalog.display(&table.name),
            table.columns.len()
        ));
        for constraint in &table.constraints {
            let columns = |c: &[usize], t: &Table| {
                c.iter()
                    .filter_map(|&i| t.columns.get(i))
                    .map(|c| c.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let what = match &constraint.kind {
                Kind::PrimaryKey { columns: c } => format!("primary key ({})", columns(c, table)),
                Kind::Unique { columns: c, index } => {
                    let kind = if *index { "unique index" } else { "unique" };
                    format!("{kind} ({})", columns(c, table))
                }
                Kind::ForeignKey { columns: c, parent } => {
                    let parent = match parent {
                        Parent::Key {
                            table: id,
                            constraint,
                        } => catalog
                            .table(*id)
                            .map(|p| {
                                let key =
                                    p.constraints.get(*constraint).map_or(String::new(), |k| {
                                        match &k.kind {
                                            Kind::PrimaryKey { columns: c }
                                            | Kind::Unique { columns: c, .. } => columns(c, p),
                                            _ => String::new(),
                                        }
                                    });
                                format!("{} ({key})", catalog.display(&p.name))
                            })
                            .unwrap_or_default(),
                        Parent::Missing(name) => format!("{name}, not in the input"),
                    };
                    format!("foreign key ({}) → {parent}", columns(c, table))
                }
                Kind::NotNull { .. } | Kind::Check { .. } => continue,
            };
            let state = &constraint.state;
            let mut notes = Vec::new();
            if !state.enabled {
                notes.push("disabled");
            } else if !state.validated {
                notes.push("not validated");
            }
            if state.initially_deferred {
                notes.push("deferred");
            }
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!(", {}", notes.join(", "))
            };
            let name = constraint.name.as_deref().unwrap_or("(unnamed)");
            lines.push(format!("    {name}  {what}{notes}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_counts_read_like_people_write_them() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(40_854), "40.9 kB");
        assert_eq!(size(133_498_655), "133.5 MB");
        assert_eq!(size(2_200_000_000), "2.2 GB");
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(827_620), "827,620");
        assert_eq!(count(12_345_678), "12,345,678");
    }

    #[test]
    fn plain_display_writes_a_line_per_finished_data_file() {
        let mut out = Vec::new();
        let mut progress = Progress::new(Display::Plain, false, &mut out);
        progress.pass(Pass::Schema, 1, 10);
        progress.file(0, "t.sql", 10);
        progress.pass(Pass::Rows, 2, 2500);
        progress.file(0, "a.sql", 500);
        progress.advance(500);
        progress.file(1, "b.sql", 2000);
        progress.resolve();
        let phases = progress.finish();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.rsplit_once("  ").map_or(l, |(head, _)| head))
            .collect();
        assert_eq!(lines, ["[2/3] a.sql  500 B", "[2/3] b.sql  2.0 kB"]);
        assert_eq!(
            (phases.schema.files, phases.rows.files, phases.rows.bytes),
            (1, 2, 2500)
        );
    }

    #[test]
    fn a_fix_names_its_stage_on_each_line() {
        let mut out = Vec::new();
        let mut progress = Progress::new(Display::Plain, false, &mut out);
        progress.stage("check 1");
        progress.pass(Pass::Schema, 1, 10);
        progress.file(0, "a.sql", 10);
        progress.pass(Pass::Rows, 1, 10);
        progress.file(0, "a.sql", 10);
        progress.resolve();
        progress.stage("write");
        progress.file(0, "a.sql", 10);
        progress.stage("verify");
        progress.pass(Pass::Rows, 1, 8);
        progress.file(0, "a.sql", 8);
        progress.finish();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.rsplit_once("  ").map_or(l, |(head, _)| head))
            .collect();
        assert_eq!(
            lines,
            [
                "[check 1] a.sql  10 B",
                "[write] a.sql  10 B",
                "[verify] a.sql  8 B"
            ]
        );

        let mut out = Vec::new();
        let mut progress = Progress::new(Display::Live { width: 80 }, false, &mut out);
        progress.stage("check 2");
        progress.pass(Pass::Rows, 1, 10);
        progress.file(0, "a.sql", 10);
        progress.stage("write");
        progress.file(0, "b.sql", 10);
        progress.finish();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.split("\r\x1b[2K").filter(|l| !l.is_empty()).collect();
        assert!(
            lines.first().unwrap().starts_with("[check 2] Rows    "),
            "{text:?}"
        );
        let last = lines.last().unwrap();
        assert!(last.starts_with("[write] b.sql · mem "), "{text:?}");
        assert!(
            lines.iter().all(|l| l.contains(" · mem ")),
            "the memory in use ends every line: {text:?}"
        );
    }

    #[test]
    fn the_memory_stays_on_a_full_line() {
        let mut out = Vec::new();
        let mut progress = Progress::new(Display::Live { width: 80 }, false, &mut out);
        progress.stage("check 2");
        progress.pass(Pass::Rows, 1, 1000);
        progress.file(0, "app/data/some_long_table_name.sql", 1000);
        progress.finish();
        let text = String::from_utf8(out).unwrap();
        let line = text.split("\r\x1b[2K").find(|l| !l.is_empty()).unwrap();
        assert!(line.chars().count() <= 79, "{line:?}");
        let (_, memory) = line.rsplit_once(" · mem ").unwrap();
        let (number, unit) = memory.split_once(' ').unwrap();
        assert!(
            number.parse::<f64>().is_ok() && ["B", "kB", "MB", "GB", "TB"].contains(&unit),
            "the whole size shows: {line:?}"
        );
    }

    #[test]
    fn off_and_live_write_no_lines_of_their_own() {
        for display in [Display::Off, Display::Live { width: 80 }] {
            let mut out = Vec::new();
            let mut progress = Progress::new(display, false, &mut out);
            progress.pass(Pass::Rows, 1, 10);
            progress.file(0, "a.sql", 10);
            progress.advance(10);
            progress.finish();
            let text = String::from_utf8(out).unwrap();
            assert!(!text.contains('\n'), "{display:?}: {text:?}");
            if display != Display::Off {
                assert!(
                    text.ends_with("\r\x1b[2K"),
                    "the live line is cleared: {text:?}"
                );
            }
        }
    }

    #[test]
    fn the_live_line_fits_the_terminal() {
        let mut out = Vec::new();
        let mut progress = Progress::new(Display::Live { width: 40 }, false, &mut out);
        progress.pass(Pass::Rows, 1, 1000);
        progress.file(
            0,
            "a/very/long/path/to/a/data/file/with/a/long/name.sql",
            1000,
        );
        progress.finish();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.split("\r\x1b[2K").filter(|l| !l.is_empty()).collect();
        assert!(!lines.is_empty(), "{text:?}");
        for line in lines {
            assert!(line.chars().count() <= 39, "{line:?}");
        }
    }
}
