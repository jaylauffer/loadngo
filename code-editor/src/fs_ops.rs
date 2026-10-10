//! The editor's filesystem work. Every function here blocks, so the app runs
//! them on the host's offload workers ([`perform`]), never on a frame.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::text_file::{self, DecodedText, LineEnding, NotText};

/// What identifies one version of a file on disk, to notice a change made
/// by something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

impl DiskStamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        }
    }
}

/// One child of a directory in the file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Work the editor asks for. Each request carries the editor's id for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoRequest {
    ListDir {
        dir: PathBuf,
    },
    ReadFile {
        path: PathBuf,
    },
    /// Writes `bytes` unless the file on disk no longer matches `expected`
    /// (then nothing is written and the result says so).
    WriteFile {
        path: PathBuf,
        bytes: Vec<u8>,
        expected: Option<DiskStamp>,
        revision: u64,
    },
    /// Writes `bytes` with no check, creating parent directories; for the
    /// editor's own session and backup files.
    WriteState {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    RemoveState {
        path: PathBuf,
    },
    /// Reads one of the editor's own files; a missing file is `None`.
    ReadState {
        path: PathBuf,
    },
    StatFiles {
        paths: Vec<PathBuf>,
    },
}

/// What came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoResponse {
    Listed {
        dir: PathBuf,
        entries: Result<Vec<DirEntry>, String>,
    },
    Read {
        path: PathBuf,
        result: Result<(DecodedText, DiskStamp), ReadError>,
    },
    Written {
        path: PathBuf,
        revision: u64,
        result: Result<DiskStamp, WriteError>,
    },
    StateWritten {
        path: PathBuf,
        result: Result<(), String>,
    },
    StateRead {
        path: PathBuf,
        result: Result<Option<Vec<u8>>, String>,
    },
    Stats {
        stamps: Vec<(PathBuf, Option<DiskStamp>)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Io(String),
    NotText(NotText),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(message) => f.write_str(message),
            ReadError::NotText(reason) => write!(f, "{reason}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    Io(String),
    /// The file changed (or vanished) on disk since the editor read it.
    ChangedOnDisk,
}

/// Runs `request`. Blocks; call it on a worker.
pub fn perform(request: IoRequest) -> IoResponse {
    match request {
        IoRequest::ListDir { dir } => {
            let entries = list_dir(&dir).map_err(|error| error.to_string());
            IoResponse::Listed { dir, entries }
        }
        IoRequest::ReadFile { path } => {
            let result = read_file(&path);
            IoResponse::Read { path, result }
        }
        IoRequest::WriteFile {
            path,
            bytes,
            expected,
            revision,
        } => {
            let result = write_file(&path, &bytes, expected);
            IoResponse::Written {
                path,
                revision,
                result,
            }
        }
        IoRequest::WriteState { path, bytes } => {
            let result = path
                .parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| loadngo_persistence::replace_atomically(&path, &bytes))
                .map_err(|error| error.to_string());
            IoResponse::StateWritten { path, result }
        }
        IoRequest::RemoveState { path } => {
            let result = match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.to_string()),
                _ => Ok(()),
            };
            IoResponse::StateWritten { path, result }
        }
        IoRequest::ReadState { path } => {
            let result = match fs::read(&path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.to_string()),
            };
            IoResponse::StateRead { path, result }
        }
        IoRequest::StatFiles { paths } => IoResponse::Stats {
            stamps: paths
                .into_iter()
                .map(|path| {
                    let stamp = fs::metadata(&path).ok().map(|meta| DiskStamp::of(&meta));
                    (path, stamp)
                })
                .collect(),
        },
    }
}

/// Directory names the tree never shows: build output and repository
/// internals. Any directory holding a `CACHEDIR.TAG` is skipped as well.
const HIDDEN_DIRS: &[&str] = &["target", ".git"];

/// The children of `dir` the tree shows: directories first, then files,
/// each in case-insensitive name order.
pub fn list_dir(dir: &Path) -> io::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Follow symlinks so a linked directory browses like a directory.
        let is_dir = fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir());
        if is_dir
            && (HIDDEN_DIRS.contains(&name.as_str()) || entry.path().join("CACHEDIR.TAG").exists())
        {
            continue;
        }
        if name == ".DS_Store" {
            continue;
        }
        entries.push(DirEntry { name, is_dir });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

pub fn read_file(path: &Path) -> Result<(DecodedText, DiskStamp), ReadError> {
    let metadata = fs::metadata(path).map_err(|error| ReadError::Io(error.to_string()))?;
    let bytes = fs::read(path).map_err(|error| ReadError::Io(error.to_string()))?;
    let decoded = text_file::decode(bytes).map_err(ReadError::NotText)?;
    Ok((decoded, DiskStamp::of(&metadata)))
}

pub fn write_file(
    path: &Path,
    bytes: &[u8],
    expected: Option<DiskStamp>,
) -> Result<DiskStamp, WriteError> {
    if let Some(expected) = expected {
        let current = fs::metadata(path).ok().map(|meta| DiskStamp::of(&meta));
        if current != Some(expected) {
            return Err(WriteError::ChangedOnDisk);
        }
    }
    loadngo_persistence::replace_atomically(path, bytes)
        .map_err(|error| WriteError::Io(error.to_string()))?;
    let metadata = fs::metadata(path).map_err(|error| WriteError::Io(error.to_string()))?;
    Ok(DiskStamp::of(&metadata))
}

/// Encodes `text` for writing with the file's line ending.
pub fn file_bytes(text: &str, line_ending: LineEnding) -> Vec<u8> {
    text_file::encode(text, line_ending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_puts_directories_first_and_hides_build_output() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::create_dir(dir.path().join("cache")).unwrap();
        fs::write(dir.path().join("cache/CACHEDIR.TAG"), "Signature").unwrap();
        fs::write(dir.path().join("b.rs"), "").unwrap();
        fs::write(dir.path().join("A.md"), "").unwrap();
        let names: Vec<_> = list_dir(dir.path())
            .unwrap()
            .into_iter()
            .map(|entry| (entry.name, entry.is_dir))
            .collect();
        assert_eq!(
            names,
            vec![
                ("src".to_string(), true),
                ("A.md".to_string(), false),
                ("b.rs".to_string(), false)
            ]
        );
    }

    #[test]
    fn a_write_is_refused_once_the_file_changed_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        fs::write(&path, "one").unwrap();
        let (_, stamp) = read_file(&path).unwrap();
        fs::write(&path, "other program").unwrap();
        assert_eq!(
            write_file(&path, b"mine", Some(stamp)),
            Err(WriteError::ChangedOnDisk)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "other program");
        let (_, fresh) = read_file(&path).unwrap();
        let written = write_file(&path, b"mine", Some(fresh)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine");
        assert_eq!(written.len, 4);
    }

    #[test]
    fn state_files_are_created_with_their_directory_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("backups/x.json");
        let written = perform(IoRequest::WriteState {
            path: path.clone(),
            bytes: b"{}".to_vec(),
        });
        assert!(matches!(
            written,
            IoResponse::StateWritten { result: Ok(()), .. }
        ));
        let read = perform(IoRequest::ReadState { path: path.clone() });
        assert!(
            matches!(read, IoResponse::StateRead { result: Ok(Some(ref bytes)), .. } if bytes == b"{}")
        );
        perform(IoRequest::RemoveState { path: path.clone() });
        let gone = perform(IoRequest::ReadState { path });
        assert!(matches!(
            gone,
            IoResponse::StateRead {
                result: Ok(None),
                ..
            }
        ));
    }
}
