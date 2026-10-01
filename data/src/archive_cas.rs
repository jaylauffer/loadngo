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
/// Adds parents (more than one for a merge), change records and attachments; see
/// [`ArchiveManifest`].
pub const ARCHIVE_MANIFEST_FORMAT_V3: &str = "loadngo-archive-manifest-v3";
/// The file name endings of the sidecar logs written beside manifests before v3. They
/// are not manifests; `archive_cas_upgrade` attaches them to a v3 version.
pub const LEGACY_LOG_SUFFIXES: &[&str] = &[
    ".delete-log.json",
    ".add-log.json",
    ".merge-log.json",
    ".unpack-log.json",
];
pub const DEFAULT_BUFFER_BYTES: usize = 8 * 1024 * 1024;
pub const ARCHIVE_COMPRESSION_FORMAT_V1: &str = "loadngo-archive-cas-compression-v1";
/// How a valid `CACHEDIR.TAG` starts (<https://bford.info/cachedir/>). Cargo writes one in
/// every target directory, whatever it is named (`target-android-build-std` too), and
/// other tools mark their caches the same way.
pub const CACHEDIR_TAG_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether `directory` holds a valid `CACHEDIR.TAG`: regenerable build output, which
/// ingest and add do not capture.
#[must_use]
pub fn is_build_cache(directory: &Path) -> bool {
    let mut start = [0_u8; CACHEDIR_TAG_SIGNATURE.len()];
    fs::File::open(directory.join("CACHEDIR.TAG"))
        .and_then(|mut tag| tag.read_exact(&mut start))
        .is_ok()
        && &start == CACHEDIR_TAG_SIGNATURE
}

/// A root's compression setting, at `<root>/compression.json`. Without it (the default)
/// new objects are stored as they are.
pub const COMPRESSION_SETTINGS_FILE: &str = "compression.json";
/// The zstd level a root gets when compression is turned on without one.
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 9;
/// Objects smaller than this are always stored as they are: the file system's block
/// rounding eats what compression could save.
pub const MIN_COMPRESSED_OBJECT_BYTES: u64 = 8 * 1024;
/// How much of a large object is compressed first, quickly, to skip media and other
/// already-compressed content without compressing all of it.
const COMPRESSION_SAMPLE_BYTES: usize = 1024 * 1024;

/// A root's compression setting (`compression.json`).
///
/// Compression changes only how an object's bytes sit on disk. An object is named by the
/// BLAKE3 hash of its uncompressed bytes either way and [`ArchiveObject::size`] stays
/// the uncompressed size, so manifests, roots and signatures do not change, and every
/// reader in this module returns (and verifies) the uncompressed bytes. A compressed
/// object is `objects/<xx>/<hash>.zst` (one zstd stream) instead of `<hash>.blob`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveCompression {
    pub format: String,
    pub codec: String,
    pub level: i32,
}

/// One change that made a version from its parents: what kind, the paths it named,
/// who made it, when and why. A v3 manifest lists its records before its entries, so
/// the root's hash and signature cover them, and a lister reads them from the header.
/// See `docs/RECONCILIATION.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRecord {
    pub actor: String,
    pub at_unix_secs: u64,
    pub reason: String,
    pub change: ArchiveChange,
}

/// What an [`ArchiveRecord`] changed. Paths are as named by whoever made the change: a
/// removed directory is one path, not one per file below it. What each path held before
/// is in the parent version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveChange {
    /// New entries (`archive_cas_add`), with any directories made to hold them.
    Created { paths: Vec<String> },
    /// Entries whose state changed in place: an unreadable entry excluded by its owner.
    Changed { paths: Vec<String> },
    /// Entries placed somewhere else: for a merge, each parent's tree under a folder.
    Moved { moves: Vec<ArchiveMove> },
    /// Entries removed, a directory with everything below it.
    Deleted { paths: Vec<String> },
    /// Files that became others: each zip became a folder of its members at its path.
    Derived { paths: Vec<String> },
}

/// One placement change in [`ArchiveChange::Moved`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveMove {
    /// The parent version the entries come from.
    pub parent: CasHash,
    /// Their path there; empty for that version's whole tree.
    pub from: String,
    pub to: String,
}

/// A file kept with a version as it was found, not as a record of the version's own
/// making: the sidecar logs that described changes before v3, attached unchanged by
/// `archive_cas_upgrade`. Nothing vouches for what they say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveAttachment {
    /// Its file name where it was found.
    pub name: String,
    pub object: ArchiveObject,
}

impl ArchiveRecord {
    /// # Errors
    /// An empty actor or reason.
    pub fn new(
        change: ArchiveChange,
        actor: impl Into<String>,
        reason: impl Into<String>,
        at_unix_secs: u64,
    ) -> Result<Self> {
        let record = Self {
            actor: actor.into(),
            at_unix_secs,
            reason: reason.into(),
            change,
        };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<()> {
        if self.actor.trim().is_empty() {
            bail!("a change record needs an actor");
        }
        if self.reason.trim().is_empty() {
            bail!("a change record needs a reason");
        }
        let paths: Vec<&str> = match &self.change {
            ArchiveChange::Created { paths }
            | ArchiveChange::Changed { paths }
            | ArchiveChange::Deleted { paths }
            | ArchiveChange::Derived { paths } => paths.iter().map(String::as_str).collect(),
            ArchiveChange::Moved { moves } => {
                for one in moves {
                    if !one.from.is_empty() {
                        validate_relative_path(&one.from)?;
                    }
                }
                moves.iter().map(|one| one.to.as_str()).collect()
            }
        };
        if paths.is_empty() {
            bail!("a change record names no paths");
        }
        for path in paths {
            validate_relative_path(path)?;
        }
        Ok(())
    }

    /// `deleted 3 paths: a, b, c` and so on, for a listing.
    #[must_use]
    pub fn describe(&self) -> String {
        let (verb, names): (&str, Vec<String>) = match &self.change {
            ArchiveChange::Created { paths } => ("created", paths.clone()),
            ArchiveChange::Changed { paths } => ("changed", paths.clone()),
            ArchiveChange::Deleted { paths } => ("deleted", paths.clone()),
            ArchiveChange::Derived { paths } => ("unpacked", paths.clone()),
            ArchiveChange::Moved { moves } => (
                "moved",
                moves
                    .iter()
                    .map(|one| {
                        let from = if one.from.is_empty() { "/" } else { &one.from };
                        format!("{from} of {} to {}", &one.parent.to_hex()[..12], one.to)
                    })
                    .collect(),
            ),
        };
        const SHOWN: usize = 3;
        let mut text = format!(
            "{verb} {} {}",
            names.len(),
            if names.len() == 1 { "path" } else { "paths" }
        );
        for (index, name) in names.iter().take(SHOWN).enumerate() {
            text.push_str(if index == 0 { ": " } else { ", " });
            text.push_str(name);
        }
        if names.len() > SHOWN {
            text.push_str(&format!(", and {} more", names.len() - SHOWN));
        }
        text
    }
}

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

/// One version of an archive: everything it holds (`entries`) and, from v3, how it was
/// made. Its root is the BLAKE3 hash of its canonical bytes, which is what a signature
/// signs.
///
/// - v1 and v2 name at most one version they replace (`supersedes_archive_root`); why
///   was in sidecar logs beside the manifest file, outside the hash.
/// - v3 names its `parents` (none for a fresh capture, one for an edit, one per source
///   for a merge) and the `records` of the changes that made it from them, before its
///   entries, so the root covers them. `unverified_history` holds files attached as
///   found (the old sidecar logs); the root covers their bytes, nothing vouches for
///   their content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub format: String,
    pub archive_id: String,
    pub source_label: String,
    pub created_at_unix_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_archive_root: Option<CasHash>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<CasHash>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ArchiveRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_history: Vec<ArchiveAttachment>,
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

    fn path_mut(&mut self) -> &mut String {
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
    /// A v3 manifest with no parents and no records: a fresh capture.
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
            format: ARCHIVE_MANIFEST_FORMAT_V3.to_string(),
            archive_id,
            source_label,
            created_at_unix_secs,
            supersedes_archive_root: None,
            parents: Vec::new(),
            records: Vec::new(),
            unverified_history: Vec::new(),
            entries,
        })
    }

    /// The versions this one was made from: v3 `parents`, or the one root a v1 or v2
    /// manifest supersedes.
    #[must_use]
    pub fn parents(&self) -> Vec<CasHash> {
        self.supersedes_archive_root
            .into_iter()
            .chain(self.parents.iter().copied())
            .collect()
    }

    /// Every file entry's path and object.
    pub fn file_objects(&self) -> impl Iterator<Item = (&str, ArchiveObject)> {
        self.entries.iter().filter_map(|entry| match entry {
            ArchiveEntry::File { path, object, .. } => Some((path.as_str(), *object)),
            _ => None,
        })
    }

    /// The objects of [`Self::unverified_history`]. They are part of the version's
    /// record, so a purge keeps them even when it retires the version.
    pub fn attached_objects(&self) -> impl Iterator<Item = ArchiveObject> + '_ {
        self.unverified_history
            .iter()
            .map(|attached| attached.object)
    }

    /// The next version of this archive: `entries`, made from this one by `record`.
    fn successor(&self, entries: Vec<ArchiveEntry>, record: ArchiveRecord) -> Result<Self> {
        let parent = self.digest()?;
        let mut next = Self::new(
            self.archive_id.clone(),
            self.source_label.clone(),
            record.at_unix_secs,
            entries,
        )?;
        next.parents = vec![parent];
        next.records = vec![record];
        Ok(next)
    }

    /// One manifest holding every entry of `sources` (folder, manifest, its root), each
    /// source's entries under its folder (`under`, a relative path such as
    /// `Untitled/Documents`), with directory entries for those folders. Each source is a
    /// parent, and one `Moved` record says where each went. Objects are referenced as
    /// they are; nothing is copied, so the sources must live in the same CAS root as the
    /// merged manifest.
    ///
    /// # Errors
    /// Invalid id or label, a folder that is not a clean relative path, two sources
    /// under the same folder, a source entry colliding with another's, or an empty
    /// actor or reason.
    pub fn merged(
        archive_id: impl Into<String>,
        source_label: impl Into<String>,
        created_at_unix_secs: u64,
        sources: &[(&str, &ArchiveManifest, CasHash)],
        actor: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self> {
        let mut entries = Vec::new();
        let mut folders = std::collections::BTreeSet::new();
        let mut moves = Vec::new();
        for (under, source, root) in sources {
            let under = under.trim_matches('/');
            validate_relative_path(under)?;
            if !folders.insert(under.to_string()) {
                bail!("two sources are merged under {under:?}");
            }
            moves.push(ArchiveMove {
                parent: *root,
                from: String::new(),
                to: under.to_string(),
            });
            for entry in &source.entries {
                let mut entry = entry.clone();
                let path = format!("{under}/{}", entry.path());
                *entry.path_mut() = path;
                entries.push(entry);
            }
        }
        // Directory entries for every folder level the sources sit under.
        let mut parents = std::collections::BTreeSet::new();
        for folder in &folders {
            let mut at = String::new();
            for part in folder.split('/') {
                if !at.is_empty() {
                    at.push('/');
                }
                at.push_str(part);
                parents.insert(at.clone());
            }
        }
        for folder in parents {
            if entries.iter().any(|e| e.path() == folder) {
                continue;
            }
            entries.push(ArchiveEntry::Directory {
                path: folder,
                modified_at_unix_secs: None,
            });
        }
        let record = ArchiveRecord::new(
            ArchiveChange::Moved { moves },
            actor,
            reason,
            created_at_unix_secs,
        )?;
        let mut merged = Self::new(archive_id, source_label, created_at_unix_secs, entries)?;
        merged.parents = sources.iter().map(|(_, _, root)| *root).collect();
        merged.records = vec![record];
        Ok(merged)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let v1 = self.format == ARCHIVE_MANIFEST_FORMAT_V1;
        let v3 = self.format == ARCHIVE_MANIFEST_FORMAT_V3;
        if !v1 && !v3 && self.format != ARCHIVE_MANIFEST_FORMAT_V2 {
            bail!("unsupported archive manifest format {:?}", self.format);
        }
        if v1 && (!self.is_complete() || self.excluded_entry_count() > 0) {
            bail!("archive manifest v1 cannot represent unresolved or excluded source entries");
        }
        if v1 && self.supersedes_archive_root.is_some() {
            bail!("archive manifest v1 cannot represent a superseded archive root");
        }
        if v3 && self.supersedes_archive_root.is_some() {
            bail!("archive manifest v3 names its parents, not a superseded root");
        }
        if !v3
            && !(self.parents.is_empty()
                && self.records.is_empty()
                && self.unverified_history.is_empty())
        {
            bail!("only archive manifest v3 holds parents, change records or attachments");
        }
        let mut seen = std::collections::BTreeSet::new();
        if let Some(twice) = self.parents.iter().find(|parent| !seen.insert(**parent)) {
            bail!("parent {twice} is named twice");
        }
        for record in &self.records {
            record.validate()?;
        }
        for attached in &self.unverified_history {
            validate_relative_path(&attached.name)
                .with_context(|| format!("attachment name {:?}", attached.name))?;
            if attached.name.contains('/') {
                bail!("attachment name {:?} is not a file name", attached.name);
            }
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
    /// exclusion, producing a new immutable manifest made from this one by a `Changed`
    /// record.
    pub fn with_owner_approved_exclusion(
        &self,
        path: &str,
        reason: impl Into<String>,
        actor: impl Into<String>,
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
            reason: reason.clone(),
        };
        let record = ArchiveRecord::new(
            ArchiveChange::Changed {
                paths: vec![path.to_string()],
            },
            actor,
            reason,
            created_at_unix_secs,
        )?;
        self.successor(entries, record)
    }

    /// Produces a new manifest with `added` entries alongside every existing one, made
    /// from this one by a `Created` record naming them. Missing parent directories are
    /// created as directory entries (and named too); an added directory that already
    /// exists is kept as it is. A file, symlink or other entry whose path is already
    /// taken is refused: remove the old one first, so nothing is replaced silently. The
    /// objects the added files point at must already be stored (see
    /// [`ArchiveCasStorage::ingest_file`]).
    pub fn with_entries_added(
        &self,
        added: Vec<ArchiveEntry>,
        reason: impl Into<String>,
        actor: impl Into<String>,
        created_at_unix_secs: u64,
    ) -> Result<Self> {
        if added.is_empty() {
            bail!("nothing given to add");
        }
        let mut entries: std::collections::BTreeMap<String, ArchiveEntry> = self
            .entries
            .iter()
            .map(|entry| (entry.path().to_string(), entry.clone()))
            .collect();
        let mut added_paths = Vec::new();
        for entry in added {
            let path = entry.path().to_string();
            validate_relative_path(&path)?;
            // Parents first, as directories, where the archive has none.
            let mut parent = path.as_str();
            while let Some((up, _)) = parent.rsplit_once('/') {
                match entries.get(up) {
                    Some(ArchiveEntry::Directory { .. }) => {}
                    Some(_) => {
                        bail!("cannot add {path:?}: {up:?} is not a directory in the archive")
                    }
                    None => {
                        entries.insert(
                            up.to_string(),
                            ArchiveEntry::Directory {
                                path: up.to_string(),
                                modified_at_unix_secs: None,
                            },
                        );
                        added_paths.push(up.to_string());
                    }
                }
                parent = up;
            }
            match (entries.get(&path), &entry) {
                (Some(ArchiveEntry::Directory { .. }), ArchiveEntry::Directory { .. }) => continue,
                (Some(_), _) => bail!(
                    "{path:?} is already in the archive; remove it first (archive_cas_remove) to replace it"
                ),
                (None, _) => {
                    added_paths.push(path.clone());
                    entries.insert(path, entry);
                }
            }
        }
        if added_paths.is_empty() {
            bail!("everything given is already in the archive");
        }
        added_paths.sort();
        let record = ArchiveRecord::new(
            ArchiveChange::Created { paths: added_paths },
            actor,
            reason,
            created_at_unix_secs,
        )?;
        self.successor(entries.into_values().collect(), record)
    }

    /// Produces a new manifest with the named entries removed, made from this one by a
    /// `Deleted` record naming `paths` as given. A directory path also drops everything
    /// nested under it. This never touches blob objects or other manifest files -- it
    /// is a purely logical, append-only edit; reclaiming the disk space of any
    /// now-unreferenced blob is `archive_cas_purge`, once the new version is signed.
    pub fn with_entries_removed(
        &self,
        paths: &[String],
        reason: impl Into<String>,
        actor: impl Into<String>,
        created_at_unix_secs: u64,
    ) -> Result<Self> {
        if paths.is_empty() {
            bail!("no paths given to remove");
        }
        for requested in paths {
            if !self.entries.iter().any(|entry| entry.path() == requested) {
                bail!("path not present in manifest: {requested:?}");
            }
        }
        let directory_prefixes: Vec<String> = paths
            .iter()
            .filter(|requested| {
                self.entries.iter().any(|entry| {
                    entry.path() == requested.as_str()
                        && matches!(entry, ArchiveEntry::Directory { .. })
                })
            })
            .map(|requested| format!("{requested}/"))
            .collect();
        let drop_exact: std::collections::BTreeSet<&str> =
            paths.iter().map(String::as_str).collect();
        let is_removed = |path: &str| {
            drop_exact.contains(path)
                || directory_prefixes
                    .iter()
                    .any(|prefix| path.starts_with(prefix.as_str()))
        };
        let remaining: Vec<ArchiveEntry> = self
            .entries
            .iter()
            .filter(|entry| !is_removed(entry.path()))
            .cloned()
            .collect();
        if remaining.len() == self.entries.len() {
            bail!("removal selection matched no manifest entries");
        }
        let mut named: Vec<String> = drop_exact.into_iter().map(str::to_string).collect();
        named.sort();
        let record = ArchiveRecord::new(
            ArchiveChange::Deleted { paths: named },
            actor,
            reason,
            created_at_unix_secs,
        )?;
        self.successor(remaining, record)
    }

    /// The same entries as a v3 version made from this one with no change, carrying
    /// `attachments` as [`Self::unverified_history`]: how an archive written before v3
    /// keeps its old sidecar logs inside the store.
    ///
    /// # Errors
    /// No attachments, or one without a name.
    pub fn with_unverified_history(
        &self,
        attachments: Vec<ArchiveAttachment>,
        created_at_unix_secs: u64,
    ) -> Result<Self> {
        if attachments.is_empty() {
            bail!("nothing to attach");
        }
        let parent = self.digest()?;
        let mut next = Self::new(
            self.archive_id.clone(),
            self.source_label.clone(),
            created_at_unix_secs,
            self.entries.clone(),
        )?;
        next.parents = vec![parent];
        next.unverified_history = attachments;
        next.canonical_bytes()?;
        Ok(next)
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
    /// The zstd level new objects are compressed at, when this root compresses.
    compression_level: Option<i32>,
}

/// Why a compression attempt kept an object as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptRaw {
    /// Smaller than [`MIN_COMPRESSED_OBJECT_BYTES`].
    Small,
    /// Compressing saved too little (less than a sixteenth, or less than 4 KiB).
    Incompressible,
}

/// The uncompressed bytes of a stored object, whichever form it is stored in.
pub enum ObjectReader {
    Raw(File),
    Zstd(Box<zstd::stream::read::Decoder<'static, std::io::BufReader<File>>>),
}

impl Read for ObjectReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Raw(file) => file.read(buffer),
            Self::Zstd(decoder) => decoder.read(buffer),
        }
    }
}

/// A stored object as a seekable file of its uncompressed bytes: the object itself, or
/// a temporary decompressed copy under `partials/` that is removed on drop.
pub struct SeekableObject {
    file: File,
    temporary: Option<PathBuf>,
}

impl SeekableObject {
    pub fn file(&mut self) -> &mut File {
        &mut self.file
    }
}

impl Drop for SeekableObject {
    fn drop(&mut self) {
        if let Some(temporary) = &self.temporary {
            let _ = fs::remove_file(temporary);
        }
    }
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
        let compression_level = Self::read_compression(&root)?.map(|setting| setting.level);
        Ok(Self {
            root,
            objects,
            partials,
            manifests,
            buffer_bytes,
            compression_level,
        })
    }

    fn read_compression(root: &Path) -> Result<Option<ArchiveCompression>> {
        let path = root.join(COMPRESSION_SETTINGS_FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()))
            }
        };
        let setting: ArchiveCompression = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a compression setting", path.display()))?;
        if setting.format != ARCHIVE_COMPRESSION_FORMAT_V1 || setting.codec != "zstd" {
            bail!(
                "{} names {} / {}; this build reads {ARCHIVE_COMPRESSION_FORMAT_V1} / zstd",
                path.display(),
                setting.format,
                setting.codec
            );
        }
        if !zstd::compression_level_range().contains(&setting.level) {
            bail!(
                "{} has zstd level {} out of range",
                path.display(),
                setting.level
            );
        }
        Ok(Some(setting))
    }

    /// The zstd level new objects are compressed at, or `None` when this root stores
    /// them as they are.
    pub fn compression_level(&self) -> Option<i32> {
        self.compression_level
    }

    /// Turns compression of new objects on at `level` (writes `compression.json`), or
    /// off with `None`. Objects already stored keep their form; readers take both.
    pub fn set_compression(&mut self, level: Option<i32>) -> Result<()> {
        let path = self.root.join(COMPRESSION_SETTINGS_FILE);
        match level {
            Some(level) => {
                if !zstd::compression_level_range().contains(&level) {
                    bail!(
                        "zstd level {level} is outside {:?}",
                        zstd::compression_level_range()
                    );
                }
                let setting = ArchiveCompression {
                    format: ARCHIVE_COMPRESSION_FORMAT_V1.into(),
                    codec: "zstd".into(),
                    level,
                };
                let temporary = self.partials.join(format!(
                    ".compression-{}-{}.partial",
                    std::process::id(),
                    unique_suffix()
                ));
                write_synced_file(&temporary, &serde_json::to_vec_pretty(&setting)?)?;
                fs::rename(&temporary, &path)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                sync_parent(&path)?;
            }
            None => match fs::remove_file(&path) {
                Ok(()) => sync_parent(&path)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            },
        }
        self.compression_level = level;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifests_root(&self) -> &Path {
        &self.manifests
    }

    /// Where the object is stored as it is (`.blob`). It may instead be stored
    /// compressed ([`Self::compressed_object_path`]); read it with [`Self::open_object`].
    pub fn object_path(&self, hash: CasHash) -> PathBuf {
        let hex = hash.to_hex();
        self.objects.join(&hex[..2]).join(format!("{hex}.blob"))
    }

    /// A fresh, unused path under `partials/` for a temporary file.
    pub fn scratch_path(&self, label: &str) -> PathBuf {
        self.partials.join(format!(
            ".{label}-{}-{}.partial",
            std::process::id(),
            unique_suffix()
        ))
    }

    /// Where the object is stored when compressed (`.zst`).
    pub fn compressed_object_path(&self, hash: CasHash) -> PathBuf {
        let hex = hash.to_hex();
        self.objects.join(&hex[..2]).join(format!("{hex}.zst"))
    }

    /// Whether the object is stored, in either form.
    pub fn has_object(&self, hash: CasHash) -> bool {
        self.object_path(hash).exists() || self.compressed_object_path(hash).exists()
    }

    /// Bytes the object takes on disk: both forms together while a compression pass
    /// that stopped between them has left both.
    pub fn stored_bytes(&self, hash: CasHash) -> Result<u64> {
        let mut total = 0;
        let mut found = false;
        for path in [self.object_path(hash), self.compressed_object_path(hash)] {
            match fs::metadata(&path) {
                Ok(metadata) => {
                    total += metadata.len();
                    found = true;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if !found {
            bail!("archive object is missing: {hash}");
        }
        Ok(total)
    }

    /// The object's uncompressed bytes: the `.blob` when there is one, otherwise the
    /// `.zst` decompressed as it is read. Not verified here; callers hash what they read.
    pub fn open_object(&self, hash: CasHash) -> Result<ObjectReader> {
        let raw = self.object_path(hash);
        match File::open(&raw) {
            Ok(file) => return Ok(ObjectReader::Raw(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to open {}", raw.display()))
            }
        }
        let compressed = self.compressed_object_path(hash);
        let file = File::open(&compressed).with_context(|| {
            format!(
                "archive object is missing: {} (nor {})",
                raw.display(),
                compressed.display()
            )
        })?;
        Ok(ObjectReader::Zstd(Box::new(
            zstd::stream::read::Decoder::new(file)
                .with_context(|| format!("failed to read {}", compressed.display()))?,
        )))
    }

    /// The object as a seekable file of its uncompressed bytes, for readers such as the
    /// zip reader that need to seek. A compressed object is decompressed into a
    /// temporary file first (and the copy checked against its hash).
    pub fn open_object_seekable(&self, expected: ArchiveObject) -> Result<SeekableObject> {
        if let Ok(file) = File::open(self.object_path(expected.hash)) {
            return Ok(SeekableObject {
                file,
                temporary: None,
            });
        }
        let temporary = self.partials.join(format!(
            ".decompressed-{}-{}.partial",
            std::process::id(),
            unique_suffix()
        ));
        let mut seekable = SeekableObject {
            file: File::create(&temporary)
                .with_context(|| format!("failed to create {}", temporary.display()))?,
            temporary: Some(temporary),
        };
        let mut reader = self.open_object(expected.hash)?;
        let (hash, size) = self.copy_hashing(&mut reader, &mut seekable.file)?;
        check_object(expected, hash, size)?;
        seekable.file.seek(SeekFrom::Start(0))?;
        Ok(seekable)
    }

    /// Copies all of `reader` into `writer`, returning the BLAKE3 hash and length.
    fn copy_hashing(
        &self,
        reader: &mut dyn Read,
        writer: &mut dyn Write,
    ) -> Result<(CasHash, u64)> {
        let mut buffer = vec![0_u8; self.buffer_bytes];
        let mut hasher = blake3::Hasher::new();
        let mut size = 0_u64;
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            hasher.update(&buffer[..count]);
            writer.write_all(&buffer[..count])?;
            size += count as u64;
        }
        Ok((CasHash::from_bytes(*hasher.finalize().as_bytes()), size))
    }

    /// Compresses the uncompressed object bytes in `raw` into a new file `out` when that
    /// pays, checking that `raw` really holds `expected` as it goes and that `out`
    /// decompresses back to it. Returns the compressed length, or why the object stays
    /// as it is (then `out` does not exist).
    ///
    /// # Errors
    /// When `raw` does not hash to `expected` (nothing is written), or on I/O failure.
    pub fn compress_to(
        &self,
        raw: &Path,
        expected: ArchiveObject,
        level: i32,
        out: &Path,
    ) -> Result<std::result::Result<u64, KeptRaw>> {
        if expected.size < MIN_COMPRESSED_OBJECT_BYTES {
            return Ok(Err(KeptRaw::Small));
        }
        let mut source =
            File::open(raw).with_context(|| format!("failed to open {}", raw.display()))?;
        if expected.size > COMPRESSION_SAMPLE_BYTES as u64 {
            // A fast look at the start: media and archives are already compressed.
            let mut sample = vec![0_u8; COMPRESSION_SAMPLE_BYTES];
            source.read_exact(&mut sample)?;
            let packed = zstd::bulk::compress(&sample, 1)?;
            if packed.len() as u64 * 100 > sample.len() as u64 * 97 {
                return Ok(Err(KeptRaw::Incompressible));
            }
            source.seek(SeekFrom::Start(0))?;
        }
        let result = (|| -> Result<std::result::Result<u64, KeptRaw>> {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(out)
                .with_context(|| format!("failed to create {}", out.display()))?;
            let mut encoder = zstd::stream::write::Encoder::new(file, level)?;
            encoder.include_checksum(true)?;
            encoder.set_pledged_src_size(Some(expected.size))?;
            let (hash, size) = self.copy_hashing(&mut source, &mut encoder)?;
            check_object(expected, hash, size)
                .with_context(|| format!("{} is damaged; not compressed", raw.display()))?;
            encoder.finish()?.sync_all()?;
            let compressed = fs::metadata(out)?.len();
            let saved = expected.size.saturating_sub(compressed);
            if saved < expected.size / 16 || saved < 4096 {
                fs::remove_file(out)?;
                return Ok(Err(KeptRaw::Incompressible));
            }
            let mut decoder = zstd::stream::read::Decoder::new(File::open(out)?)?;
            let (hash, size) = self.copy_hashing(&mut decoder, &mut std::io::sink())?;
            check_object(expected, hash, size)
                .context("compressed copy does not decompress to the object")?;
            Ok(Ok(compressed))
        })();
        if !matches!(result, Ok(Ok(_))) {
            let _ = fs::remove_file(out);
        }
        result
    }

    /// For a new object whose uncompressed bytes are in `partial`: what to publish and
    /// where. When this root compresses and it pays, a verified compressed copy (the
    /// partial is then removed) as `.zst`; otherwise the partial itself as `.blob`.
    fn prepare_publish(&self, partial: &Path, object: ArchiveObject) -> Result<(PathBuf, PathBuf)> {
        if let Some(level) = self.compression_level {
            let compressed = self.partials.join(format!(
                ".compressed-{}-{}.partial",
                std::process::id(),
                unique_suffix()
            ));
            if self
                .compress_to(partial, object, level, &compressed)?
                .is_ok()
            {
                fs::remove_file(partial)?;
                return Ok((compressed, self.compressed_object_path(object.hash)));
            }
        }
        Ok((partial.to_path_buf(), self.object_path(object.hash)))
    }

    /// Stores an object already present as a `.blob` compressed instead, when that pays:
    /// the verified `.zst` is published first, then the `.blob` removed, so the object
    /// is readable throughout. Returns `(bytes before, bytes after)`, or why it stays.
    pub fn compress_stored_object(
        &self,
        expected: ArchiveObject,
        level: i32,
    ) -> Result<std::result::Result<(u64, u64), KeptRaw>> {
        let raw = self.object_path(expected.hash);
        let compressed_path = self.compressed_object_path(expected.hash);
        let before = fs::metadata(&raw)
            .with_context(|| format!("archive object is missing: {}", raw.display()))?
            .len();
        if compressed_path.exists() {
            // A pass stopped between publishing the `.zst` and removing the `.blob`.
            self.verify_compressed(expected)?;
            fs::remove_file(&raw)?;
            sync_parent(&raw)?;
            return Ok(Ok((before, fs::metadata(&compressed_path)?.len())));
        }
        let temporary = self.partials.join(format!(
            ".compressed-{}-{}.partial",
            std::process::id(),
            unique_suffix()
        ));
        let after = match self.compress_to(&raw, expected, level, &temporary)? {
            Ok(after) => after,
            Err(kept) => return Ok(Err(kept)),
        };
        let published = fs::hard_link(&temporary, &compressed_path);
        let _ = fs::remove_file(&temporary);
        published.with_context(|| format!("failed to publish {}", compressed_path.display()))?;
        sync_parent(&compressed_path)?;
        fs::remove_file(&raw).with_context(|| format!("failed to remove {}", raw.display()))?;
        sync_parent(&raw)?;
        Ok(Ok((before, after)))
    }

    fn verify_compressed(&self, expected: ArchiveObject) -> Result<()> {
        let path = self.compressed_object_path(expected.hash);
        let mut decoder = zstd::stream::read::Decoder::new(
            File::open(&path).with_context(|| format!("failed to open {}", path.display()))?,
        )?;
        let (hash, size) = self.copy_hashing(&mut decoder, &mut std::io::sink())?;
        check_object(expected, hash, size).with_context(|| format!("{}", path.display()))
    }

    /// Adds an in-memory control object, such as a manifest, to the archive.
    ///
    /// Large source files should use [`Self::ingest_file`]. This helper is for
    /// small, generated objects whose bytes are already available in memory.
    /// Stores whatever `reader` yields, hashing it while it is written to a partial
    /// file, so content of any size streams through a bounded buffer. When the object
    /// already exists with the same size, the partial is discarded (identical content
    /// hashes the same; `archive_cas_verify` re-checks stored bytes).
    pub fn add_stream(&self, reader: &mut dyn Read) -> Result<ArchiveIngestResult> {
        let temporary = self.partials.join(format!(
            ".stream-{}-{}.partial",
            std::process::id(),
            unique_suffix()
        ));
        let mut hasher = blake3::Hasher::new();
        let mut size = 0u64;
        {
            let mut file = File::create(&temporary)
                .with_context(|| format!("failed to create {}", temporary.display()))?;
            let mut buffer = vec![0u8; self.buffer_bytes.max(64 * 1024)];
            loop {
                let read = match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = fs::remove_file(&temporary);
                        return Err(error).context("failed to read content to store");
                    }
                };
                hasher.update(&buffer[..read]);
                file.write_all(&buffer[..read])?;
                size += read as u64;
            }
            file.sync_all()?;
        }
        let object = ArchiveObject {
            hash: CasHash::from_bytes(*hasher.finalize().as_bytes()),
            size,
        };
        let object_path = self.object_path(object.hash);
        self.ensure_object_parent(&object_path)?;
        let existing = |path: &Path| -> Result<bool> {
            match fs::metadata(path) {
                Ok(metadata) if metadata.len() == size => Ok(true),
                Ok(metadata) => bail!(
                    "stored object {} is {} bytes, expected {size}",
                    object.hash,
                    metadata.len()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.into()),
            }
        };
        if self.compressed_object_path(object.hash).exists() || existing(&object_path)? {
            fs::remove_file(&temporary)?;
            return Ok(ArchiveIngestResult {
                object,
                inserted: false,
                resumed_bytes: 0,
            });
        }
        let (source, target) = match self.prepare_publish(&temporary, object) {
            Ok(publish) => publish,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        match fs::hard_link(&source, &target) {
            Ok(()) => {
                sync_parent(&target)?;
                fs::remove_file(&source)?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: true,
                    resumed_bytes: 0,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if target == object_path {
                    existing(&object_path)?;
                }
                fs::remove_file(&source)?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: false,
                    resumed_bytes: 0,
                })
            }
            Err(error) => {
                let _ = fs::remove_file(&source);
                Err(error).context("failed to publish archive content object")
            }
        }
    }

    pub fn add_content(&self, bytes: &[u8]) -> Result<ArchiveIngestResult> {
        let hash = CasHash::digest(bytes);
        let object = ArchiveObject {
            hash,
            size: u64::try_from(bytes.len()).context("content length exceeds u64")?,
        };
        let object_path = self.object_path(hash);
        self.ensure_object_parent(&object_path)?;

        if self.has_object(hash) {
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
        let (source, target) = match self.prepare_publish(&temporary, object) {
            Ok(publish) => publish,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        match fs::hard_link(&source, &target) {
            Ok(()) => {
                sync_parent(&target)?;
                fs::remove_file(&source)?;
                sync_parent(&source)?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: true,
                    resumed_bytes: 0,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.verify_object(object)?;
                fs::remove_file(&source)?;
                Ok(ArchiveIngestResult {
                    object,
                    inserted: false,
                    resumed_bytes: 0,
                })
            }
            Err(error) => {
                let _ = fs::remove_file(&source);
                Err(error).context("failed to publish archive content object")
            }
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

        if self.has_object(hash) {
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

        // A compressed copy replaces the resumable partial; if publishing then fails,
        // the next ingest of this source starts over.
        let (partial_path, object_path) = self.prepare_publish(&partial_path, object)?;
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

    /// Checks the object's uncompressed bytes against its hash and size, whichever
    /// form it is stored in.
    pub fn verify_object(&self, expected: ArchiveObject) -> Result<()> {
        let path = self.object_path(expected.hash);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() != expected.size => bail!(
                "archive object size mismatch for {}: expected {}, got {}",
                expected.hash,
                expected.size,
                metadata.len()
            ),
            Ok(_) => {
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.verify_compressed(expected)
            }
            Err(error) => Err(error).with_context(|| format!("failed to stat {}", path.display())),
        }
    }

    /// Up to `size` uncompressed bytes from `offset`. A compressed object is decompressed
    /// from its start, so reads far into a large one cost time in proportion.
    pub fn read_range(&self, hash: CasHash, offset: u64, size: usize) -> Result<Vec<u8>> {
        let path = self.object_path(hash);
        let length = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut reader = self.open_object(hash)?;
                let skipped = std::io::copy(&mut (&mut reader).take(offset), &mut std::io::sink())?;
                if skipped < offset {
                    bail!("offset {offset} past end of archive object {hash}");
                }
                let mut bytes = Vec::with_capacity(size.min(self.buffer_bytes));
                reader
                    .take(u64::try_from(size).unwrap_or(u64::MAX))
                    .read_to_end(&mut bytes)?;
                return Ok(bytes);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to stat {}", path.display()))
            }
        };
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
        self.read_manifest_and_root(path)
            .map(|(manifest, _)| manifest)
    }

    /// [`Self::read_manifest`] plus the manifest's root hash. A manifest on
    /// disk is its own canonical bytes, so the root is the hash of the file as
    /// read, without serializing the manifest a second time.
    pub fn read_manifest_and_root(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<(ArchiveManifest, CasHash)> {
        let path = path.as_ref();
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let manifest: ArchiveManifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let canonical = manifest.canonical_bytes()?;
        if canonical != bytes {
            bail!("archive manifest is not canonical: {}", path.display());
        }
        Ok((manifest, CasHash::digest(&bytes)))
    }

    /// Every canonical manifest currently readable under `manifests/`, newest
    /// paths mixed with old ones -- callers that need "what's still live"
    /// (GC) must include every manifest here, since blobs are globally
    /// deduplicated across archives with no refcounting.
    pub fn list_manifests(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.manifests)
            .with_context(|| format!("failed to enumerate {}", self.manifests.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("json")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.ends_with(".signature.json")
                            || LEGACY_LOG_SUFFIXES
                                .iter()
                                .any(|suffix| name.ends_with(suffix))
                    })
            {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    /// Removes one blob object. Callers must have already established, by
    /// scanning every manifest in the CAS root (see [`Self::list_manifests`]),
    /// that no manifest anywhere references this hash. This performs no such
    /// check itself -- it is the low-level primitive a GC sweep calls once
    /// per confirmed-orphaned object.
    /// Returns the bytes freed, counting both forms if both are stored.
    pub fn remove_object(&self, hash: CasHash) -> Result<u64> {
        let size = self.stored_bytes(hash)?;
        for path in [self.object_path(hash), self.compressed_object_path(hash)] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to remove archive object {}", path.display())
                    })
                }
            }
        }
        Ok(size)
    }

    /// Every blob currently stored under `objects/`, as `(hash, size)`. A GC
    /// sweep computes the referenced set from [`Self::list_manifests`] and
    /// treats everything here that isn't in that set as orphaned.
    pub fn list_objects(&self) -> Result<Vec<(CasHash, u64)>> {
        self.list_objects_with_progress(|_| {})
    }

    /// Same as [`Self::list_objects`], but calls `on_progress(count)` after
    /// every object found -- this is a full directory walk plus one
    /// `metadata()` stat per object, which on a large CAS (hundreds of
    /// thousands of blobs) on slow or removable storage can take long
    /// enough that a caller needs its own sense of progress. Callers know
    /// the *expected* object count cheaply and in advance, from summing
    /// `unique_objects` across the manifests they've already read -- this
    /// is only the (slow) confirmation walk against what's actually on
    /// disk, not a source of the expected total itself.
    pub fn list_objects_with_progress(
        &self,
        mut on_progress: impl FnMut(usize),
    ) -> Result<Vec<(CasHash, u64)>> {
        let mut objects = Vec::new();
        for shard in fs::read_dir(&self.objects)
            .with_context(|| format!("failed to enumerate {}", self.objects.display()))?
        {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for blob in fs::read_dir(shard.path())
                .with_context(|| format!("failed to enumerate {}", shard.path().display()))?
            {
                let blob = blob?;
                let path = blob.path();
                let Some(stem) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(object_stem)
                else {
                    continue;
                };
                let bytes = hex::decode(stem)
                    .with_context(|| format!("non-hex object file name: {}", path.display()))?;
                let hash = CasHash::from_slice(&bytes)
                    .with_context(|| format!("invalid object hash: {}", path.display()))?;
                let size = blob.metadata()?.len();
                objects.push((hash, size));
                on_progress(objects.len());
            }
        }
        objects.sort_by_key(|(hash, _)| *hash);
        // An object stored in both forms is one object taking both sizes.
        objects.dedup_by(|later, earlier| {
            let same = later.0 == earlier.0;
            if same {
                earlier.1 += later.1;
            }
            same
        });
        Ok(objects)
    }

    /// The hash of every blob under `objects/`, from file names alone. No
    /// per-object `stat`: on a spinning or USB drive that stat is what makes a
    /// full walk take hours (about 90 objects a second on a 588k-object root),
    /// while names come back a directory at a time. `on_progress(count)` is
    /// called once per shard directory.
    pub fn object_hashes_with_progress(
        &self,
        mut on_progress: impl FnMut(usize),
    ) -> Result<Vec<CasHash>> {
        let mut hashes = Vec::new();
        for shard in fs::read_dir(&self.objects)
            .with_context(|| format!("failed to enumerate {}", self.objects.display()))?
        {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for blob in fs::read_dir(shard.path())
                .with_context(|| format!("failed to enumerate {}", shard.path().display()))?
            {
                let name = blob?.file_name();
                let Some(stem) = name.to_str().and_then(object_stem) else {
                    continue;
                };
                let bytes = hex::decode(stem)
                    .with_context(|| format!("non-hex object file name: {stem}"))?;
                hashes.push(
                    CasHash::from_slice(&bytes)
                        .with_context(|| format!("invalid object hash: {stem}"))?,
                );
            }
            on_progress(hashes.len());
        }
        hashes.sort();
        hashes.dedup();
        Ok(hashes)
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
        if let Ok(source_metadata) = fs::metadata(&source_path) {
            if source_metadata.len() != expected.size {
                bail!(
                    "archive object size mismatch for {}: expected {}, got {}",
                    expected.hash,
                    expected.size,
                    source_metadata.len()
                );
            }
        }

        let mut source = self.open_object(expected.hash)?;
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

/// The hash part of an object file name, `<hash>.blob` or `<hash>.zst`.
fn object_stem(name: &str) -> Option<&str> {
    name.strip_suffix(".blob")
        .or_else(|| name.strip_suffix(".zst"))
}

fn check_object(expected: ArchiveObject, hash: CasHash, size: u64) -> Result<()> {
    if size != expected.size {
        bail!(
            "archive object size mismatch for {}: expected {}, got {size}",
            expected.hash,
            expected.size
        );
    }
    if hash != expected.hash {
        bail!(
            "archive object hash mismatch: expected {}, got {hash}",
            expected.hash
        );
    }
    Ok(())
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

/// A suffix no other call in this process returns: the clock alone repeats across
/// threads (macOS reports whole microseconds), so a counter is folded in.
fn unique_suffix() -> u128 {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (nanos << 32) | u128::from(count)
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
    #[test]
    fn temporary_names_do_not_repeat_across_threads() {
        let names: Vec<u128> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| (0..5_000).map(|_| unique_suffix()).collect::<Vec<_>>()))
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap())
                .collect()
        });
        let distinct: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(distinct.len(), names.len());
    }

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

    fn file_entry(store: &ArchiveCasStorage, path: &str, bytes: &[u8]) -> ArchiveEntry {
        let object = store.add_content(bytes).unwrap().object;
        ArchiveEntry::File {
            path: path.to_string(),
            object,
            modified_at_unix_secs: None,
        }
    }

    #[test]
    fn with_entries_removed_drops_a_single_file_and_records_why() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let entries = vec![
            file_entry(&store, "keep.txt", b"keep"),
            file_entry(&store, "drop.txt", b"drop"),
        ];
        let manifest = ArchiveManifest::new("archive-a", "test", 1, entries).unwrap();
        let base_root = manifest.digest().unwrap();

        let amended = manifest
            .with_entries_removed(&["drop.txt".to_string()], "cleanup", "jay", 2)
            .unwrap();

        assert_eq!(amended.entries.len(), 1);
        assert_eq!(amended.entries[0].path(), "keep.txt");
        assert_eq!(amended.format, ARCHIVE_MANIFEST_FORMAT_V3);
        assert_eq!(amended.parents(), vec![base_root]);
        assert_eq!(amended.supersedes_archive_root, None);
        assert_eq!(
            amended.records,
            vec![ArchiveRecord {
                actor: "jay".into(),
                at_unix_secs: 2,
                reason: "cleanup".into(),
                change: ArchiveChange::Deleted {
                    paths: vec!["drop.txt".into()]
                },
            }]
        );
        // The record is inside the root: the same entries with another reason are
        // another version.
        let other = manifest
            .with_entries_removed(&["drop.txt".to_string()], "other", "jay", 2)
            .unwrap();
        assert_eq!(other.entries, amended.entries);
        assert_ne!(other.digest().unwrap(), amended.digest().unwrap());
        assert!(manifest
            .with_entries_removed(&["drop.txt".to_string()], " ", "jay", 2)
            .is_err());
        assert!(manifest
            .with_entries_removed(&["drop.txt".to_string()], "cleanup", "", 2)
            .is_err());
    }

    #[test]
    fn with_entries_removed_drops_a_directory_recursively() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let entries = vec![
            ArchiveEntry::Directory {
                path: "old".to_string(),
                modified_at_unix_secs: None,
            },
            file_entry(&store, "old/a.txt", b"a"),
            file_entry(&store, "old/nested/b.txt", b"b"),
            file_entry(&store, "keep.txt", b"keep"),
        ];
        let manifest = ArchiveManifest::new("archive-a", "test", 1, entries).unwrap();

        let amended = manifest
            .with_entries_removed(&["old".to_string()], "cleanup", "jay", 2)
            .unwrap();

        assert_eq!(amended.entries.len(), 1);
        assert_eq!(amended.entries[0].path(), "keep.txt");
        // One path for the directory, not one per entry below it.
        assert_eq!(
            amended.records[0].change,
            ArchiveChange::Deleted {
                paths: vec!["old".into()]
            }
        );
    }

    #[test]
    fn with_entries_removed_rejects_an_absent_path() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let entries = vec![file_entry(&store, "keep.txt", b"keep")];
        let manifest = ArchiveManifest::new("archive-a", "test", 1, entries).unwrap();
        let error = manifest
            .with_entries_removed(&["missing.txt".to_string()], "cleanup", "jay", 2)
            .unwrap_err();
        assert!(error.to_string().contains("not present in manifest"));
    }

    #[test]
    fn gc_reference_scan_keeps_a_blob_shared_by_another_manifest() {
        // This mirrors what archive_cas_gc does: union the referenced blobs
        // across every manifest in the CAS root before deciding what's
        // orphaned. A blob two archives share must survive removing it from
        // just one of them.
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let shared_object = store.add_content(b"shared bytes").unwrap().object;

        let manifest_a = ArchiveManifest::new(
            "archive-a",
            "test",
            1,
            vec![ArchiveEntry::File {
                path: "shared.bin".to_string(),
                object: shared_object,
                modified_at_unix_secs: None,
            }],
        )
        .unwrap();
        let manifest_b = ArchiveManifest::new(
            "archive-b",
            "test",
            1,
            vec![ArchiveEntry::File {
                path: "also-shared.bin".to_string(),
                object: shared_object,
                modified_at_unix_secs: None,
            }],
        )
        .unwrap();
        store.write_manifest(&manifest_a).unwrap();
        store.write_manifest(&manifest_b).unwrap();

        // Remove archive-a's only entry -- archive-b still references the
        // same blob.
        let amended_a = manifest_a
            .with_entries_removed(&["shared.bin".to_string()], "cleanup", "jay", 2)
            .unwrap();
        assert!(amended_a.entries.is_empty());
        store.write_manifest(&amended_a).unwrap();

        let mut referenced = std::collections::BTreeSet::new();
        for path in store.list_manifests().unwrap() {
            let manifest = store.read_manifest(&path).unwrap();
            referenced.insert(manifest.digest().unwrap());
            for entry in &manifest.entries {
                if let ArchiveEntry::File { object, .. } = entry {
                    referenced.insert(object.hash);
                }
            }
        }
        assert!(
            referenced.contains(&shared_object.hash),
            "blob still referenced by archive-b's original manifest must not look orphaned"
        );

        let objects = store.list_objects().unwrap();
        assert!(objects.iter().any(|(hash, _)| *hash == shared_object.hash));
    }

    #[test]
    fn adding_entries_supersedes_the_manifest_and_creates_parents() {
        let object = ArchiveObject {
            hash: CasHash::digest(b"new"),
            size: 3,
        };
        let base = ArchiveManifest::new(
            "added-test",
            "test",
            1,
            vec![ArchiveEntry::Directory {
                path: "old".into(),
                modified_at_unix_secs: None,
            }],
        )
        .unwrap();
        let file = |path: &str| ArchiveEntry::File {
            path: path.into(),
            object,
            modified_at_unix_secs: Some(2),
        };
        let amended = base
            .with_entries_added(vec![file("added/today/export.zip")], "export", "jay", 5)
            .unwrap();
        assert_eq!(amended.parents(), vec![base.digest().unwrap()]);
        assert_eq!(
            amended.records[0].change,
            ArchiveChange::Created {
                paths: vec![
                    "added".into(),
                    "added/today".into(),
                    "added/today/export.zip".into()
                ]
            }
        );
        let paths: Vec<&str> = amended.entries.iter().map(ArchiveEntry::path).collect();
        assert_eq!(
            paths,
            ["added", "added/today", "added/today/export.zip", "old"]
        );
        // A taken path is refused rather than replaced.
        let again = amended.with_entries_added(vec![file("added/today/export.zip")], "x", "jay", 6);
        assert!(again
            .unwrap_err()
            .to_string()
            .contains("already in the archive"));
        // Adding under an existing directory keeps it.
        let more = amended
            .with_entries_added(vec![file("old/more.txt")], "x", "jay", 7)
            .unwrap();
        assert_eq!(
            more.records[0].change,
            ArchiveChange::Created {
                paths: vec!["old/more.txt".into()]
            }
        );
        assert!(base.with_entries_added(vec![], "x", "jay", 8).is_err());
        assert!(base
            .with_entries_added(vec![file("a")], " ", "jay", 8)
            .is_err());
    }

    #[test]
    fn list_manifests_excludes_old_logs_and_signature_sidecars() {
        // A real bug: list_manifests originally excluded only
        // `.delete-log.json`, not `.signature.json` -- both of which also
        // end in `.json` and sit in the same directory next to the manifest
        // they describe. Once a manifest was actually signed (the normal,
        // expected state for anything worth pruning/GC-ing), every caller
        // of list_manifests -- archive_cas_gc's reference scan and the
        // (since removed) archive_cas_prune_manifests's ancestor walk -- tried to parse the
        // signature file as a manifest and failed outright.
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let entries = vec![file_entry(&store, "a.txt", b"a")];
        let manifest = ArchiveManifest::new("archive-a", "test", 1, entries).unwrap();
        let (manifest_path, _) = store.write_manifest(&manifest).unwrap();
        let stem = manifest_path.file_stem().unwrap().to_str().unwrap();
        for suffix in LEGACY_LOG_SUFFIXES {
            fs::write(
                manifest_path.with_file_name(format!("{stem}{suffix}")),
                b"{\"format\":\"loadngo-archive-delete-log-v1\"}",
            )
            .unwrap();
        }
        let signature_path = manifest_path.with_file_name(format!("{stem}.signature.json"));
        fs::write(&signature_path, b"{\"not\":\"a manifest\"}").unwrap();

        let listed = store.list_manifests().unwrap();
        assert_eq!(listed, vec![manifest_path]);
    }

    #[test]
    fn list_objects_with_progress_reports_every_object_exactly_once() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        store.add_content(b"one").unwrap();
        store.add_content(b"two").unwrap();
        store.add_content(b"three").unwrap();

        let mut progress_calls = Vec::new();
        let objects = store
            .list_objects_with_progress(|found| progress_calls.push(found))
            .unwrap();

        assert_eq!(objects.len(), 3);
        // One callback per object, strictly increasing, ending at the total.
        assert_eq!(progress_calls, vec![1, 2, 3]);
    }

    #[test]
    fn merged_puts_each_archive_under_its_folder_and_references_the_same_objects() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let note = store.add_content(b"note").unwrap().object;
        let song = store.add_content(b"song").unwrap().object;
        let docs = ArchiveManifest::new(
            "docs",
            "Documents",
            1,
            vec![
                ArchiveEntry::Directory {
                    path: "a".into(),
                    modified_at_unix_secs: Some(5),
                },
                ArchiveEntry::File {
                    path: "a/note.txt".into(),
                    object: note,
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::Excluded {
                    path: "a/big.ipa".into(),
                    reason: "owner-approved".into(),
                },
            ],
        )
        .unwrap();
        let music = ArchiveManifest::new(
            "music",
            "Music",
            2,
            vec![ArchiveEntry::File {
                path: "song.ogg".into(),
                object: song,
                modified_at_unix_secs: None,
            }],
        )
        .unwrap();
        let docs_root = docs.digest().unwrap();
        let music_root = music.digest().unwrap();
        let merged = ArchiveManifest::merged(
            "all",
            "Everything",
            3,
            &[
                ("Untitled/Documents", &docs, docs_root),
                ("Music", &music, music_root),
            ],
            "jay",
            "one archive",
        )
        .unwrap();
        assert_eq!(merged.parents(), vec![docs_root, music_root]);
        assert_eq!(
            merged.records[0].change,
            ArchiveChange::Moved {
                moves: vec![
                    ArchiveMove {
                        parent: docs_root,
                        from: String::new(),
                        to: "Untitled/Documents".into(),
                    },
                    ArchiveMove {
                        parent: music_root,
                        from: String::new(),
                        to: "Music".into(),
                    },
                ]
            }
        );
        let paths: Vec<&str> = merged.entries.iter().map(ArchiveEntry::path).collect();
        assert_eq!(
            paths,
            [
                "Music",
                "Music/song.ogg",
                "Untitled",
                "Untitled/Documents",
                "Untitled/Documents/a",
                "Untitled/Documents/a/big.ipa",
                "Untitled/Documents/a/note.txt",
            ]
        );
        assert_eq!(merged.file_count(), 2);
        assert_eq!(merged.excluded_entry_count(), 1);
        assert!(merged.entries.iter().any(|e| matches!(e,
            ArchiveEntry::File { path, object, .. } if path == "Music/song.ogg" && *object == song)));
        let (path, _) = store.write_manifest(&merged).unwrap();
        assert_eq!(store.read_manifest(&path).unwrap(), merged);

        let twice = [("Same", &docs, docs_root), ("Same", &music, music_root)];
        assert!(ArchiveManifest::merged("all", "x", 3, &twice, "jay", "x").is_err());
        let up = [("../up", &docs, docs_root)];
        assert!(ArchiveManifest::merged("all", "x", 3, &up, "jay", "x").is_err());
    }

    #[test]
    fn v2_manifests_keep_their_bytes_and_v3_fields_stay_out_of_them() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let mut v2 =
            ArchiveManifest::new("old", "Old", 1, vec![file_entry(&store, "a.txt", b"a")]).unwrap();
        v2.format = ARCHIVE_MANIFEST_FORMAT_V2.into();
        v2.supersedes_archive_root = Some(CasHash::digest(b"before"));
        let bytes = v2.canonical_bytes().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("parents") && !text.contains("records"));
        // Read back, it is the same bytes, so its root and signature still hold.
        let (path, _) = store.write_manifest(&v2).unwrap();
        let (read, root) = store.read_manifest_and_root(&path).unwrap();
        assert_eq!(read.canonical_bytes().unwrap(), bytes);
        assert_eq!(root, CasHash::digest(&bytes));
        assert_eq!(read.parents(), vec![CasHash::digest(b"before")]);

        let mut mixed = v2.clone();
        mixed.parents = vec![CasHash::digest(b"x")];
        assert!(mixed.canonical_bytes().is_err());
        let mut v3 = v2.clone();
        v3.format = ARCHIVE_MANIFEST_FORMAT_V3.into();
        assert!(v3.canonical_bytes().is_err(), "v3 has no supersedes field");
    }

    #[test]
    fn v3_lists_parents_and_records_before_entries() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let base = ArchiveManifest::new(
            "a",
            "A",
            1,
            vec![
                file_entry(&store, "keep.txt", b"k"),
                file_entry(&store, "drop.txt", b"d"),
            ],
        )
        .unwrap();
        let next = base
            .with_entries_removed(&["drop.txt".into()], "why", "jay", 2)
            .unwrap();
        let text = String::from_utf8(next.canonical_bytes().unwrap()).unwrap();
        let at = |key: &str| text.find(&format!("\n  \"{key}\"")).unwrap();
        assert!(at("parents") < at("records") && at("records") < at("entries"));
        let (path, _) = store.write_manifest(&next).unwrap();
        assert_eq!(store.read_manifest(&path).unwrap(), next);
    }

    #[test]
    fn unverified_history_attaches_old_logs_to_an_unchanged_version() {
        let directory = tempdir().unwrap();
        let store = ArchiveCasStorage::new(directory.path().join("cas")).unwrap();
        let base = ArchiveManifest::new("a", "A", 1, vec![file_entry(&store, "x", b"x")]).unwrap();
        let log = store.add_content(b"{\"old\":\"log\"}").unwrap().object;
        let attached = vec![ArchiveAttachment {
            name: "a-1234.delete-log.json".into(),
            object: log,
        }];
        let next = base.with_unverified_history(attached.clone(), 5).unwrap();
        assert_eq!(next.entries, base.entries);
        assert_eq!(next.parents(), vec![base.digest().unwrap()]);
        assert!(next.records.is_empty());
        assert_eq!(next.attached_objects().collect::<Vec<_>>(), vec![log]);
        assert!(base.with_unverified_history(Vec::new(), 5).is_err());
    }

    #[test]
    fn records_describe_themselves_briefly() {
        let record = ArchiveRecord::new(
            ArchiveChange::Deleted {
                paths: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            },
            "jay",
            "why",
            1,
        )
        .unwrap();
        assert_eq!(record.describe(), "deleted 4 paths: a, b, c, and 1 more");
        let record = ArchiveRecord::new(
            ArchiveChange::Created {
                paths: vec!["x/y".into()],
            },
            "jay",
            "why",
            1,
        )
        .unwrap();
        assert_eq!(record.describe(), "created 1 path: x/y");
        assert!(ArchiveRecord::new(ArchiveChange::Deleted { paths: vec![] }, "j", "w", 1).is_err());
        assert!(ArchiveRecord::new(
            ArchiveChange::Deleted {
                paths: vec!["../x".into()]
            },
            "j",
            "w",
            1
        )
        .is_err());
    }
}
