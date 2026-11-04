//! Working trees: checking a version out to a folder, and recording changes made there.
//!
//! A checkout writes every file of a version into a new folder, checking each against its
//! hash, plus a [`MARKER_FILE`] naming the store, repository, branch and version it came
//! from. `status` compares the folder with that version; `commit` records the changes
//! under the paths it is given (never the whole tree unasked, because several agents may
//! share one tree) as a new signed version of the branch.

use super::ignore::IgnoreRules;
use super::{
    now_unix_secs, validate_name, Leaf, Repository, SignedHead, Signer, TreeCache, TreeEntry,
    Trust, Version, VERSION_FORMAT,
};
use crate::archive_cas::ArchiveObject;
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The file at the top of a checkout that says where it came from.
pub const MARKER_FILE: &str = ".loadngo-repo";
pub const MARKER_FORMAT: &str = "loadngo-repository-checkout-v1";
const BUFFER_BYTES: usize = 1 << 20;

/// What [`MARKER_FILE`] holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutMarker {
    pub format: String,
    /// The store, as an absolute path.
    pub store: PathBuf,
    pub repository_id: String,
    /// The branch the tree follows; `None` for a version checked out by its hash, which
    /// cannot be committed to.
    pub branch: Option<String>,
    /// The version the tree is based on.
    pub version: CasHash,
}

impl CheckoutMarker {
    /// The marker at the top of working tree `tree`.
    ///
    /// # Errors
    /// No marker, or one that cannot be read.
    pub fn read(tree: &Path) -> Result<Self> {
        let path = tree.join(MARKER_FILE);
        let bytes = fs::read(&path)
            .with_context(|| format!("{} is not a checkout (no {MARKER_FILE})", tree.display()))?;
        let marker: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a checkout marker", path.display()))?;
        if marker.format != MARKER_FORMAT {
            bail!("{} has unknown format {:?}", path.display(), marker.format);
        }
        Ok(marker)
    }

    fn write(&self, tree: &Path) -> Result<()> {
        super::write_file_synced(&tree.join(MARKER_FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// Opens the repository this tree came from.
    ///
    /// # Errors
    /// As [`Repository::open`].
    pub fn repository(&self) -> Result<Repository> {
        Repository::open(&self.store, &self.repository_id, false)
    }
}

/// What a checkout targets.
pub enum Target {
    /// The head of a branch; the tree then follows the branch.
    Branch(String),
    /// One version, which must be in the history of a trusted branch.
    Version(CasHash),
}

/// What [`checkout`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutReport {
    pub version: CasHash,
    pub files: u64,
    pub bytes: u64,
}

/// Writes the version `target` names into `destination`, a folder that must not exist.
/// Everything is written into a temporary sibling first and renamed into place at the
/// end, so a checkout that fails (a damaged object, an untrusted head) leaves nothing at
/// `destination`.
///
/// # Errors
/// An untrusted target, a missing or damaged object, a destination that exists or whose
/// parent does not, too little free space, or a failed write.
pub fn checkout(
    repository: &Repository,
    target: &Target,
    trust: &Trust,
    destination: &Path,
) -> Result<CheckoutReport> {
    let (version_hash, branch) = match target {
        Target::Branch(branch) => (
            repository.trusted_head(branch, trust)?.0,
            Some(branch.clone()),
        ),
        Target::Version(hash) => {
            if repository.trusted_branch_holding(*hash, trust)?.is_none() {
                bail!(
                    "version {hash} is not in the history of any branch of {} signed by a trusted key",
                    repository.id()
                );
            }
            (*hash, None)
        }
    };
    let version = repository.version(version_hash)?;
    if destination.exists() || destination.symlink_metadata().is_ok() {
        bail!("{} already exists", destination.display());
    }
    let parent = match destination.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => bail!("{} has no parent folder", destination.display()),
    };
    if !parent.is_dir() {
        bail!("{} is not a folder", parent.display());
    }
    let root = repository.tree(version.tree)?;
    let (files, bytes) = root.totals();
    crate::disk::ensure_room(parent, bytes, crate::disk::floor_from_env())?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("{} has no usable name", destination.display()))?;
    let temporary = parent.join(format!(
        ".{name}.checkout-{}-{}",
        std::process::id(),
        now_unix_secs()
    ));
    fs::create_dir(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    let written = (|| -> Result<()> {
        let mut buffer = vec![0_u8; BUFFER_BYTES];
        write_tree(repository, version.tree, &temporary, &mut buffer)?;
        let store = fs::canonicalize(repository.store().root())?;
        CheckoutMarker {
            format: MARKER_FORMAT.to_string(),
            store,
            repository_id: repository.id().to_string(),
            branch,
            version: version_hash,
        }
        .write(&temporary)?;
        fs::rename(&temporary, destination).with_context(|| {
            format!(
                "failed to move {} to {}",
                temporary.display(),
                destination.display()
            )
        })
    })();
    if let Err(error) = written {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    Ok(CheckoutReport {
        version: version_hash,
        files,
        bytes,
    })
}

fn write_tree(
    repository: &Repository,
    hash: CasHash,
    folder: &Path,
    buffer: &mut [u8],
) -> Result<()> {
    for entry in repository.tree(hash)?.entries {
        #[cfg(windows)]
        if super::is_windows_unsafe(entry.name()) {
            bail!(
                "{:?} cannot be created as a file on Windows (a device name or a forbidden character)",
                entry.name()
            );
        }
        let path = folder.join(entry.name());
        match &entry {
            TreeEntry::File { object, .. } => {
                write_object(repository, *object, &path, false, buffer)?
            }
            TreeEntry::Executable { object, .. } => {
                write_object(repository, *object, &path, true, buffer)?;
            }
            TreeEntry::Symlink { target, .. } => make_symlink(target, &path)?,
            TreeEntry::Directory { tree, .. } => {
                fs::create_dir(&path)
                    .with_context(|| format!("failed to create {}", path.display()))?;
                write_tree(repository, *tree, &path, buffer)?;
            }
        }
    }
    Ok(())
}

/// Copies an object to a new file, hashing as it goes; a mismatch is an error.
fn write_object(
    repository: &Repository,
    object: ArchiveObject,
    path: &Path,
    executable: bool,
    buffer: &mut [u8],
) -> Result<()> {
    let mut reader = repository
        .store()
        .open_object(object.hash)
        .with_context(|| {
            format!(
                "file object {} for {} is not in the store",
                object.hash,
                path.display()
            )
        })?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_mode(&mut options, executable);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    loop {
        let read = reader
            .read(buffer)
            .with_context(|| format!("failed to read file object {}", object.hash))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        size += read as u64;
    }
    let hash = CasHash::from_bytes(*hasher.finalize().as_bytes());
    if hash != object.hash || size != object.size {
        bail!(
            "file object {} for {} is damaged: {size} bytes hashing to {hash}",
            object.hash,
            path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(options: &mut OpenOptions, executable: bool) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(if executable { 0o755 } else { 0o644 });
}

#[cfg(not(unix))]
fn set_mode(_options: &mut OpenOptions, _executable: bool) {}

#[cfg(unix)]
fn make_symlink(target: &str, path: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, path)
        .with_context(|| format!("failed to create symlink {}", path.display()))
}

#[cfg(not(unix))]
fn make_symlink(_target: &str, path: &Path) -> Result<()> {
    bail!(
        "{} is a symlink; checking out symlinks is not supported on this platform yet",
        path.display()
    )
}

/// One difference between a working tree and its version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Created(String),
    Changed(String),
    Deleted(String),
}

impl Change {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Created(path) | Self::Changed(path) | Self::Deleted(path) => path,
        }
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Created(_) => "created",
            Self::Changed(_) => "changed",
            Self::Deleted(_) => "deleted",
        }
    }
}

/// A working tree compared with the version it is based on.
pub struct Status {
    pub marker: CheckoutMarker,
    pub repository: Repository,
    /// What the version holds.
    pub tracked: BTreeMap<String, Leaf>,
    /// What the tree holds that is tracked or not ignored.
    pub found: BTreeMap<String, Leaf>,
    /// The differences, sorted by path.
    pub changes: Vec<Change>,
}

/// Compares working tree `tree` with its version. Tracked paths are always looked at;
/// untracked ones unless `.gitignore` rules ignore them, they are inside a build cache
/// (`CACHEDIR.TAG`) or another checkout, or are named `.git`.
///
/// # Errors
/// No marker, an unreadable version or file, or a name that is not UTF-8.
pub fn status(tree: &Path) -> Result<Status> {
    let marker = CheckoutMarker::read(tree)?;
    let repository = marker.repository()?;
    let version = repository.version(marker.version)?;
    let tracked = repository.leaves(version.tree)?;
    let found = scan(tree, &tracked)?;
    let changes = compare(&tracked, &found);
    Ok(Status {
        marker,
        repository,
        tracked,
        found,
        changes,
    })
}

fn compare(tracked: &BTreeMap<String, Leaf>, found: &BTreeMap<String, Leaf>) -> Vec<Change> {
    let paths: BTreeSet<&String> = tracked.keys().chain(found.keys()).collect();
    let mut changes = Vec::new();
    for path in paths {
        match (tracked.get(path), found.get(path)) {
            (Some(a), Some(b)) if a != b => changes.push(Change::Changed(path.clone())),
            (Some(_), None) => changes.push(Change::Deleted(path.clone())),
            (None, Some(_)) => changes.push(Change::Created(path.clone())),
            _ => {}
        }
    }
    changes
}

/// Every file and symlink in `tree` that is tracked or not ignored, hashed.
fn scan(tree: &Path, tracked: &BTreeMap<String, Leaf>) -> Result<BTreeMap<String, Leaf>> {
    let mut tracked_folders = BTreeSet::new();
    for path in tracked.keys() {
        let mut rest = path.as_str();
        while let Some((folder, _)) = rest.rsplit_once('/') {
            tracked_folders.insert(folder.to_string());
            rest = folder;
        }
    }
    let mut walk = Walk {
        top: tree,
        tracked,
        tracked_folders,
        found: BTreeMap::new(),
        buffer: vec![0; BUFFER_BYTES],
    };
    walk.folder("", &IgnoreRules::default(), false)?;
    Ok(walk.found)
}

struct Walk<'a> {
    top: &'a Path,
    tracked: &'a BTreeMap<String, Leaf>,
    tracked_folders: BTreeSet<String>,
    found: BTreeMap<String, Leaf>,
    buffer: Vec<u8>,
}

impl Walk<'_> {
    /// `inside_ignored`: this folder is ignored but holds tracked paths, so only those
    /// are looked at.
    fn folder(&mut self, relative: &str, outer: &IgnoreRules, inside_ignored: bool) -> Result<()> {
        let absolute = if relative.is_empty() {
            self.top.to_path_buf()
        } else {
            self.top.join(relative)
        };
        let mut rules = outer.clone();
        rules.read_folder(&absolute, relative)?;
        let mut entries: Vec<_> = fs::read_dir(&absolute)
            .with_context(|| format!("failed to list {}", absolute.display()))?
            .collect::<std::io::Result<_>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                bail!("{} has a name that is not UTF-8", entry.path().display());
            };
            if name.eq_ignore_ascii_case(".git") || (relative.is_empty() && name == MARKER_FILE) {
                continue;
            }
            let path = super::join(relative, name);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                let holds_tracked = self.tracked_folders.contains(&path);
                let skipped = inside_ignored
                    || rules.is_ignored(&path, true)
                    || crate::archive_cas::is_build_cache(&entry.path())
                    || entry.path().join(MARKER_FILE).exists();
                if !skipped {
                    self.folder(&path, &rules, false)?;
                } else if holds_tracked {
                    self.folder(&path, &rules, true)?;
                }
                continue;
            }
            let tracked = self.tracked.get(&path);
            if tracked.is_none() && (inside_ignored || rules.is_ignored(&path, false)) {
                continue;
            }
            if kind.is_symlink() {
                let target = fs::read_link(entry.path())?;
                let target = target
                    .to_str()
                    .ok_or_else(|| anyhow!("symlink {path} has a target that is not UTF-8"))?;
                self.found.insert(path, Leaf::Symlink(target.to_string()));
            } else if kind.is_file() {
                let (object, executable) = hash_file(&entry.path(), &mut self.buffer)?;
                let executable = executable.unwrap_or(matches!(tracked, Some(Leaf::Executable(_))));
                let leaf = if executable {
                    Leaf::Executable(object)
                } else {
                    Leaf::File(object)
                };
                self.found.insert(path, leaf);
            }
        }
        Ok(())
    }
}

/// A file's object (hash and size) and, where the platform says, whether it is
/// executable.
fn hash_file(path: &Path, buffer: &mut [u8]) -> Result<(ArchiveObject, Option<bool>)> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let executable = is_executable(&file.metadata()?);
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    loop {
        let read = file.read(buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((
        ArchiveObject {
            hash: CasHash::from_bytes(*hasher.finalize().as_bytes()),
            size,
        },
        executable,
    ))
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn is_executable(metadata: &fs::Metadata) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> Option<bool> {
    None
}

/// What [`commit`] and [`init`] made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitReport {
    pub version: CasHash,
    pub parent: Option<CasHash>,
    pub branch: String,
    pub changes: Vec<Change>,
}

/// Records the changes in working tree `tree` under `paths` (files or folders, relative
/// to the tree's top) as a new version of the tree's branch, signed by `signer`, and
/// moves the branch to it. Changes elsewhere stay as they are, uncommitted.
///
/// # Errors
/// No branch to commit to, a branch that moved since the tree was based on it, a path
/// that names nothing, nothing to commit, or a failed write.
pub fn commit(
    tree: &Path,
    paths: &[String],
    message: &str,
    author: &str,
    signer: &Signer,
    trust: &Trust,
) -> Result<CommitReport> {
    if paths.is_empty() {
        bail!("name the paths to commit with --path");
    }
    if message.trim().is_empty() || author.trim().is_empty() {
        bail!("a commit needs a message and an author");
    }
    let status = status(tree)?;
    let branch = status.marker.branch.clone().ok_or_else(|| {
        anyhow!(
            "this tree is a checkout of one version, not of a branch; check out a branch to commit"
        )
    })?;
    let repository = &status.repository;
    let (head, _) = repository.trusted_head(&branch, trust)?;
    refuse_untrusted(
        &SignedHead::sign(repository.id(), &branch, CasHash::digest(b""), signer, 0)?,
        trust,
    )?;
    if head != status.marker.version {
        bail!(
            "branch {branch} is at {head}, but this tree is based on {}; it has moved since",
            status.marker.version
        );
    }
    let mut named = Vec::new();
    for path in paths {
        let path = normalize(path)?;
        let prefix = format!("{path}/");
        let exists = |map: &BTreeMap<String, Leaf>| {
            map.contains_key(&path)
                || map
                    .range(prefix.clone()..)
                    .next()
                    .is_some_and(|(key, _)| key.starts_with(&prefix))
        };
        if !exists(&status.tracked) && !exists(&status.found) && !tree.join(&path).exists() {
            bail!("{path} is neither in the version nor in the tree");
        }
        named.push(path);
    }
    let selected: Vec<Change> = status
        .changes
        .iter()
        .filter(|change| {
            named
                .iter()
                .any(|path| change.path() == path || change.path().starts_with(&format!("{path}/")))
        })
        .cloned()
        .collect();
    if selected.is_empty() {
        bail!("nothing to commit under the named paths");
    }
    let mut leaves = status.tracked.clone();
    for change in &selected {
        match change {
            Change::Deleted(path) => {
                leaves.remove(path);
            }
            Change::Created(path) | Change::Changed(path) => {
                let leaf = store_leaf(repository, tree, path, &status.found[path])?;
                leaves.insert(path.clone(), leaf);
            }
        }
    }
    let base = repository.version(head)?;
    let version = record(
        repository,
        &leaves,
        Some((head, base.tree)),
        message,
        author,
    )?;
    let signed = SignedHead::sign(repository.id(), &branch, version, signer, now_unix_secs())?;
    refuse_untrusted(&signed, trust)?;
    repository.set_head(&signed, Some(head))?;
    let mut marker = status.marker.clone();
    marker.version = version;
    marker.write(tree)?;
    Ok(CommitReport {
        version,
        parent: Some(head),
        branch,
        changes: selected,
    })
}

/// Records every file in `tree` that is not ignored as the first version of a new
/// `branch` of `repository`, and makes `tree` its checkout.
///
/// # Errors
/// A tree that is a checkout already, a branch that exists, nothing to record, or a
/// failed write.
pub fn init(
    repository: &Repository,
    branch: &str,
    tree: &Path,
    message: &str,
    author: &str,
    signer: &Signer,
    trust: &Trust,
) -> Result<CommitReport> {
    super::validate_id(branch, "branch")?;
    // Refuse before writing anything: a head no trusted key verifies would leave the
    // branch stuck.
    refuse_untrusted(
        &SignedHead::sign(repository.id(), branch, CasHash::digest(b""), signer, 0)?,
        trust,
    )?;
    if tree.join(MARKER_FILE).exists() {
        bail!("{} is a checkout already", tree.display());
    }
    if repository.head(branch)?.is_some() {
        bail!("{} has a branch {branch} already", repository.id());
    }
    if message.trim().is_empty() || author.trim().is_empty() {
        bail!("a first version needs a message and an author");
    }
    let found = scan(tree, &BTreeMap::new())?;
    if found.is_empty() {
        bail!("{} holds nothing to record", tree.display());
    }
    let mut leaves = BTreeMap::new();
    for (path, leaf) in &found {
        leaves.insert(path.clone(), store_leaf(repository, tree, path, leaf)?);
    }
    let version = record(repository, &leaves, None, message, author)?;
    let signed = SignedHead::sign(repository.id(), branch, version, signer, now_unix_secs())?;
    refuse_untrusted(&signed, trust)?;
    repository.set_head(&signed, None)?;
    CheckoutMarker {
        format: MARKER_FORMAT.to_string(),
        store: fs::canonicalize(repository.store().root())?,
        repository_id: repository.id().to_string(),
        branch: Some(branch.to_string()),
        version,
    }
    .write(tree)?;
    Ok(CommitReport {
        version,
        parent: None,
        branch: branch.to_string(),
        changes: found.into_keys().map(Change::Created).collect(),
    })
}

/// Refuses a head the trusted keys would not verify: once written, the branch could not
/// be moved again without editing the store by hand.
pub(super) fn refuse_untrusted(head: &SignedHead, trust: &Trust) -> Result<()> {
    head.verify(trust).map(|_| ()).with_context(|| {
        format!(
            "{} would sign with a key the trusted keys do not hold; name a trusted key, or sign with one",
            head.signer_identity
        )
    })
}

/// Writes the trees for `leaves` and a version of them on `parent` (version and tree).
fn record(
    repository: &Repository,
    leaves: &BTreeMap<String, Leaf>,
    parent: Option<(CasHash, CasHash)>,
    message: &str,
    author: &str,
) -> Result<CasHash> {
    for path in leaves.keys() {
        for name in path.split('/') {
            validate_name(name).with_context(|| format!("path {path:?}"))?;
        }
    }
    let mut cache = TreeCache::default();
    let tree = repository.put_leaves(leaves, &mut cache)?;
    if parent.is_some_and(|(_, old)| old == tree) {
        bail!("the named changes leave the tree as it was; nothing to commit");
    }
    let changes = repository.changes_between(
        parent.map(|(version, _)| version),
        parent.map(|(_, tree)| tree),
        tree,
        &mut cache,
    )?;
    repository.put_version(&Version {
        format: VERSION_FORMAT.to_string(),
        repository_id: repository.id().to_string(),
        parents: parent.map(|(version, _)| version).into_iter().collect(),
        tree,
        author: author.to_string(),
        authored_at_unix_secs: now_unix_secs(),
        message: message.to_string(),
        changes,
        origin: None,
    })
}

/// Stores the file or symlink at `path` in `tree`; returns what was stored (a file is
/// read again, so the stored bytes are what the version names).
fn store_leaf(repository: &Repository, tree: &Path, path: &str, seen: &Leaf) -> Result<Leaf> {
    let absolute = tree.join(path);
    match seen {
        Leaf::Symlink(target) => Ok(Leaf::Symlink(target.clone())),
        Leaf::File(object) | Leaf::Executable(object) => {
            crate::disk::ensure_room(
                repository.store().root(),
                object.size,
                crate::disk::floor_from_env(),
            )?;
            let mut file = File::open(&absolute)
                .with_context(|| format!("failed to open {}", absolute.display()))?;
            let stored = repository
                .store()
                .add_stream(&mut file)
                .with_context(|| format!("failed to store {}", absolute.display()))?
                .object;
            Ok(if matches!(seen, Leaf::Executable(_)) {
                Leaf::Executable(stored)
            } else {
                Leaf::File(stored)
            })
        }
    }
}

/// A path relative to a tree's top, in `a/b` form.
fn normalize(path: &str) -> Result<String> {
    let trimmed = path.trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() || trimmed.starts_with('/') {
        bail!("{path:?} must be a path relative to the top of the tree");
    }
    for name in trimmed.split('/') {
        if name.is_empty() || name == "." || name == ".." {
            bail!("{path:?} must be a plain relative path");
        }
    }
    Ok(trimmed.to_string())
}

/// The top of the checkout holding `start` (it or a folder above it with a
/// [`MARKER_FILE`]).
///
/// # Errors
/// When there is none.
pub fn find_tree(start: &Path) -> Result<PathBuf> {
    let start =
        fs::canonicalize(start).with_context(|| format!("no folder {}", start.display()))?;
    let mut folder = start.as_path();
    loop {
        if folder.join(MARKER_FILE).is_file() {
            return Ok(folder.to_path_buf());
        }
        folder = folder.parent().ok_or_else(|| {
            anyhow!(
                "{} is not inside a checkout (no {MARKER_FILE} above it)",
                start.display()
            )
        })?;
    }
}
