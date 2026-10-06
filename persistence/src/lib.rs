//! Atomic, corruption-checked file persistence for `loadngo`-based games and
//! tools.
//!
//! Every prior attempt at this in the wider `loadngo`/`sng-rusty` family
//! (`loadngo/data`'s CAS index, its task-file `persistence` module,
//! `sng-rusty`'s several save/load pairs) either skips atomicity (a write
//! interrupted partway through corrupts the only copy on disk) or writes a
//! schema-version field that's never actually checked on read. This crate
//! exists to stop that pattern from being reinvented, badly, per game. See
//! `docs/PERSISTENCE.md` for the fuller reasoning and prior-art survey.
//!
//! Deliberately byte-oriented rather than generic over `serde::Serialize`:
//! callers own their own serialization format (RON, JSON, anything), and
//! this crate's only real dependency stays `blake3`.
//!
//! # On-disk format
//!
//! ```text
//! offset 0..4    magic b"LGP1"
//! offset 4..36   BLAKE3 hash (32 bytes) of bytes[36..]
//! offset 36..40  caller-supplied schema_version: u32, little-endian
//! offset 40..    payload bytes (opaque to this crate)
//! ```
//!
//! The magic distinguishes "not one of our files at all" from "one of our
//! files, but corrupted" — the same idea as the CAS lineage's own
//! `Marker` constant, just at the file-envelope level instead of the
//! content-block level.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

const MAGIC: [u8; 4] = *b"LGP1";
const HASH_LEN: usize = 32;
const VERSION_LEN: usize = 4;
const HEADER_LEN: usize = MAGIC.len() + HASH_LEN + VERSION_LEN;

/// A successfully verified, decoded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub schema_version: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum PersistenceError {
    Io(std::io::Error),
    /// The path has no parent directory to stage the atomic write in.
    NoParentDirectory,
    /// The file doesn't start with this crate's magic — not one of ours,
    /// or a completely different format.
    BadMagic,
    /// The file is shorter than a valid header; definitely not intact.
    Truncated,
    /// The file has the right shape but its stored hash doesn't match its
    /// content — write was interrupted, or the file was corrupted at rest.
    ChecksumMismatch,
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(formatter, "persistence I/O error: {err}"),
            Self::NoParentDirectory => {
                write!(
                    formatter,
                    "path has no parent directory for an atomic write"
                )
            }
            Self::BadMagic => write!(formatter, "not a loadngo-persistence file (bad magic)"),
            Self::Truncated => write!(formatter, "loadngo-persistence file is truncated"),
            Self::ChecksumMismatch => {
                write!(formatter, "loadngo-persistence file failed its checksum")
            }
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<std::io::Error> for PersistenceError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Writes `payload` to `path` atomically, inside this crate's checked
/// envelope: builds the envelope in memory, then [`replace_atomically`].
///
/// A write that fails partway through never touches the file at `path`;
/// at worst it leaves a stray `.tmp` file, which the next successful
/// `write_atomic` call for the same path simply overwrites.
///
/// # Errors
///
/// Returns [`PersistenceError::NoParentDirectory`] if `path` has no parent
/// directory, or [`PersistenceError::Io`] for any filesystem failure.
pub fn write_atomic(
    path: &Path,
    schema_version: u32,
    payload: &[u8],
) -> Result<(), PersistenceError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(PersistenceError::NoParentDirectory)?;
    fs::create_dir_all(parent)?;

    let mut hashed = Vec::with_capacity(VERSION_LEN + payload.len());
    hashed.extend_from_slice(&schema_version.to_le_bytes());
    hashed.extend_from_slice(payload);
    let hash = blake3::hash(&hashed);

    let mut buffer = Vec::with_capacity(HEADER_LEN + payload.len());
    buffer.extend_from_slice(&MAGIC);
    buffer.extend_from_slice(hash.as_bytes());
    buffer.extend_from_slice(&hashed);

    replace_atomically(path, &buffer)?;
    Ok(())
}

/// Replaces the file at `path` with `contents`, as they are, so that after a
/// crash or power loss it holds either its old contents or all of the new.
///
/// Writes a `.tmp` file beside `path` (the rename must stay on one volume),
/// flushes it to disk, renames it over `path`, then makes the rename durable:
/// on Unix by syncing the parent directory, on Windows by renaming with
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
/// Microsoft documents write-through in terms of moves done as a copy and
/// delete; for a rename within a volume it is the strongest request the API
/// offers, and the usual choice for durable replacement on Windows.
///
/// For files that need no envelope, such as another program's own format.
/// The parent directory is created if missing.
///
/// # Errors
///
/// Any filesystem failure, or a `path` with no file name.
pub fn replace_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }
    if path.file_name().is_none() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("{} has no file name", path.display()),
        ));
    }

    let tmp_path = temp_path_for(path);
    {
        let mut tmp_file = File::create(&tmp_path)?;
        tmp_file.write_all(contents)?;
        tmp_file.sync_all()?;
    }
    rename_durably(&tmp_path, path, parent.unwrap_or(Path::new(".")))
}

/// Reads and verifies `path`.
///
/// `Ok(None)` means the file simply doesn't exist yet (first launch, no
/// save written) — not an error. Any structural problem is a distinct
/// [`PersistenceError`] variant so a caller can log or report which
/// failure actually happened rather than a generic "couldn't load."
///
/// Schema-version *policy* (what counts as too old or too new, whether to
/// migrate) is deliberately left to the caller — this only reports the
/// version cleanly.
///
/// # Errors
///
/// Returns [`PersistenceError::BadMagic`], [`PersistenceError::Truncated`],
/// or [`PersistenceError::ChecksumMismatch`] for a malformed or corrupted
/// file, or [`PersistenceError::Io`] for any other filesystem failure.
pub fn read_checked(path: &Path) -> Result<Option<Loaded>, PersistenceError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if bytes.len() < HEADER_LEN {
        return Err(PersistenceError::Truncated);
    }
    if !bytes.starts_with(&MAGIC) {
        return Err(PersistenceError::BadMagic);
    }
    let stored_hash = &bytes[MAGIC.len()..MAGIC.len() + HASH_LEN];
    let rest = &bytes[MAGIC.len() + HASH_LEN..];
    let actual_hash = blake3::hash(rest);
    if stored_hash != &actual_hash.as_bytes()[..] {
        return Err(PersistenceError::ChecksumMismatch);
    }
    let schema_version = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let payload = rest[VERSION_LEN..].to_vec();
    Ok(Some(Loaded {
        schema_version,
        payload,
    }))
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map_or_else(Default::default, ToOwned::to_owned);
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(unix)]
fn rename_durably(from: &Path, to: &Path, parent: &Path) -> io::Result<()> {
    fs::rename(from, to)?;
    File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn rename_durably(from: &Path, to: &Path, _parent: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let wide =
        |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated UTF-16 paths that outlive the call.
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    // The failure is HRESULT_FROM_WIN32(GetLastError()); its low 16 bits are
    // the Win32 code, which io::Error maps to an ErrorKind.
    .map_err(|err| io::Error::from_raw_os_error(err.code().0 & 0xFFFF))
}

#[cfg(not(any(unix, windows)))]
fn rename_durably(from: &Path, to: &Path, _parent: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{read_checked, replace_atomically, write_atomic, PersistenceError};

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("must create a temp directory")
    }

    #[test]
    fn round_trips_schema_version_and_payload() {
        let dir = temp_dir();
        let path = dir.path().join("profile.bin");

        write_atomic(&path, 3, b"hello world").expect("write must succeed");
        let loaded = read_checked(&path)
            .expect("read must succeed")
            .expect("file must exist");

        assert_eq!(loaded.schema_version, 3);
        assert_eq!(loaded.payload, b"hello world");
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let dir = temp_dir();
        let path = dir.path().join("does-not-exist.bin");

        let loaded = read_checked(&path).expect("a missing file must not error");

        assert!(loaded.is_none());
    }

    #[test]
    fn a_corrupted_payload_byte_is_detected() {
        let dir = temp_dir();
        let path = dir.path().join("profile.bin");
        write_atomic(&path, 1, b"payload bytes").expect("write must succeed");

        let mut bytes = fs::read(&path).expect("file must exist");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        fs::write(&path, &bytes).expect("must overwrite with corrupted bytes");

        assert!(matches!(
            read_checked(&path),
            Err(PersistenceError::ChecksumMismatch)
        ));
    }

    #[test]
    fn a_file_shorter_than_the_header_is_truncated_not_a_checksum_mismatch() {
        let dir = temp_dir();
        let path = dir.path().join("profile.bin");
        write_atomic(&path, 1, b"payload bytes").expect("write must succeed");

        let bytes = fs::read(&path).expect("file must exist");
        // Cut well below the 40-byte header so this can only be reported as
        // `Truncated`, distinct from a same-length-but-corrupted file.
        fs::write(&path, &bytes[..10]).expect("must write truncated bytes");

        assert!(matches!(
            read_checked(&path),
            Err(PersistenceError::Truncated)
        ));
    }

    #[test]
    fn a_file_shortened_within_the_payload_is_a_checksum_mismatch() {
        let dir = temp_dir();
        let path = dir.path().join("profile.bin");
        write_atomic(&path, 1, b"payload bytes").expect("write must succeed");

        let bytes = fs::read(&path).expect("file must exist");
        // Still at least a full header, so this must be caught by the hash
        // check rather than the length check.
        fs::write(&path, &bytes[..bytes.len() - 3]).expect("must write shortened bytes");

        assert!(matches!(
            read_checked(&path),
            Err(PersistenceError::ChecksumMismatch)
        ));
    }

    #[test]
    fn a_bad_magic_is_reported_distinctly_from_a_checksum_mismatch() {
        let dir = temp_dir();
        let path = dir.path().join("profile.bin");
        write_atomic(&path, 1, b"payload bytes").expect("write must succeed");

        let mut bytes = fs::read(&path).expect("file must exist");
        bytes[0..4].copy_from_slice(b"NOPE");
        fs::write(&path, &bytes).expect("must overwrite with bad magic");

        assert!(matches!(
            read_checked(&path),
            Err(PersistenceError::BadMagic)
        ));
    }

    #[test]
    fn a_stray_temp_file_does_not_affect_reading_the_real_path() {
        let dir = temp_dir();
        let path = dir.path().join("profile.bin");
        write_atomic(&path, 1, b"real content").expect("write must succeed");

        let tmp_path = dir.path().join("profile.bin.tmp");
        fs::write(&tmp_path, b"leftover garbage from an interrupted write")
            .expect("must write a stray temp file");

        let loaded = read_checked(&path)
            .expect("read must succeed")
            .expect("file must exist");
        assert_eq!(loaded.payload, b"real content");
    }

    #[test]
    fn replace_atomically_writes_raw_bytes_and_replaces_an_existing_file() {
        let dir = temp_dir();
        let path = dir.path().join("nested").join("state.json");
        replace_atomically(&path, b"{\"height\":1}").unwrap();
        replace_atomically(&path, b"{\"height\":2}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"height\":2}");
        assert!(!dir.path().join("nested").join("state.json.tmp").exists());
        assert!(replace_atomically(Path::new("/"), b"").is_err());
    }

    #[test]
    fn a_second_write_fully_replaces_the_first_no_partial_mixing() {
        let dir = temp_dir();
        let path = dir.path().join("profile.bin");

        write_atomic(
            &path,
            1,
            b"first payload, quite a bit longer than the second",
        )
        .expect("first write must succeed");
        write_atomic(&path, 2, b"second").expect("second write must succeed");

        let loaded = read_checked(&path)
            .expect("read must succeed")
            .expect("file must exist");
        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.payload, b"second");
    }
}
