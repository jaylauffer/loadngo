//! Find in files: every line under a folder holding a piece of text.
//!
//! [`search`] reads files as they are saved on disk and blocks for as long
//! as the folder takes, so the app runs it on a thread of its own and stops
//! it early through [`Cancel`].

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most matches one search returns.
pub const MAX_MATCHES: usize = 5_000;
/// Files larger than this are skipped (generated data, logs).
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// A match line is shown up to this many characters.
const MAX_LINE_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub path: PathBuf,
    /// One-based line; one-based character columns of the match.
    pub line: usize,
    pub column: usize,
    pub end_column: usize,
    /// The line, trimmed, at most `MAX_LINE_CHARS`.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchOutcome {
    pub matches: Vec<SearchMatch>,
    pub files_searched: usize,
    /// Stopped at `MAX_MATCHES`.
    pub truncated: bool,
    pub cancelled: bool,
    pub elapsed: Duration,
}

/// Stops a running search.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Directories never searched: build output and repository internals, as
/// in the folder tree, plus any holding a `CACHEDIR.TAG`.
fn skipped_dir(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    name == "target" || name == ".git" || path.join("CACHEDIR.TAG").exists()
}

/// Every line under `root` holding `query`, in the folder tree's order
/// (subfolders first, then files, each by name). Case is ignored unless the
/// query has an uppercase letter.
pub fn search(root: &Path, query: &str, cancel: &Cancel) -> SearchOutcome {
    let started = Instant::now();
    let mut outcome = SearchOutcome::default();
    if !query.is_empty() {
        let ignore_case = !query.chars().any(char::is_uppercase);
        let needle = if ignore_case {
            query.to_lowercase()
        } else {
            query.to_string()
        };
        let mut walk = Walk {
            needle,
            ignore_case,
            cancel,
            buffer: Vec::new(),
            outcome: &mut outcome,
        };
        walk.dir(root);
    }
    outcome.elapsed = started.elapsed();
    outcome
}

struct Walk<'a> {
    needle: String,
    ignore_case: bool,
    cancel: &'a Cancel,
    /// Reused for every file read.
    buffer: Vec<u8>,
    outcome: &'a mut SearchOutcome,
}

impl Walk<'_> {
    /// False once the search should stop (cancelled or full).
    fn dir(&mut self, dir: &Path) -> bool {
        if self.cancel.is_cancelled() {
            self.outcome.cancelled = true;
            return false;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return true;
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => {
                    if !skipped_dir(&entry.path()) {
                        dirs.push(entry.path());
                    }
                }
                Ok(kind) if kind.is_file() => files.push(entry),
                _ => {}
            }
        }
        dirs.sort();
        files.sort_by_key(std::fs::DirEntry::file_name);
        for sub in dirs {
            if !self.dir(&sub) {
                return false;
            }
        }
        for entry in files {
            if !self.file(&entry) {
                return false;
            }
        }
        true
    }

    fn file(&mut self, entry: &fs::DirEntry) -> bool {
        if entry
            .metadata()
            .map_or(true, |meta| meta.len() > MAX_FILE_BYTES)
        {
            return true;
        }
        let path = entry.path();
        self.buffer.clear();
        if fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut self.buffer))
            .is_err()
            || self.buffer[..self.buffer.len().min(8192)].contains(&0)
        {
            return true;
        }
        let Ok(text) = std::str::from_utf8(&self.buffer) else {
            return true;
        };
        self.outcome.files_searched += 1;
        for (index, line) in text.lines().enumerate() {
            let haystack = if self.ignore_case {
                std::borrow::Cow::Owned(lowercase_same_length(line))
            } else {
                std::borrow::Cow::Borrowed(line)
            };
            let Some(byte) = haystack.find(&self.needle) else {
                continue;
            };
            let column = haystack[..byte].chars().count() + 1;
            self.outcome.matches.push(SearchMatch {
                path: path.clone(),
                line: index + 1,
                column,
                end_column: column + self.needle.chars().count(),
                text: line.trim_start().chars().take(MAX_LINE_CHARS).collect(),
            });
            if self.outcome.matches.len() >= MAX_MATCHES {
                self.outcome.truncated = true;
                return false;
            }
        }
        true
    }
}

/// Lowercases character by character, keeping characters whose lowercase
/// form is longer, so character positions stay the same.
fn lowercase_same_length(text: &str) -> String {
    text.chars()
        .map(|ch| {
            let mut lower = ch.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(single), None) => single,
                _ => ch,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        dir
    }

    #[test]
    fn finds_lines_in_path_order_and_skips_build_output_and_binaries() {
        let dir = tree(&[
            ("b.rs", b"fn helper() {}\n// Helper again\n"),
            ("a/deep.rs", b"    let x = helper();\n"),
            ("target/debug/out.rs", b"helper"),
            ("cache/CACHEDIR.TAG", b"Signature"),
            ("cache/x.rs", b"helper"),
            ("blob.bin", b"helper\0\0"),
        ]);
        let outcome = search(dir.path(), "helper", &Cancel::default());
        let found: Vec<_> = outcome
            .matches
            .iter()
            .map(|m| {
                (
                    m.path
                        .strip_prefix(dir.path())
                        .unwrap()
                        .display()
                        .to_string(),
                    m.line,
                    m.column,
                    m.text.clone(),
                )
            })
            .collect();
        assert_eq!(
            found,
            vec![
                (
                    "a/deep.rs".to_string(),
                    1,
                    13,
                    "let x = helper();".to_string()
                ),
                ("b.rs".to_string(), 1, 4, "fn helper() {}".to_string()),
                ("b.rs".to_string(), 2, 4, "// Helper again".to_string()),
            ]
        );
        assert_eq!(outcome.files_searched, 2);
    }

    #[test]
    fn an_uppercase_letter_makes_the_search_match_case() {
        let dir = tree(&[("a.rs", b"Helper\nhelper\n")]);
        let outcome = search(dir.path(), "Helper", &Cancel::default());
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(outcome.matches[0].line, 1);
    }

    #[test]
    fn a_cancelled_search_stops() {
        let dir = tree(&[("a.rs", b"x")]);
        let cancel = Cancel::default();
        cancel.cancel();
        assert!(search(dir.path(), "x", &cancel).cancelled);
    }
}
