//! Streaming archive CAS for large, local preservation objects.
//!
//! This is deliberately separate from [`crate::cas::CasStorage`]. The network
//! CAS remains a small-object, `u32`-sized protocol surface; archive CAS uses
//! bounded-memory file ingestion, `u64` sizes, and positioned reads.

use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ARCHIVE_CAS_FORMAT_V1: &str = "loadngo-archive-cas-v1";
pub const ARCHIVE_MANIFEST_FORMAT_V1: &str = "loadngo-archive-manifest-v1";
pub const ARCHIVE_MANIFEST_FORMAT_V2: &str = "loadngo-archive-manifest-v2";
pub const DEFAULT_BUFFER_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveObject {
    pub hash: CasHash,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveIngestResult {
    pub object: ArchiveObject,
    pub inserted: bool,
    pub resumed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub format: String,
    pub archive_id: String,
    pub source_label: String,
    pub created_at_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_archive_root: Option<CasHash>,
    pub entries: Vec<ArchiveEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveEntry {
    Directory {
        path: String,
        modified_at_unix_secs: Option<u64>,
    },
    File {
        path: String,
        object: ArchiveObject,
        modified_at_unix_secs: Option<u64>,
    },
    Symlink {
        path: String,
        target: String,
    },
    /// A source directory entry that the mounted filesystem reported but
    /// could not be reopened. It deliberately carries no object: this makes
    /// the capture gap explicit rather than silently omitting the entry.
    Unreadable {
        path: String,
        operation: String,
        error: String,
    },
    /// An owner-approved source exclusion. It remains visible in the
    /// manifest, has no blob object, and defines the archive's scope.
    Excluded {
        path: String,
        reason: String,
    },
}

impl ArchiveEntry {
    pub fn path(&self) -> &str {
        match self {
            Self::Directory { path, .. }
            | Self::File { path, .. }
            | Self::Symlink { path, .. }
            | Self::Unreadable { path, .. }
            | Self::Excluded { path, .. } => path,
        }
    }
}

impl ArchiveManifest {
    pub fn new(
        archive_id: impl Into<String>,
        source_label: impl Into<String>,
        created_at_unix_secs: u64,
        mut entries: Vec<ArchiveEntry>,
    ) -> Result<Self> {
        let archive_id = archive_id.into();
        if !is_archive_id(&archive_id) {
            bail!("archive_id must use lowercase letters, digits, and hyphens");
        }
        let source_label = source_label.into();
        if source_label.trim().is_empty() {
            bail!("source_label must not be empty");
        }
        validate_manifest_entries(&mut entries)?;
        Ok(Self {
            format: ARCHIVE_MANIFEST_FORMAT_V2.to_string(),
            archive_id,
            source_label,
            created_at_unix_secs,
            supersedes_archive_root: None,
            entries,
        })
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        if self.format != ARCHIVE_MANIFEST_FORMAT_V1 && self.format != ARCHIVE_MANIFEST_FORMAT_V2 {
            bail!("unsupported archive manifest format {:?}", self.format);
        }
        if self.format == ARCHIVE_MANIFEST_FORMAT_V1
            && (!self.is_complete() || self.excluded_entry_count() > 0)
        {
            bail!("archive manifest v1 cannot represent unresolved or excluded source entries");
        }
        if self.format == ARCHIVE_MANIFEST_FORMAT_V1 && self.supersedes_archive_root.is_some() {
            bail!("archive manifest v1 cannot represent a superseded archive root");
        }
        if !is_archive_id(&self.archive_id) {
            bail!("archive_id must use lowercase letters, digits, and hyphens");
        }
        if self.source_label.trim().is_empty() {
            bail!("source_label must not be empty");
        }
        let mut normalized = self.clone();
        validate_manifest_entries(&mut normalized.entries)?;
        serde_json::to_vec_pretty(&normalized).context("failed to serialize archive manifest")
    }

    pub fn digest(&self) -> Result<CasHash> {
        Ok(CasHash::digest(&self.canonical_bytes()?))
    }

    pub fn file_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, ArchiveEntry::File { .. }))
            .count()
    }

    pub fn unreadable_entry_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, ArchiveEntry::Unreadable { .. }))
            .count()
    }

    pub fn excluded_entry_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, ArchiveEntry::Excluded { .. }))
            .count()
    }

    pub fn is_complete(&self) -> bool {
        self.unreadable_entry_count() == 0
    }

    /// Replaces one unresolved source entry with a declared owner-approved
    /// exclusion, producing a new immutable manifest that links to this root.
    pub fn with_owner_approved_exclusion(
        &self,
        path: &str,
        reason: impl Into<String>,
        created_at_unix_secs: u64,
    ) -> Result<Self> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            bail!("exclusion reason must not be empty");
        }
        let mut entries = self.entries.clone();
        let Some(entry) = entries.iter_mut().find(|entry| entry.path() == path) else {
            bail!("source entry is not present in archive manifest: {path:?}");
        };
        if !matches!(entry, ArchiveEntry::Unreadable { .. }) {
            bail!("only an unresolved unreadable entry may be excluded: {path:?}");
        }
        *entry = ArchiveEntry::Excluded {
            path: path.to_string(),
            reason,
        };
        let mut amended = Self::new(
            self.archive_id.clone(),
            self.source_label.clone(),
            created_at_unix_secs,
            entries,
        )?;
        amended.supersedes_archive_root = Some(self.digest()?);
        Ok(amended)
    }
}

fn validate_manifest_entries(entries: &mut [ArchiveEntry]) -> Result<()> {
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    for entry in entries.iter() {
        validate_relative_path(entry.path())?;
        match entry {
            ArchiveEntry::Unreadable {
                operation, error, ..
            } if operation.trim().is_empty() || error.trim().is_empty() => {
                bail!("unreadable archive entry needs a non-empty operation and error");
            }
            ArchiveEntry::Excluded { reason, .. } if reason.trim().is_empty() => {
                bail!("excluded archive entry needs a non-empty reason");
            }
            _ => {}
        }
    }
    for pair in entries.windows(2) {
        if pair[0].path() == pair[1].path() {
            bail!("duplicate archive manifest path {:?}", pair[0].path());
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct ArchiveCasStorage {
    root: PathBuf,
    objects: PathBuf,
    partials: PathBuf,
    manifests: PathBuf,
    buffer_bytes: usize,
}

impl ArchiveCasStorage {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_buffer_size(root, DEFAULT_BUFFER_BYTES)
    }

    pub fn with_buffer_size(root: impl Into<PathBuf>, buffer_bytes: usize) -> Result<Self> {
        if buffer_bytes == 0 {
            bail!("archive CAS buffer size must be non-zero");
        }
        let root = root.into();
        let objects = root.join("objects");
        let partials = root.join("partials");
        let manifests = root.join("manifests");
        fs::create_dir_all(&objects)?;
        fs::create_dir_all(&partials)?;
        fs::create_dir_all(&manifests)?;
        Ok(Self {
            root,
            objects,
            partials,
            manifests,
            buffer_bytes,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifests_root(&self) -> &Path {
        &self.manifests
    }

    pub fn object_path(&self, hash: CasHash) -> PathBuf {
        let hex = hash.to_hex();
        self.objects.join(&hex[..2]).join(format!("{hex}.blob"))
    }

    /// Adds an in-memory control object, such as a manifest, to the archive.
    ///
    /// Large source files should use [`Self::ingest_file`]. This helper is for
    /// small, generated objects whose bytes are already available in memory.
    pub fn add_content(&self, bytes: &[u8]) -> Result<ArchiveIngestResult> {
        let hash = CasHash::digest(bytes);
        let object = ArchiveObject {
            hash,
            size: u64::try_from(bytes.len()).context("content length exceeds u64")?,
        };
        let object_path = self.object_path(hash);
        self.ensure_object_parent(&object_path)?;

        if object_path.exists() {
            self.verify_object(object)?;
            return Ok(ArchiveIngestResult {
                object,
                inserted: false,
                resumed_bytes: 0,
            });
        }

        let temporary = self.partials.join(format!(
            ".content-{}-{}.partial",
            std::process::id(),
            unique_suffix()
        ));
        write_synced_file(&temporary, bytes)?;
        match fs::hard_link(&temporary, &object_path) {
            Ok(()) => {
                sync_parent(&object_path)?;
                fs::remove_file(&temporary)?;
                sync_parent(&temporary)?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: true,
                    resumed_bytes: 0,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_object(object)?;
                fs::remove_file(&temporary)?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: false,
                    resumed_bytes: 0,
                })
            }
            Err(error) => Err(error).context("failed to publish archive content object"),
        }
    }

    pub fn ingest_file(
        &self,
        source: impl AsRef<Path>,
        source_key: &str,
    ) -> Result<ArchiveIngestResult> {
        let source = source.as_ref();
        if source_key.trim().is_empty() {
            bail!("source_key must not be empty");
        }
        let initial = SourceStamp::from_path(source)?;
        if !initial.file_type.is_file() {
            bail!(
                "archive CAS accepts only regular files: {}",
                source.display()
            );
        }

        let partial_path = self.partial_path(source_key, initial);
        let partial_len = match fs::metadata(&partial_path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to stat {}", partial_path.display()))
            }
        };
        if partial_len > initial.size {
            bail!(
                "partial object {} is larger than source {}",
                partial_path.display(),
                source.display()
            );
        }

        let mut source_file = File::open(source)
            .with_context(|| format!("failed to open source {}", source.display()))?;
        let mut hasher = blake3::Hasher::new();
        if partial_len > 0 {
            self.verify_and_hash_partial_prefix(
                &mut source_file,
                &partial_path,
                partial_len,
                &mut hasher,
            )?;
        }

        let mut partial_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&partial_path)
            .with_context(|| format!("failed to open partial {}", partial_path.display()))?;
        let mut buffer = vec![0_u8; self.buffer_bytes];
        loop {
            let count = source_file
                .read(&mut buffer)
                .with_context(|| format!("failed to read source {}", source.display()))?;
            if count == 0 {
                break;
            }
            partial_file
                .write_all(&buffer[..count])
                .with_context(|| format!("failed to write partial {}", partial_path.display()))?;
            hasher.update(&buffer[..count]);
        }
        partial_file
            .sync_all()
            .with_context(|| format!("failed to synchronize partial {}", partial_path.display()))?;
        drop(partial_file);

        let final_stamp = SourceStamp::from_path(source)?;
        if initial != final_stamp {
            bail!("source changed during archive ingest: {}", source.display());
        }
        let partial_size = fs::metadata(&partial_path)?.len();
        if partial_size != initial.size {
            bail!(
                "partial object length mismatch for {}: expected {}, got {}",
                source.display(),
                initial.size,
                partial_size
            );
        }

        let hash = CasHash::from_bytes(*hasher.finalize().as_bytes());
        let object = ArchiveObject {
            hash,
            size: initial.size,
        };
        let object_path = self.object_path(hash);
        self.ensure_object_parent(&object_path)?;

        if object_path.exists() {
            self.verify_object(object)?;
            fs::remove_file(&partial_path).with_context(|| {
                format!(
                    "failed to remove deduplicated partial {}",
                    partial_path.display()
                )
            })?;
            return Ok(ArchiveIngestResult {
                object,
                inserted: false,
                resumed_bytes: partial_len,
            });
        }

        match fs::hard_link(&partial_path, &object_path) {
            Ok(()) => {
                sync_parent(&object_path)?;
                fs::remove_file(&partial_path).with_context(|| {
                    format!(
                        "failed to remove published partial {}",
                        partial_path.display()
                    )
                })?;
                sync_parent(&partial_path)?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: true,
                    resumed_bytes: partial_len,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_object(object)?;
                fs::remove_file(&partial_path).with_context(|| {
                    format!("failed to remove raced partial {}", partial_path.display())
                })?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: false,
                    resumed_bytes: partial_len,
                })
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to publish archive object {} from {}",
                    object_path.display(),
                    partial_path.display()
                )
            }),
        }
    }

    pub fn verify_object(&self, expected: ArchiveObject) -> Result<()> {
        let path = self.object_path(expected.hash);
        let metadata = fs::metadata(&path)
            .with_context(|| format!("archive object is missing: {}", path.display()))?;
        if metadata.len() != expected.size {
            bail!(
                "archive object size mismatch for {}: expected {}, got {}",
                expected.hash,
                expected.size,
                metadata.len()
            );
        }
        let actual = hash_file_streaming(&path, self.buffer_bytes)?;
        if actual != expected.hash {
            bail!(
                "archive object hash mismatch for {}: expected {}, got {}",
                path.display(),
                expected.hash,
                actual
            );
        }
        Ok(())
    }

    pub fn read_range(&self, hash: CasHash, offset: u64, size: usize) -> Result<Vec<u8>> {
        let path = self.object_path(hash);
        let length = fs::metadata(&path)
            .with_context(|| format!("archive object is missing: {}", path.display()))?
            .len();
        if offset > length {
            bail!("offset {offset} past end of archive object {hash}");
        }
        let remaining = length - offset;
        let amount = remaining.min(u64::try_from(size).unwrap_or(u64::MAX));
        let amount = usize::try_from(amount).context("range is too large for memory")?;
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0_u8; amount];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Restores one verified archive object to a newly-created destination.
    ///
    /// The object is streamed through a temporary sibling file, hashed while
    /// copying, synchronized, and only then renamed into place. This method
    /// refuses to replace an existing destination.
    pub fn restore_object_to_path(
        &self,
        expected: ArchiveObject,
        destination: &Path,
    ) -> Result<()> {
        if destination.exists() {
            bail!(
                "restore destination already exists and will not be replaced: {}",
                destination.display()
            );
        }
        let parent = destination.parent().ok_or_else(|| {
            anyhow!(
                "restore destination has no parent: {}",
                destination.display()
            )
        })?;
        if !parent.is_dir() {
            bail!(
                "restore destination parent is not a directory: {}",
                parent.display()
            );
        }
        let file_name = destination.file_name().ok_or_else(|| {
            anyhow!(
                "restore destination has no file name: {}",
                destination.display()
            )
        })?;
        let temporary = parent.join(format!(
            ".{}-{}-{}.partial",
            file_name.to_string_lossy(),
            std::process::id(),
            unique_suffix()
        ));
        let result = self.restore_object_to_temporary(expected, &temporary);
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }

        if destination.exists() {
            let _ = fs::remove_file(&temporary);
            bail!(
                "restore destination appeared during copy and will not be replaced: {}",
                destination.display()
            );
        }
        fs::rename(&temporary, destination).with_context(|| {
            format!(
                "failed to publish restored object from {} to {}",
                temporary.display(),
                destination.display()
            )
        })?;
        sync_parent(destination)?;
        Ok(())
    }

    pub fn write_manifest(&self, manifest: &ArchiveManifest) -> Result<(PathBuf, ArchiveObject)> {
        let bytes = manifest.canonical_bytes()?;
        let stored = self.add_content(&bytes)?;
        let object = stored.object;
        let path = self.manifests.join(format!(
            "{}-{}.json",
            manifest.archive_id,
            object.hash.to_hex()
        ));
        if path.exists() {
            let existing = fs::read(&path)?;
            if existing != bytes {
                bail!("existing archive manifest differs at {}", path.display());
            }
            return Ok((path, object));
        }

        let temporary = self.manifests.join(format!(
            ".{}-{}-{}.partial",
            manifest.archive_id,
            std::process::id(),
            unique_suffix()
        ));
        write_synced_file(&temporary, &bytes)?;
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                sync_parent(&path)?;
                fs::remove_file(&temporary)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = fs::read(&path)?;
                fs::remove_file(&temporary)?;
                if existing != bytes {
                    bail!("raced archive manifest differs at {}", path.display());
                }
            }
            Err(error) => return Err(error).context("failed to publish archive manifest"),
        }
        Ok((path, object))
    }

    pub fn read_manifest(&self, path: impl AsRef<Path>) -> Result<ArchiveManifest> {
        let path = path.as_ref();
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let manifest: ArchiveManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let canonical = manifest.canonical_bytes()?;
        if canonical != bytes {
            bail!("archive manifest is not canonical: {}", path.display());
        }
        Ok(manifest)
    }

    fn partial_path(&self, source_key: &str, stamp: SourceStamp) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ARCHIVE_CAS_FORMAT_V1.as_bytes());
        hasher.update(&[0]);
        hasher.update(source_key.as_bytes());
        hasher.update(&stamp.size.to_le_bytes());
        hasher.update(
            &stamp
                .modified_at_unix_secs
                .unwrap_or_default()
                .to_le_bytes(),
        );
        self.partials.join(format!(
            "{}.part",
            hex::encode(hasher.finalize().as_bytes())
        ))
    }

    fn ensure_object_parent(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("archive object has no parent: {}", path.display()))?;
        fs::create_dir_all(parent)?;
        Ok(())
    }

    fn verify_and_hash_partial_prefix(
        &self,
        source: &mut File,
        partial_path: &Path,
        partial_len: u64,
        hasher: &mut blake3::Hasher,
    ) -> Result<()> {
        let mut partial = File::open(partial_path)
            .with_context(|| format!("failed to open partial {}", partial_path.display()))?;
        let mut source_buffer = vec![0_u8; self.buffer_bytes];
        let mut partial_buffer = vec![0_u8; self.buffer_bytes];
        let mut remaining = partial_len;
        while remaining > 0 {
            let wanted = usize::try_from(remaining.min(self.buffer_bytes as u64))?;
            source.read_exact(&mut source_buffer[..wanted])?;
            partial.read_exact(&mut partial_buffer[..wanted])?;
            if source_buffer[..wanted] != partial_buffer[..wanted] {
                bail!(
                    "source no longer matches resumable partial {}",
                    partial_path.display()
                );
            }
            hasher.update(&source_buffer[..wanted]);
            remaining -= wanted as u64;
        }
        Ok(())
    }

    fn restore_object_to_temporary(&self, expected: ArchiveObject, temporary: &Path) -> Result<()> {
        let source_path = self.object_path(expected.hash);
        let source_metadata = fs::metadata(&source_path)
            .with_context(|| format!("archive object is missing: {}", source_path.display()))?;
        if source_metadata.len() != expected.size {
            bail!(
                "archive object size mismatch for {}: expected {}, got {}",
                expected.hash,
                expected.size,
                source_metadata.len()
            );
        }

        let mut source = File::open(&source_path)?;
        let mut destination = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temporary)
            .with_context(|| {
                format!("failed to create restore temporary {}", temporary.display())
            })?;
        let mut buffer = vec![0_u8; self.buffer_bytes];
        let mut hasher = blake3::Hasher::new();
        let mut copied = 0_u64;
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            destination.write_all(&buffer[..count])?;
            hasher.update(&buffer[..count]);
            copied = copied
                .checked_add(count as u64)
                .ok_or_else(|| anyhow!("restored object size overflow"))?;
        }
        destination.sync_all()?;
        if copied != expected.size {
            bail!(
                "restored object size mismatch for {}: expected {}, got {}",
                expected.hash,
                expected.size,
                copied
            );
        }
        let actual = CasHash::from_bytes(*hasher.finalize().as_bytes());
        if actual != expected.hash {
            bail!(
                "restored object hash mismatch: expected {}, got {}",
                expected.hash,
                actual
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceStamp {
    size: u64,
    modified_at_unix_secs: Option<u64>,
    file_type: std::fs::FileType,
}

impl SourceStamp {
    fn from_path(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to stat source {}", path.display()))?;
        let modified_at_unix_secs = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        Ok(Self {
            size: metadata.len(),
            modified_at_unix_secs,
            file_type: metadata.file_type(),
        })
    }
}

fn hash_file_streaming(path: &Path, buffer_bytes: usize) -> Result<CasHash> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; buffer_bytes];
    let mut hasher = blake3::Hasher::new();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(CasHash::from_bytes(*hasher.finalize().as_bytes()))
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)
            .with_context(|| format!("failed to open archive parent {}", parent.display()))?
            .sync_all()
            .with_context(|| {
                format!("failed to synchronize archive parent {}", parent.display())
            })?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn is_archive_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_relative_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty() || value.contains('\\') || path.is_absolute() {
        bail!("archive manifest path must be non-empty and relative: {value:?}");
    }
    if value
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        bail!("archive manifest path is not canonical: {value:?}");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("archive manifest path escapes its root: {value:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn partial_resume_rechecks_the_source_prefix() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.bin");
        let content = b"a resumable source object";
        fs::write(&source, content).unwrap();
        let store = ArchiveCasStorage::with_buffer_size(directory.path().join("cas"), 3).unwrap();

        let stamp = SourceStamp::from_path(&source).unwrap();
        let partial_path = store.partial_path("source.bin", stamp);
        fs::write(&partial_path, &content[..7]).unwrap();

        let result = store.ingest_file(&source, "source.bin").unwrap();
        assert_eq!(result.resumed_bytes, 7);
        assert!(result.inserted);
        store.verify_object(result.object).unwrap();
        assert!(!partial_path.exists());

        let mismatched_partial = store.partial_path("mismatched.bin", stamp);
        fs::write(&mismatched_partial, b"not-the").unwrap();
        let error = store.ingest_file(&source, "mismatched.bin").unwrap_err();
        assert!(error
            .to_string()
            .contains("source no longer matches resumable partial"));
    }

    #[test]
    fn restore_streams_to_a_new_destination_without_overwrite() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.bin");
        let content = (0..(1024 * 1024 + 31))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        fs::write(&source, &content).unwrap();
        let store =
            ArchiveCasStorage::with_buffer_size(directory.path().join("cas"), 4_093).unwrap();
        let object = store.ingest_file(&source, "source.bin").unwrap().object;

        let restore_directory = restore_drill_directory(&directory);
        let restored = restore_directory.path().join("restored.bin");
        store.restore_object_to_path(object, &restored).unwrap();
        assert_eq!(fs::read(&restored).unwrap(), content);
        assert!(store.restore_object_to_path(object, &restored).is_err());
    }

    fn restore_drill_directory(fallback: &tempfile::TempDir) -> tempfile::TempDir {
        match std::env::var_os("LOADNGO_ARCHIVE_RESTORE_DRILL_ROOT") {
            Some(root) => tempfile::tempdir_in(root).unwrap(),
            None => tempfile::tempdir_in(fallback.path()).unwrap(),
        }
    }
}
