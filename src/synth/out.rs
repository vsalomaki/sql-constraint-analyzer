//! Output files: opened on first write, in load order, counting lines from the text actually written.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::finding::Location;

pub use crate::source::Encoding;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
}

impl LineEnding {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputFile {
    /// Relative to the output folder, with `/` separators.
    pub path: String,
    /// SQL statements, PL/SQL blocks and script commands in the file; comments and blank lines don't count.
    #[serde(default)]
    pub statements: u64,
}

struct Open {
    info: OutputFile,
    /// `None` once closed.
    writer: Option<BufWriter<File>>,
    /// The line the next character goes on.
    line: u64,
}

pub(crate) struct Out {
    root: PathBuf,
    encoding: Encoding,
    files: Vec<Open>,
    by_path: HashMap<String, usize>,
}

impl Out {
    pub(crate) fn new(root: &Path, encoding: Encoding) -> Self {
        Self {
            root: root.to_owned(),
            encoding,
            files: Vec::new(),
            by_path: HashMap::new(),
        }
    }

    /// Writes `text` to `path`, opening the file on first use; returns the line `text` starts on.
    pub(crate) fn write(
        &mut self,
        path: &str,
        text: &str,
        statements: u64,
    ) -> io::Result<Location> {
        let index = match self.by_path.get(path) {
            Some(&i) => i,
            None => self.open(path)?,
        };
        let encoding = self.encoding;
        let file = self
            .files
            .get_mut(index)
            .ok_or_else(|| io::Error::other(format!("output file {path} lost its slot")))?;
        let writer = file.writer.as_mut().ok_or_else(|| {
            io::Error::other(format!(
                "output file {path} was written after it was closed"
            ))
        })?;
        file.info.statements += statements;
        let at = Location {
            file: file.info.path.clone(),
            line: file.line,
        };
        match encoding {
            Encoding::Utf8 => writer.write_all(text.as_bytes())?,
            Encoding::Windows1252 => {
                let (bytes, _, unmappable) = encoding_rs::WINDOWS_1252.encode(text);
                if unmappable {
                    return Err(io::Error::other(format!(
                        "{path}: text has a character Windows-1252 can't encode"
                    )));
                }
                writer.write_all(&bytes)?;
            }
        }
        file.line += u64::try_from(text.bytes().filter(|&b| b == b'\n').count()).unwrap_or(0);
        Ok(at)
    }

    fn open(&mut self, path: &str) -> io::Result<usize> {
        let full = self.root.join(path);
        if let Some(dir) = full.parent() {
            fs::create_dir_all(dir)?;
        }
        let writer = BufWriter::new(File::create_new(&full)?);
        let info = OutputFile {
            path: path.to_owned(),
            statements: 0,
        };
        self.files.push(Open {
            info,
            writer: Some(writer),
            line: 1,
        });
        let index = self.files.len() - 1;
        self.by_path.insert(path.to_owned(), index);
        Ok(index)
    }

    /// Flushes and closes `path`, which takes no more writes; an export with many parts would otherwise run out of
    /// file handles.
    pub(crate) fn close(&mut self, path: &str) -> io::Result<()> {
        let writer = self
            .by_path
            .get(path)
            .and_then(|&i| self.files.get_mut(i))
            .and_then(|f| f.writer.take());
        match writer {
            Some(mut writer) => writer.flush(),
            None => Ok(()),
        }
    }

    /// Flushes every file and returns them in the order they were first written: the load order.
    pub(crate) fn finish(self) -> io::Result<Vec<OutputFile>> {
        self.files
            .into_iter()
            .map(|f| {
                if let Some(mut writer) = f.writer {
                    writer.flush()?;
                }
                Ok(f.info)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_count_from_the_text_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Out::new(dir.path(), Encoding::Utf8);
        assert_eq!(out.write("a/x.sql", "one\r\ntwo\n", 2).unwrap().line, 1);
        assert_eq!(out.write("a/x.sql", "'multi\nline';\n", 1).unwrap().line, 3);
        let files = out.finish().unwrap();
        assert_eq!(
            files,
            [OutputFile {
                path: "a/x.sql".to_owned(),
                statements: 3,
            }]
        );
    }

    #[test]
    fn a_closed_file_is_on_disk_and_takes_no_more_writes() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Out::new(dir.path(), Encoding::Utf8);
        out.write("x.sql", "one\n", 1).unwrap();
        out.close("x.sql").unwrap();
        assert_eq!(fs::read(dir.path().join("x.sql")).unwrap(), b"one\n");
        assert!(out.write("x.sql", "two\n", 1).is_err());
        assert_eq!(out.finish().unwrap().len(), 1);
    }

    #[test]
    fn windows_1252_output_refuses_characters_it_cannot_encode() {
        let dir = tempfile::tempdir().unwrap();
        let mut out = Out::new(dir.path(), Encoding::Windows1252);
        out.write("x.sql", "Mäkinen €\n", 0).unwrap();
        assert!(out.write("x.sql", "Ā\n", 0).is_err());
        out.finish().unwrap();
        assert_eq!(
            fs::read(dir.path().join("x.sql")).unwrap(),
            b"M\xe4kinen \x80\n"
        );
    }
}
