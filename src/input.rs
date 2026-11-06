//! The files a run reads: which, in what order, in what role. The order is the load order, so it is always explicit.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::dialect::{DialectId, Include, Runner};
use crate::source::Encoding;

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("no file matches the pattern '{0}'")]
    NoMatch(String),
    #[error("invalid pattern '{pattern}': {reason}")]
    BadPattern { pattern: String, reason: String },
    #[error("input file '{}' does not exist", .0.display())]
    Missing(PathBuf),
    #[error("'{}' is not a file", .0.display())]
    NotAFile(PathBuf),
    #[error("cannot read '{}'", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "'{}' looks like UTF-16 (or binary): it starts with a UTF-16 byte-order mark or has NUL bytes. \
         Convert it to UTF-8 or Windows-1252 first, e.g. `iconv -f UTF-16 -t UTF-8`",
        .0.display()
    )]
    Utf16(PathBuf),
    #[error("{list}: line {line}: not valid UTF-8")]
    ListNotUtf8 { list: String, line: usize },
    #[error("script '{}' includes itself: {chain}", script.display())]
    IncludeLoop { script: PathBuf, chain: String },
    #[error("{}:{line}: cannot find the script '{name}' (looked in {looked_in})", from.display())]
    IncludeNotFound {
        from: PathBuf,
        line: u64,
        name: String,
        looked_in: String,
    },
    #[error("no input files: give paths or patterns, --files-from or --script")]
    Empty,
    #[error(
        "no data files to check: give paths or patterns, --files-from or --script \
         (--schema and --reference files are read for their tables only)"
    )]
    NoData,
    #[error(
        "{again}: table {table} is created again; it was first created at {first} \
         (--folder-schema decides which folder names the schema of an unqualified name)"
    )]
    TableCreatedTwice {
        table: String,
        first: String,
        again: String,
    },
}

/// What a file is for. Schema files count as loaded first, then reference files, then the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Schema,
    Reference,
    Data,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Reference => "reference",
            Self::Data => "data",
        }
    }
}

/// Why a file sits where it does in the order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderSource {
    /// A path given as is.
    Given,
    GlobMatches,
    ListedIn(String),
    IncludedBy(PathBuf),
}

impl fmt::Display for OrderSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Given => f.write_str("as given"),
            Self::GlobMatches => f.write_str("version-sorted glob matches"),
            Self::ListedIn(list) => write!(f, "as listed in {list}"),
            Self::IncludedBy(script) => write!(f, "as included by {}", script.display()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFile {
    pub path: PathBuf,
    pub role: Role,
    pub bytes: u64,
    pub order: OrderSource,
}

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub struct InputSpec {
    /// Paths or glob patterns of the main inputs.
    pub paths: Vec<String>,
    /// A file of paths, or `-` for stdin.
    pub files_from: Option<String>,
    /// The list is NUL-separated, not newline-separated.
    pub null: bool,
    pub script: Option<PathBuf>,
    pub schema: Vec<String>,
    pub reference: Vec<String>,
    pub dialect: DialectId,
    pub runner: Runner,
    pub encoding: Encoding,
    /// Where `@file` also looks after the working directory (`SQLPATH`).
    pub sqlpath: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct Resolved {
    pub files: Vec<InputFile>,
    pub warnings: Vec<String>,
}

struct Candidate {
    path: PathBuf,
    role: Role,
    order: OrderSource,
}

/// The files of a run in processing order. `stdin` is read only for `--files-from -`.
impl Resolved {
    /// A run that checks rows needs data files; schema and reference files alone leave nothing to check.
    pub fn require_data(&self) -> Result<(), InputError> {
        if self.files.iter().any(|f| f.role == Role::Data) {
            Ok(())
        } else {
            Err(InputError::NoData)
        }
    }
}

pub fn resolve(spec: &InputSpec, stdin: &mut dyn Read) -> Result<Resolved, InputError> {
    let mut candidates = Vec::new();
    expand_patterns(&spec.schema, Role::Schema, &mut candidates)?;
    expand_patterns(&spec.reference, Role::Reference, &mut candidates)?;
    expand_patterns(&spec.paths, Role::Data, &mut candidates)?;
    if let Some(list) = &spec.files_from {
        let order = OrderSource::ListedIn(if list == "-" {
            "standard input".to_owned()
        } else {
            list.clone()
        });
        for path in read_list(list, spec.null, stdin)? {
            candidates.push(Candidate {
                path,
                role: Role::Data,
                order: order.clone(),
            });
        }
    }
    if let Some(script) = &spec.script {
        let mut paths = Vec::new();
        follow_script(
            script,
            spec,
            &mut Vec::new(),
            &mut HashSet::new(),
            &mut paths,
        )?;
        for path in paths {
            candidates.push(Candidate {
                path,
                role: Role::Data,
                order: OrderSource::IncludedBy(script.clone()),
            });
        }
    }
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let mut first_position = HashMap::new();
    for candidate in candidates {
        let key = fs::canonicalize(&candidate.path).map_err(|e| path_error(&candidate.path, e))?;
        if let Some(position) = first_position.get(&key) {
            warnings.push(format!(
                "'{}' is listed more than once; it is read once, at position {position}",
                candidate.path.display()
            ));
            continue;
        }
        let bytes = inspect(&candidate.path)?;
        files.push(InputFile {
            path: candidate.path,
            role: candidate.role,
            bytes,
            order: candidate.order,
        });
        first_position.insert(key, files.len());
    }
    if files.is_empty() {
        return Err(InputError::Empty);
    }
    Ok(Resolved { files, warnings })
}

fn path_error(path: &Path, source: io::Error) -> InputError {
    if source.kind() == io::ErrorKind::NotFound {
        InputError::Missing(path.to_owned())
    } else {
        InputError::Io {
            path: path.to_owned(),
            source,
        }
    }
}

fn has_glob_characters(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

fn expand_patterns(
    patterns: &[String],
    role: Role,
    out: &mut Vec<Candidate>,
) -> Result<(), InputError> {
    for pattern in patterns {
        if !has_glob_characters(pattern) {
            out.push(Candidate {
                path: PathBuf::from(pattern),
                role,
                order: OrderSource::Given,
            });
            continue;
        }
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        let walk = glob::glob_with(pattern, options).map_err(|e| InputError::BadPattern {
            pattern: pattern.clone(),
            reason: e.to_string(),
        })?;
        let mut matches = Vec::new();
        for entry in walk {
            let path = entry.map_err(|e| InputError::Io {
                path: e.path().to_owned(),
                source: io::Error::new(e.error().kind(), e.error().to_string()),
            })?;
            if path.is_file() {
                matches.push(path);
            }
        }
        if matches.is_empty() {
            return Err(InputError::NoMatch(pattern.clone()));
        }
        matches.sort_by(|a, b| version_cmp(&path_bytes(a), &path_bytes(b)));
        out.extend(matches.into_iter().map(|path| Candidate {
            path,
            role,
            order: OrderSource::GlobMatches,
        }));
    }
    Ok(())
}

fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

/// Compares like GNU `sort -V` (the Debian version order): digit runs compare as numbers; in the text between them
/// `~` sorts before everything, even the end, then letters, then other characters. Ties compare bytewise.
pub fn version_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (mut left, mut right) = (a, b);
    while !left.is_empty() || !right.is_empty() {
        let (text_l, rest_l) = split_at_digit(left, false);
        let (text_r, rest_r) = split_at_digit(right, false);
        let order = compare_text(text_l, text_r);
        if order != Ordering::Equal {
            return order;
        }
        let (run_l, rest_l) = split_at_digit(rest_l, true);
        let (run_r, rest_r) = split_at_digit(rest_r, true);
        let order = compare_numbers(run_l, run_r);
        if order != Ordering::Equal {
            return order;
        }
        (left, right) = (rest_l, rest_r);
    }
    a.cmp(b)
}

/// The leading run of digits (or of non-digits) and what follows it.
fn split_at_digit(s: &[u8], digits: bool) -> (&[u8], &[u8]) {
    let n = s
        .iter()
        .take_while(|b| b.is_ascii_digit() == digits)
        .count();
    s.split_at_checked(n).unwrap_or((s, &[]))
}

fn compare_text(a: &[u8], b: &[u8]) -> Ordering {
    let weight = |c: Option<&u8>| match c {
        None => 0,
        Some(b'~') => -1,
        Some(c) if c.is_ascii_alphabetic() => i32::from(*c),
        Some(c) => i32::from(*c) + 256,
    };
    (0..a.len().max(b.len()))
        .map(|i| weight(a.get(i)).cmp(&weight(b.get(i))))
        .find(|order| *order != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

fn compare_numbers(a: &[u8], b: &[u8]) -> Ordering {
    let significant = |s: &[u8]| {
        let zeros = s.iter().take_while(|&&c| c == b'0').count();
        s.get(zeros..).unwrap_or_default().to_vec()
    };
    let (a, b) = (significant(a), significant(b));
    a.len().cmp(&b.len()).then_with(|| a.cmp(&b))
}

/// The paths of a `--files-from` list, in order.
pub fn parse_list(bytes: &[u8], null: bool, list: &str) -> Result<Vec<PathBuf>, InputError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut paths = Vec::new();
    for (index, entry) in bytes
        .split(|&b| b == if null { 0 } else { b'\n' })
        .enumerate()
    {
        let entry = if null {
            entry
        } else {
            entry.strip_suffix(b"\r").unwrap_or(entry)
        };
        if entry.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let text = std::str::from_utf8(entry).map_err(|_| InputError::ListNotUtf8 {
            list: list.to_owned(),
            line: index + 1,
        })?;
        paths.push(PathBuf::from(text));
    }
    Ok(paths)
}

fn read_list(list: &str, null: bool, stdin: &mut dyn Read) -> Result<Vec<PathBuf>, InputError> {
    let mut bytes = Vec::new();
    if list == "-" {
        stdin
            .read_to_end(&mut bytes)
            .map_err(|source| InputError::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
    } else {
        bytes = fs::read(list).map_err(|e| path_error(Path::new(list), e))?;
    }
    let name = if list == "-" { "standard input" } else { list };
    parse_list(&bytes, null, name)
}

fn follow_script(
    script: &Path,
    spec: &InputSpec,
    stack: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) -> Result<(), InputError> {
    let canonical = fs::canonicalize(script).map_err(|e| path_error(script, e))?;
    if stack.contains(&canonical) {
        let chain = stack
            .iter()
            .chain([&canonical])
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(InputError::IncludeLoop {
            script: script.to_owned(),
            chain,
        });
    }
    // A repeated script is listed again, for the duplicate warning, but not followed again.
    out.push(script.to_owned());
    if !seen.insert(canonical.clone()) {
        return Ok(());
    }
    stack.push(canonical);
    let opened = open(script)?;
    let found = spec
        .dialect
        .script_includes(opened.reader, spec.runner, spec.encoding)
        .map_err(|source| InputError::Io {
            path: script.to_owned(),
            source,
        })?;
    for include in found {
        let target = find_include(script, &include, spec)?;
        follow_script(&target, spec, stack, seen, out)?;
    }
    stack.pop();
    Ok(())
}

fn find_include(script: &Path, include: &Include, spec: &InputSpec) -> Result<PathBuf, InputError> {
    let mut name = PathBuf::from(&include.name);
    if name.extension().is_none() {
        name.as_mut_os_string().push(".sql");
    }
    let mut dirs: Vec<PathBuf> = if include.beside_script {
        vec![script.parent().map(Path::to_owned).unwrap_or_default()]
    } else {
        std::iter::once(PathBuf::new())
            .chain(spec.sqlpath.iter().cloned())
            .collect()
    };
    if name.is_absolute() {
        dirs = vec![PathBuf::new()];
    }
    let candidates: Vec<PathBuf> = dirs.iter().map(|dir| dir.join(&name)).collect();
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| InputError::IncludeNotFound {
            from: script.to_owned(),
            line: include.line,
            name: name.display().to_string(),
            looked_in: candidates
                .iter()
                .map(|p| format!("'{}'", p.display()))
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// A file ready to be split: positioned after any byte-order mark.
pub struct Opened {
    pub reader: BufReader<File>,
    /// Where `reader` starts in the file as stored; hand it to `Splitter::starting_at`.
    pub base_offset: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum Head {
    Plain,
    Utf8Bom,
    Utf16,
}

const SNIFF_BYTES: usize = 4096;

fn sniff(head: &[u8]) -> Head {
    if head.starts_with(&[0xFF, 0xFE])
        || head.starts_with(&[0xFE, 0xFF])
        || head.iter().take(SNIFF_BYTES).any(|&b| b == 0)
    {
        Head::Utf16
    } else if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Head::Utf8Bom
    } else {
        Head::Plain
    }
}

pub fn open(path: &Path) -> Result<Opened, InputError> {
    let io_error = |source| InputError::Io {
        path: path.to_owned(),
        source,
    };
    let file = File::open(path).map_err(|e| path_error(path, e))?;
    let mut reader = BufReader::with_capacity(64 << 10, file);
    let bom_len: usize = match sniff(reader.fill_buf().map_err(io_error)?) {
        Head::Utf16 => return Err(InputError::Utf16(path.to_owned())),
        Head::Utf8Bom => 3,
        Head::Plain => 0,
    };
    reader.consume(bom_len);
    Ok(Opened {
        reader,
        base_offset: u64::try_from(bom_len).unwrap_or_default(),
    })
}

/// Checks that `path` is a readable file of a supported encoding, and gives its size.
fn inspect(path: &Path) -> Result<u64, InputError> {
    let meta = fs::metadata(path).map_err(|e| path_error(path, e))?;
    if !meta.is_file() {
        return Err(InputError::NotAFile(path.to_owned()));
    }
    open(path)?;
    Ok(meta.len())
}

/// The listing `sqlca inputs` prints: one line per file in processing order.
pub fn write_listing(files: &[InputFile], json: bool, out: &mut dyn Write) -> io::Result<()> {
    #[derive(Serialize)]
    struct Line<'a> {
        position: usize,
        role: Role,
        path: &'a str,
        bytes: u64,
        order: String,
    }
    for (index, file) in files.iter().enumerate() {
        let position = index + 1;
        let path = file.path.to_string_lossy();
        if json {
            let line = Line {
                position,
                role: file.role,
                path: &path,
                bytes: file.bytes,
                order: file.order.to_string(),
            };
            serde_json::to_writer(&mut *out, &line)?;
            writeln!(out)?;
        } else {
            writeln!(
                out,
                "{position:>4}  {:<9}  {path}  {} bytes  ({})",
                file.role.as_str(),
                file.bytes,
                file.order
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_sort_follows_sort_v() {
        let ordered = [
            "~z", "1", "2", "2_x", "9", "10", "10_x", "100", "A1", "V1.9", "V1.10", "V1.10a", "V2",
            "a~1.sql", "a0.sql", "a1.sql", "a2.sql", "ab.sql", "a-1.sql", "a_1.sql", "b", "z",
        ];
        // Same order as GNU sort -V 9.7 on this list, which differs from plain bytewise order.
        for pair in ordered.windows(2) {
            if let [a, b] = pair {
                assert_eq!(
                    version_cmp(a.as_bytes(), b.as_bytes()),
                    Ordering::Less,
                    "{a} < {b}"
                );
                assert_eq!(
                    version_cmp(b.as_bytes(), a.as_bytes()),
                    Ordering::Greater,
                    "{b} > {a}"
                );
            }
        }
        assert_eq!(version_cmp(b"a1", b"a1"), Ordering::Equal);
    }

    #[test]
    fn numerically_equal_runs_tie_bytewise() {
        assert_eq!(version_cmp(b"x01y", b"x1y"), Ordering::Less);
        assert_eq!(version_cmp(b"x1y", b"x01y"), Ordering::Greater);
    }

    fn list(bytes: &[u8], null: bool) -> Vec<String> {
        parse_list(bytes, null, "list")
            .unwrap()
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    #[test]
    fn lists_are_split_on_newlines_with_crlf_and_blank_lines_ignored() {
        assert_eq!(
            list(b"b.sql\r\n\r\n  \na dir/a.sql\n\nc.sql", false),
            ["b.sql", "a dir/a.sql", "c.sql"]
        );
        assert_eq!(list(b"\xEF\xBB\xBFa.sql\n", false), ["a.sql"]);
    }

    #[test]
    fn null_lists_keep_newlines_inside_names() {
        assert_eq!(list(b"a\nb.sql\0c.sql\0", true), ["a\nb.sql", "c.sql"]);
        assert_eq!(list(b"", true), Vec::<String>::new());
    }

    #[test]
    fn a_list_that_is_not_utf8_names_the_line() {
        let error = parse_list(b"a.sql\n\xFF.sql\n", false, "order.txt").unwrap_err();
        assert_eq!(error.to_string(), "order.txt: line 2: not valid UTF-8");
    }

    #[test]
    fn utf16_and_boms_are_told_apart_from_plain_text() {
        assert_eq!(sniff(b"\xFF\xFEi\0n\0"), Head::Utf16);
        assert_eq!(sniff(b"\xFE\xFF\0i"), Head::Utf16);
        assert_eq!(sniff(b"insert\0into"), Head::Utf16);
        assert_eq!(sniff(b"\xEF\xBB\xBFinsert"), Head::Utf8Bom);
        assert_eq!(sniff(b"insert into"), Head::Plain);
        assert_eq!(sniff(b""), Head::Plain);
    }

    #[test]
    fn a_nul_after_the_sniffed_head_is_not_looked_for() {
        let mut text = vec![b'a'; SNIFF_BYTES];
        text.push(0);
        assert_eq!(sniff(&text), Head::Plain);
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    fn script_spec(script: PathBuf) -> InputSpec {
        InputSpec {
            script: Some(script),
            ..InputSpec::default()
        }
    }

    fn names(resolved: &Resolved, base: &Path) -> Vec<String> {
        resolved
            .files
            .iter()
            .map(|f| {
                f.path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    fn resolve_script(spec: &InputSpec) -> Result<Resolved, InputError> {
        resolve(spec, &mut io::empty())
    }

    #[test]
    fn scripts_are_followed_recursively_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let master = write(base, "load.sql", "@@ddl\n@@sub/main.sql\n@@last.sql\n");
        write(base, "ddl.sql", "create table t (a number);\n");
        write(base, "sub/main.sql", "@@part\n");
        write(base, "sub/part.sql", "insert into t values (1);\n");
        write(base, "last.sql", "");
        let mut spec = script_spec(master.clone());
        spec.sqlpath = vec![];
        let resolved = resolve_script(&spec).unwrap();
        assert_eq!(
            names(&resolved, base),
            [
                "load.sql",
                "ddl.sql",
                "sub/main.sql",
                "sub/part.sql",
                "last.sql"
            ]
        );
        assert!(resolved.files.iter().all(|f| f.role == Role::Data
            && f.order == OrderSource::IncludedBy(master.clone())));
    }

    #[test]
    fn at_looks_in_sqlpath_and_double_at_beside_the_script() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let master = write(base, "s/load.sql", "@@near\nstart shared\n");
        write(base, "s/near.sql", "");
        write(base, "lib/shared.sql", "");
        let mut spec = script_spec(master.clone());
        spec.sqlpath = vec![base.join("lib")];
        let resolved = resolve_script(&spec).unwrap();
        assert_eq!(
            names(&resolved, base),
            ["s/load.sql", "s/near.sql", "lib/shared.sql"]
        );

        spec.sqlpath = vec![];
        let error = resolve_script(&spec).unwrap_err().to_string();
        assert!(
            error.contains(":2:") && error.contains("shared.sql"),
            "{error}"
        );

        let master = write(base, "t/load.sql", "@@shared\n");
        write(base, "shared.sql", "");
        let error = resolve_script(&script_spec(master))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("cannot find the script 'shared.sql'"),
            "{error}"
        );
    }

    #[test]
    fn an_include_loop_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let a = write(base, "a.sql", "@@b\n");
        write(base, "b.sql", "@@c\n");
        write(base, "c.sql", "@@a\n");
        let error = resolve_script(&script_spec(a)).unwrap_err().to_string();
        assert!(error.contains("includes itself"), "{error}");
        let direct = write(base, "d.sql", "@@d\n");
        assert!(resolve_script(&script_spec(direct)).is_err());
    }

    #[test]
    fn a_file_included_twice_is_read_once_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let master = write(base, "load.sql", "@@x\n@@x\n");
        write(base, "x.sql", "");
        let resolved = resolve_script(&script_spec(master)).unwrap();
        assert_eq!(names(&resolved, base), ["load.sql", "x.sql"]);
        assert_eq!(resolved.warnings.len(), 1);
    }

    #[test]
    fn repeated_includes_are_not_followed_again() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        for n in 0..40 {
            let next = n + 1;
            write(
                base,
                &format!("s{n}.sql"),
                &format!("@@s{next}\n@@s{next}\n"),
            );
        }
        write(base, "s40.sql", "");
        let resolved = resolve_script(&script_spec(base.join("s0.sql"))).unwrap();
        assert_eq!(resolved.files.len(), 41);
        assert_eq!(resolved.warnings.len(), 40);
    }

    #[test]
    fn open_skips_a_utf8_bom_and_refuses_utf16() {
        let dir = tempfile::tempdir().unwrap();
        let bom = dir.path().join("bom.sql");
        fs::write(&bom, b"\xEF\xBB\xBFselect 1;\n").unwrap();
        let opened = open(&bom).unwrap();
        assert_eq!(opened.base_offset, 3);
        let mut rest = String::new();
        let mut reader = opened.reader;
        reader.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "select 1;\n");

        let wide = dir.path().join("wide.sql");
        fs::write(&wide, b"\xFF\xFEs\0e\0").unwrap();
        assert!(matches!(open(&wide), Err(InputError::Utf16(_))));
    }
}
