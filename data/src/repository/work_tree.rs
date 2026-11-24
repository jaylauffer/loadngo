//! Working trees: checking a version out to a folder, and recording changes made there.
//!
//! A checkout writes every file of a version into a new folder, checking each against its
//! hash, plus a [`MARKER_FILE`] naming the store, repository, branch and version it came
//! from. `status` compares the folder with that version; `commit` records the changes
//! under the paths it is given (never the whole tree unasked, because several agents may
//! share one tree) as a new signed version of the branch.

use super::ignore::IgnoreRules;
use super::runtime::{RepositoryArchiveCasRestoreIo, RepositoryRuntime};
use super::{
    now_unix_secs, validate_name, Leaf, Repository, SignedHead, Signer, TreeCache, TreeEntry,
    Trust, Version, VERSION_FORMAT,
};
use crate::archive_cas::ArchiveObject;
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The file at the top of a checkout that says where it came from.
pub const MARKER_FILE: &str = ".loadngo-repo";
pub const MARKER_FORMAT: &str = "loadngo-repository-checkout-v1";
const BUFFER_BYTES: usize = 1 << 20;
const MAX_TEXT_MERGE_BYTES: usize = 512 * 1024;
const MAX_TEXT_DIFF_CELLS: usize = 1_000_000;

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

/// A branch history whose head signature and reachable version chain were
/// verified together. The private fields make this a capability for safe batch
/// checkouts without repeating the signature and ancestry walk per version.
pub struct TrustedHistory {
    repository_id: String,
    branch: String,
    versions: BTreeSet<CasHash>,
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

/// Checks out a version using the command proactor for tree and manifest reads and CAS
/// object restoration. Destination file open, sync, close, and publication, along with
/// directory creation, symlink creation, mode changes, and marker publication, use the
/// bounded blocking-I/O lane. The temporary directory remains the transaction
/// boundary: a failed restore removes it.
pub fn checkout_with_runtime(
    repository: &Repository,
    target: &Target,
    trust: &Trust,
    destination: &Path,
    runtime: &mut RepositoryRuntime,
) -> Result<CheckoutReport> {
    let (version_hash, branch) = match target {
        Target::Branch(branch) => (
            repository
                .trusted_head_with_runtime(branch, trust, runtime)?
                .0,
            Some(branch.clone()),
        ),
        Target::Version(hash) => {
            if repository
                .trusted_branch_holding_with_runtime(*hash, trust, runtime)?
                .is_none()
            {
                bail!(
                    "version {hash} is not in the history of any branch of {} signed by a trusted key",
                    repository.id()
                );
            }
            (*hash, None)
        }
    };
    checkout_version_with_runtime(repository, version_hash, branch, destination, runtime)
}

/// Verifies one branch head and materializes its complete reachable history for
/// repeated version checkouts in the same command.
pub fn trusted_history_with_runtime(
    repository: &Repository,
    branch: &str,
    trust: &Trust,
    runtime: &mut RepositoryRuntime,
) -> Result<TrustedHistory> {
    let (head, _) = repository.trusted_head_with_runtime(branch, trust, runtime)?;
    let versions = repository
        .ancestry_with_runtime(head, runtime)?
        .into_iter()
        .map(|(version, _)| version)
        .collect();
    Ok(TrustedHistory {
        repository_id: repository.id().to_string(),
        branch: branch.to_string(),
        versions,
    })
}

/// Checks out `version` after proving it belongs to `history`'s trusted branch.
/// This is useful for batch audits: signature and ancestry verification happen
/// once, while every version still uses the normal transactional materializer.
pub fn checkout_in_trusted_history_with_runtime(
    repository: &Repository,
    history: &TrustedHistory,
    version: CasHash,
    destination: &Path,
    runtime: &mut RepositoryRuntime,
) -> Result<CheckoutReport> {
    if history.repository_id != repository.id() {
        bail!("trusted history belongs to another repository");
    }
    if !history.versions.contains(&version) {
        bail!(
            "version {version} is not in the trusted history of branch {}",
            history.branch
        );
    }
    checkout_version_with_runtime(
        repository,
        version,
        Some(history.branch.clone()),
        destination,
        runtime,
    )
}

fn checkout_version_with_runtime(
    repository: &Repository,
    version_hash: CasHash,
    branch: Option<String>,
    destination: &Path,
    runtime: &mut RepositoryRuntime,
) -> Result<CheckoutReport> {
    let version = repository.version_with_runtime(version_hash, runtime)?;
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
    let root = repository.tree_with_runtime(version.tree, runtime)?;
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
    let temporary_for_create = temporary.clone();
    run_blocking_fs(runtime, move || {
        fs::create_dir(&temporary_for_create)
            .with_context(|| format!("failed to create {}", temporary_for_create.display()))
    })?;
    let written = (|| -> Result<()> {
        write_tree_with_runtime(repository, version.tree, &temporary, runtime)?;
        let marker = CheckoutMarker {
            format: MARKER_FORMAT.to_string(),
            store: repository.store().root().to_path_buf(),
            repository_id: repository.id().to_string(),
            branch,
            version: version_hash,
        };
        let temporary_for_publish = temporary.clone();
        let destination_for_publish = destination.to_path_buf();
        run_blocking_fs(runtime, move || {
            let mut marker = marker;
            marker.store = fs::canonicalize(&marker.store)?;
            marker.write(&temporary_for_publish)?;
            fs::rename(&temporary_for_publish, &destination_for_publish).with_context(|| {
                format!(
                    "failed to move {} to {}",
                    temporary_for_publish.display(),
                    destination_for_publish.display()
                )
            })
        })?;
        Ok(())
    })();
    if let Err(error) = written {
        let temporary_for_cleanup = temporary.clone();
        let _ = runtime.run_blocking_io(move || fs::remove_dir_all(temporary_for_cleanup));
        return Err(error);
    }
    Ok(CheckoutReport {
        version: version_hash,
        files,
        bytes,
    })
}

fn write_tree_with_runtime(
    repository: &Repository,
    hash: CasHash,
    folder: &Path,
    runtime: &mut RepositoryRuntime,
) -> Result<()> {
    for entry in repository.tree_with_runtime(hash, runtime)?.entries {
        #[cfg(windows)]
        if super::is_windows_unsafe(entry.name()) {
            bail!(
                "{:?} cannot be created as a file on Windows (a device name or a forbidden character)",
                entry.name()
            );
        }
        let path = folder.join(entry.name());
        match entry {
            TreeEntry::File { object, .. } => {
                restore_file_with_runtime(repository, object, &path, false, runtime)?;
            }
            TreeEntry::Executable { object, .. } => {
                restore_file_with_runtime(repository, object, &path, true, runtime)?;
            }
            TreeEntry::ChunkedFile {
                manifest,
                file,
                executable,
                ..
            } => restore_chunked_file_with_runtime(
                repository, manifest, file, &path, executable, runtime,
            )?,
            TreeEntry::Symlink { target, .. } => {
                let path = path.clone();
                run_blocking_fs(runtime, move || make_symlink(&target, &path))?;
            }
            TreeEntry::Directory { tree, .. } => {
                let directory = path.clone();
                run_blocking_fs(runtime, move || {
                    fs::create_dir(&directory)
                        .with_context(|| format!("failed to create {}", directory.display()))
                })?;
                write_tree_with_runtime(repository, tree, &path, runtime)?;
            }
        }
    }
    Ok(())
}

fn run_blocking_fs<T, F>(runtime: &mut RepositoryRuntime, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    runtime.admit_optional_work()?;
    runtime.run_blocking_io(operation)?
}

fn restore_file_with_runtime(
    repository: &Repository,
    object: ArchiveObject,
    path: &Path,
    executable: bool,
    runtime: &mut RepositoryRuntime,
) -> Result<()> {
    let (proactor, handle, thermal, blocking_io) = runtime.blocking_io_parts_mut()?;
    let mut restore_io = RepositoryArchiveCasRestoreIo::new(proactor, handle, thermal, blocking_io);
    repository
        .store()
        .restore_object_to_path_with_proactor_restore_io(
            proactor,
            handle,
            object,
            path,
            &mut restore_io,
        )
        .with_context(|| format!("failed to restore file {}", path.display()))?;
    let path = path.to_path_buf();
    run_blocking_fs(runtime, move || set_path_mode(&path, executable))
}

fn restore_chunked_file_with_runtime(
    repository: &Repository,
    manifest_object: ArchiveObject,
    file_object: ArchiveObject,
    path: &Path,
    executable: bool,
    runtime: &mut RepositoryRuntime,
) -> Result<()> {
    let manifest = repository.file_manifest_with_runtime(manifest_object, file_object, runtime)?;
    let (proactor, handle, thermal, blocking_io) = runtime.blocking_io_parts_mut()?;
    let mut restore_io = RepositoryArchiveCasRestoreIo::new(proactor, handle, thermal, blocking_io);
    repository
        .store()
        .restore_manifest_to_path_with_proactor_restore_io(
            proactor,
            handle,
            &manifest,
            path,
            &mut restore_io,
        )?;
    let path = path.to_path_buf();
    run_blocking_fs(runtime, move || set_path_mode(&path, executable))
}

#[cfg(unix)]
fn set_path_mode(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )?;
    Ok(())
}

#[cfg(not(unix))]
fn set_path_mode(_path: &Path, _executable: bool) -> Result<()> {
    Ok(())
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
            TreeEntry::ChunkedFile {
                manifest,
                file,
                executable,
                ..
            } => write_chunked_file(repository, *manifest, *file, &path, *executable, buffer)?,
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

fn write_chunked_file(
    repository: &Repository,
    manifest_object: ArchiveObject,
    file_object: ArchiveObject,
    path: &Path,
    executable: bool,
    buffer: &mut [u8],
) -> Result<()> {
    let manifest = repository.file_manifest(manifest_object, file_object)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_mode(&mut options, executable);
    let mut destination = options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let mut file_hasher = blake3::Hasher::new();
    let mut file_size = 0_u64;
    for chunk in manifest.chunks {
        let mut reader = repository
            .store()
            .open_object(chunk.object.hash)
            .with_context(|| {
                format!(
                    "chunk {} for {} is not in the store",
                    chunk.object.hash,
                    path.display()
                )
            })?;
        let mut chunk_hasher = blake3::Hasher::new();
        let mut chunk_size = 0_u64;
        loop {
            let count = reader
                .read(buffer)
                .with_context(|| format!("failed to read chunk {}", chunk.object.hash))?;
            if count == 0 {
                break;
            }
            let bytes = &buffer[..count];
            chunk_hasher.update(bytes);
            file_hasher.update(bytes);
            destination.write_all(bytes)?;
            chunk_size = chunk_size
                .checked_add(count as u64)
                .ok_or_else(|| anyhow!("chunk size exceeds u64"))?;
            file_size = file_size
                .checked_add(count as u64)
                .ok_or_else(|| anyhow!("file size exceeds u64"))?;
        }
        if chunk_size != chunk.object.size
            || CasHash::from_bytes(*chunk_hasher.finalize().as_bytes()) != chunk.object.hash
        {
            bail!(
                "chunk {} for {} is damaged",
                chunk.object.hash,
                path.display()
            );
        }
    }
    let file_hash = CasHash::from_bytes(*file_hasher.finalize().as_bytes());
    if file_size != file_object.size || file_hash != file_object.hash {
        bail!(
            "reconstructed file {} does not match its manifest identity",
            path.display()
        );
    }
    destination.sync_all()?;
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
    status_internal(tree, None)
}

/// Like [`status`], hashing working files and reading repository metadata through runtime I/O.
pub fn status_with_runtime(tree: &Path, runtime: &mut RepositoryRuntime) -> Result<Status> {
    status_internal(tree, Some(runtime))
}

fn status_internal(tree: &Path, runtime: Option<&mut RepositoryRuntime>) -> Result<Status> {
    let marker = CheckoutMarker::read(tree)?;
    let repository = marker.repository()?;
    let (tracked, found) = if let Some(runtime) = runtime {
        let version = repository.version_with_runtime(marker.version, runtime)?;
        let tracked = repository.leaves_with_runtime(version.tree, runtime)?;
        let found = scan_with_runtime(tree, &tracked, &repository, runtime)?;
        (tracked, found)
    } else {
        let version = repository.version(marker.version)?;
        let tracked = repository.leaves(version.tree)?;
        let found = scan(tree, &tracked)?;
        (tracked, found)
    };
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
            (Some(a), Some(b)) if !same_leaf(a, b) => changes.push(Change::Changed(path.clone())),
            (Some(_), None) => changes.push(Change::Deleted(path.clone())),
            (None, Some(_)) => changes.push(Change::Created(path.clone())),
            _ => {}
        }
    }
    changes
}

fn same_leaf(a: &Leaf, b: &Leaf) -> bool {
    match (a, b) {
        (
            Leaf::ChunkedFile {
                file, executable, ..
            },
            Leaf::File(found),
        ) => !executable && file == found,
        (
            Leaf::ChunkedFile {
                file, executable, ..
            },
            Leaf::Executable(found),
        ) => *executable && file == found,
        (
            Leaf::File(tracked),
            Leaf::ChunkedFile {
                file, executable, ..
            },
        ) => !executable && tracked == file,
        (
            Leaf::Executable(tracked),
            Leaf::ChunkedFile {
                file, executable, ..
            },
        ) => *executable && tracked == file,
        _ => a == b,
    }
}

/// Every file and symlink in `tree` that is tracked or not ignored, hashed.
fn scan(tree: &Path, tracked: &BTreeMap<String, Leaf>) -> Result<BTreeMap<String, Leaf>> {
    scan_internal(tree, tracked, None, None)
}

fn scan_with_runtime(
    tree: &Path,
    tracked: &BTreeMap<String, Leaf>,
    repository: &Repository,
    runtime: &mut RepositoryRuntime,
) -> Result<BTreeMap<String, Leaf>> {
    scan_internal(tree, tracked, Some(repository), Some(runtime))
}

fn scan_internal(
    tree: &Path,
    tracked: &BTreeMap<String, Leaf>,
    repository: Option<&Repository>,
    runtime: Option<&mut RepositoryRuntime>,
) -> Result<BTreeMap<String, Leaf>> {
    let mut tracked_folders = BTreeSet::new();
    let mut tracked_executable = BTreeMap::new();
    for (path, leaf) in tracked {
        tracked_executable.insert(
            path.clone(),
            matches!(
                leaf,
                Leaf::Executable(_)
                    | Leaf::ChunkedFile {
                        executable: true,
                        ..
                    }
            ),
        );
        let mut rest = path.as_str();
        while let Some((folder, _)) = rest.rsplit_once('/') {
            tracked_folders.insert(folder.to_string());
            rest = folder;
        }
    }

    let mut runtime = runtime;
    let scanned = if let Some(runtime) = runtime.as_deref_mut() {
        repository.ok_or_else(|| anyhow!("runtime tree scan has no repository store"))?;
        runtime.admit_optional_work()?;
        let top = tree.to_path_buf();
        runtime.run_blocking_io(move || {
            collect_scan_paths(top, tracked_folders, tracked_executable)
        })??
    } else {
        collect_scan_paths(tree.to_path_buf(), tracked_folders, tracked_executable)?
    };

    let mut found = BTreeMap::new();
    let mut buffer = vec![0; BUFFER_BYTES];
    for (path, scanned) in scanned {
        match scanned {
            ScannedPath::Symlink(target) => {
                found.insert(path, Leaf::Symlink(target));
            }
            ScannedPath::File { tracked_executable } => {
                let absolute = tree.join(&path);
                let (object, executable) = if let Some(runtime) = runtime.as_deref_mut() {
                    hash_file_with_runtime(
                        repository
                            .ok_or_else(|| anyhow!("runtime tree scan has no repository store"))?,
                        &absolute,
                        &mut buffer,
                        runtime,
                    )?
                } else {
                    hash_file(&absolute, &mut buffer)?
                };
                let executable = executable.unwrap_or(tracked_executable);
                found.insert(
                    path,
                    if executable {
                        Leaf::Executable(object)
                    } else {
                        Leaf::File(object)
                    },
                );
            }
        }
    }
    Ok(found)
}

enum ScannedPath {
    File { tracked_executable: bool },
    Symlink(String),
}

struct Walk {
    top: PathBuf,
    tracked_executable: BTreeMap<String, bool>,
    tracked_folders: BTreeSet<String>,
    found: BTreeMap<String, ScannedPath>,
}

fn collect_scan_paths(
    top: PathBuf,
    tracked_folders: BTreeSet<String>,
    tracked_executable: BTreeMap<String, bool>,
) -> Result<BTreeMap<String, ScannedPath>> {
    let mut walk = Walk {
        top,
        tracked_executable,
        tracked_folders,
        found: BTreeMap::new(),
    };
    walk.folder("", &IgnoreRules::default(), false)?;
    Ok(walk.found)
}

impl Walk {
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
            let tracked_executable = self.tracked_executable.get(&path).copied();
            if tracked_executable.is_none() && (inside_ignored || rules.is_ignored(&path, false)) {
                continue;
            }
            if kind.is_symlink() {
                let target = fs::read_link(entry.path())?;
                let target = target
                    .to_str()
                    .ok_or_else(|| anyhow!("symlink {path} has a target that is not UTF-8"))?;
                self.found
                    .insert(path, ScannedPath::Symlink(target.to_string()));
            } else if kind.is_file() {
                self.found.insert(
                    path,
                    ScannedPath::File {
                        tracked_executable: tracked_executable.unwrap_or(false),
                    },
                );
            }
        }
        Ok(())
    }
}

fn hash_file_with_runtime(
    repository: &Repository,
    path: &Path,
    reusable_buffer: &mut Vec<u8>,
    runtime: &mut RepositoryRuntime,
) -> Result<(ArchiveObject, Option<bool>)> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let executable = is_executable(&metadata);
    let (proactor, handle, thermal, workers) = runtime.parts_mut();
    let object = repository.store().hash_file_with_proactor_admitted_and(
        proactor,
        handle,
        path,
        reusable_buffer,
        |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
        |hasher, buffer, count| {
            let mut task = workers
                .try_submit(handle, move || {
                    let mut hasher = hasher;
                    hasher.update(&buffer[..count]);
                    (hasher, buffer)
                })
                .map_err(anyhow::Error::new)?;
            loop {
                if let Some(result) = task.try_take() {
                    return result.map_err(anyhow::Error::new);
                }
                proactor
                    .run_once()
                    .context("failed to dispatch working-file hash completion")?;
            }
        },
    )?;
    Ok((object, executable))
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

/// What [`merge_with_runtime`] recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeKind {
    AlreadyContained,
    FastForward,
    MergeCommit,
}

/// What [`merge_with_runtime`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReport {
    pub kind: MergeKind,
    pub version: CasHash,
    pub first_parent: CasHash,
    pub second_parent: CasHash,
    pub merge_base: CasHash,
    pub branch: String,
    pub files: usize,
}

/// Makes a three-way merge from `other_branch` into `branch`.
///
/// Non-overlapping edits and identical edits combine automatically. Small UTF-8 text files
/// with non-overlapping line edits are merged; remaining conflicting paths must be covered
/// by exactly one of `take_ours` or `take_theirs`, which choose the whole file or symlink.
/// A directory choice applies only to conflicts beneath it, preserving independent edits
/// elsewhere in that directory. File-versus-directory conflicts choose that subtree from
/// one side. The merge is recorded as a two-parent version and the target head moves only
/// after its complete closure is stored. Existing checkouts remain at their recorded
/// version and must be checked out again.
#[allow(clippy::too_many_arguments)]
pub fn merge_with_runtime(
    repository: &Repository,
    branch: &str,
    other_branch: &str,
    take_ours: &[String],
    take_theirs: &[String],
    message: &str,
    author: &str,
    signer: &Signer,
    trust: &Trust,
    runtime: &mut RepositoryRuntime,
) -> Result<MergeReport> {
    if branch == other_branch {
        bail!("a branch cannot be merged into itself");
    }
    if message.trim().is_empty() || author.trim().is_empty() {
        bail!("a merge needs a message and an author");
    }
    let (ours, _) = repository.trusted_head_with_runtime(branch, trust, runtime)?;
    let (theirs, _) = repository.trusted_head_with_runtime(other_branch, trust, runtime)?;
    let ours_history = repository.ancestry_with_runtime(ours, runtime)?;
    let theirs_history = repository.ancestry_with_runtime(theirs, runtime)?;
    let ours_hashes: BTreeSet<_> = ours_history.iter().map(|(hash, _)| *hash).collect();
    let theirs_hashes: BTreeSet<_> = theirs_history.iter().map(|(hash, _)| *hash).collect();
    let mut versions = BTreeMap::new();
    for (hash, version) in ours_history.into_iter().chain(theirs_history) {
        versions.insert(hash, version);
    }
    if ours_hashes.contains(&theirs) {
        if !take_ours.is_empty() || !take_theirs.is_empty() {
            bail!("--ours and --theirs apply only when resolving divergent merge conflicts");
        }
        return Ok(MergeReport {
            kind: MergeKind::AlreadyContained,
            version: ours,
            first_parent: ours,
            second_parent: theirs,
            merge_base: theirs,
            branch: branch.to_string(),
            files: 0,
        });
    }
    if theirs_hashes.contains(&ours) {
        if !take_ours.is_empty() || !take_theirs.is_empty() {
            bail!("--ours and --theirs apply only when resolving divergent merge conflicts");
        }
        repository
            .check_closure_with_runtime(theirs, runtime)
            .context("refusing to fast-forward to an incomplete or damaged object closure")?;
        let version = repository.version_with_runtime(theirs, runtime)?;
        let files = repository.leaves_with_runtime(version.tree, runtime)?.len();
        refuse_signer_untrusted(signer, trust)?;
        let signed = runtime.sign_head(repository.id(), branch, theirs, signer, now_unix_secs())?;
        repository.set_head_with_runtime(&signed, Some(ours), runtime)?;
        return Ok(MergeReport {
            kind: MergeKind::FastForward,
            version: theirs,
            first_parent: ours,
            second_parent: theirs,
            merge_base: ours,
            branch: branch.to_string(),
            files,
        });
    }
    let common: BTreeSet<_> = ours_hashes.intersection(&theirs_hashes).copied().collect();
    if common.is_empty() {
        bail!("branches {branch} and {other_branch} have no common ancestor");
    }
    let mut remaining_children: BTreeMap<_, usize> =
        versions.keys().map(|hash| (*hash, 0)).collect();
    for version in versions.values() {
        for parent in &version.parents {
            let count = remaining_children
                .get_mut(parent)
                .ok_or_else(|| anyhow!("version names missing parent {parent}"))?;
            *count += 1;
        }
    }
    let mut ready: VecDeque<_> = remaining_children
        .iter()
        .filter_map(|(hash, children)| (*children == 0).then_some(*hash))
        .collect();
    let mut has_common_descendant = BTreeSet::new();
    let mut visited = 0;
    while let Some(hash) = ready.pop_front() {
        visited += 1;
        let carries_common = common.contains(&hash) || has_common_descendant.contains(&hash);
        for parent in &versions[&hash].parents {
            if carries_common {
                has_common_descendant.insert(*parent);
            }
            let children = remaining_children
                .get_mut(parent)
                .ok_or_else(|| anyhow!("version names missing parent {parent}"))?;
            *children -= 1;
            if *children == 0 {
                ready.push_back(*parent);
            }
        }
    }
    if visited != versions.len() {
        bail!("version ancestry contains a cycle");
    }
    let merge_bases: Vec<_> = common.difference(&has_common_descendant).copied().collect();
    let [merge_base] = merge_bases.as_slice() else {
        bail!(
            "branches have {} best common ancestors; merge-base selection is ambiguous",
            merge_bases.len()
        );
    };
    let merge_base = *merge_base;

    let ours_version = repository.version_with_runtime(ours, runtime)?;
    let theirs_version = repository.version_with_runtime(theirs, runtime)?;
    let base_version = repository.version_with_runtime(merge_base, runtime)?;
    let base = repository.leaves_with_runtime(base_version.tree, runtime)?;
    let our_leaves = repository.leaves_with_runtime(ours_version.tree, runtime)?;
    let their_leaves = repository.leaves_with_runtime(theirs_version.tree, runtime)?;
    let paths: BTreeSet<_> = base
        .keys()
        .chain(our_leaves.keys())
        .chain(their_leaves.keys())
        .cloned()
        .collect();
    let ours_paths: Vec<_> = take_ours
        .iter()
        .map(|path| normalize(path))
        .collect::<Result<_>>()?;
    let theirs_paths: Vec<_> = take_theirs
        .iter()
        .map(|path| normalize(path))
        .collect::<Result<_>>()?;
    let ours_choice: BTreeSet<_> = ours_paths.iter().map(String::as_str).collect();
    let theirs_choice: BTreeSet<_> = theirs_paths.iter().map(String::as_str).collect();
    if let Some(path) = ours_choice.intersection(&theirs_choice).next() {
        bail!("conflict path {path:?} is resolved as both ours and theirs");
    }
    let resolved: BTreeSet<_> = ours_choice.union(&theirs_choice).copied().collect();
    for path in &resolved {
        if resolved.iter().any(|other| {
            other != path
                && (other.starts_with(&format!("{path}/"))
                    || path.starts_with(&format!("{other}/")))
        }) {
            bail!("merge resolution paths may not overlap: {path:?}");
        }
        if !paths_contain_path(&our_leaves, path) && !paths_contain_path(&their_leaves, path) {
            bail!("resolution path {path:?} does not name a file or directory in either branch");
        }
    }

    let mut merged = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut used_resolutions = BTreeSet::new();
    for path in paths {
        let base_leaf = base.get(&path);
        let our_leaf = our_leaves.get(&path);
        let their_leaf = their_leaves.get(&path);
        let same = |a: Option<&Leaf>, b: Option<&Leaf>| match (a, b) {
            (Some(a), Some(b)) => same_leaf(a, b),
            (None, None) => true,
            _ => false,
        };
        let selected = if same(our_leaf, their_leaf) {
            our_leaf
        } else if same(our_leaf, base_leaf) {
            their_leaf
        } else if same(their_leaf, base_leaf) {
            our_leaf
        } else {
            let ours_resolution = ours_choice
                .iter()
                .find(|chosen| path == **chosen || path.starts_with(&format!("{chosen}/")));
            let theirs_resolution = theirs_choice
                .iter()
                .find(|chosen| path == **chosen || path.starts_with(&format!("{chosen}/")));
            match (ours_resolution, theirs_resolution) {
                (Some(chosen), None) => {
                    used_resolutions.insert(*chosen);
                    our_leaf
                }
                (None, Some(chosen)) => {
                    used_resolutions.insert(*chosen);
                    their_leaf
                }
                _ => {
                    if let (Some(base_leaf), Some(our_leaf), Some(their_leaf)) =
                        (base_leaf, our_leaf, their_leaf)
                    {
                        if let Some(merged_leaf) = merge_text_conflict(
                            repository, base_leaf, our_leaf, their_leaf, runtime,
                        )? {
                            merged.insert(path, merged_leaf);
                            continue;
                        }
                    }
                    conflicts.push(path.clone());
                    continue;
                }
            }
        };
        if let Some(leaf) = selected {
            merged.insert(path, leaf.clone());
        }
    }
    if !conflicts.is_empty() {
        bail!(
            "merge conflicts need --ours or --theirs for each path: {}",
            conflicts.join(", ")
        );
    }

    let mut structural_conflicts = BTreeSet::new();
    for path in merged.keys() {
        let mut rest = path.as_str();
        while let Some((parent, _)) = rest.rsplit_once('/') {
            if merged.contains_key(parent) {
                structural_conflicts.insert(parent.to_string());
            }
            rest = parent;
        }
    }
    for path in structural_conflicts {
        let prefix = format!("{path}/");
        if !merged.contains_key(&path)
            || !merged
                .range(prefix.clone()..)
                .next()
                .is_some_and(|(candidate, _)| candidate.starts_with(&prefix))
        {
            continue;
        }
        let ours_resolution = ours_choice
            .iter()
            .find(|chosen| path == **chosen || path.starts_with(&format!("{chosen}/")));
        let theirs_resolution = theirs_choice
            .iter()
            .find(|chosen| path == **chosen || path.starts_with(&format!("{chosen}/")));
        let (source, chosen) = match (ours_resolution, theirs_resolution) {
            (Some(chosen), None) => (&our_leaves, *chosen),
            (None, Some(chosen)) => (&their_leaves, *chosen),
            _ => bail!("merge creates a file-directory conflict at {path:?}; resolve it with --ours or --theirs"),
        };
        used_resolutions.insert(chosen);
        merged.retain(|candidate, _| {
            candidate != &path && !candidate.starts_with(&format!("{path}/"))
        });
        for (candidate, leaf) in source {
            if candidate == &path || candidate.starts_with(&format!("{path}/")) {
                merged.insert(candidate.clone(), leaf.clone());
            }
        }
    }
    if used_resolutions != resolved {
        let unused: Vec<_> = resolved.difference(&used_resolutions).collect();
        bail!(
            "resolution paths did not name merge conflicts: {}",
            unused.into_iter().copied().collect::<Vec<_>>().join(", ")
        );
    }
    for path in merged.keys() {
        let mut rest = path.as_str();
        while let Some((parent, _)) = rest.rsplit_once('/') {
            if merged.contains_key(parent) {
                bail!("merge resolution leaves a file-directory conflict at {parent:?}");
            }
            rest = parent;
        }
    }

    let mut cache = TreeCache::default();
    let tree = repository.put_leaves_with_runtime(&merged, &mut cache, runtime)?;
    let changes = repository.changes_between_with_runtime(
        Some(ours),
        Some(ours_version.tree),
        tree,
        &mut cache,
        runtime,
    )?;
    let version = repository.put_version_with_runtime(
        &Version {
            format: VERSION_FORMAT.to_string(),
            repository_id: repository.id().to_string(),
            parents: vec![ours, theirs],
            tree,
            author: author.to_string(),
            authored_at_unix_secs: now_unix_secs(),
            message: message.to_string(),
            changes,
            origin: None,
        },
        runtime,
    )?;
    repository
        .check_closure_with_runtime(version, runtime)
        .context("refusing to publish a merge with an incomplete or damaged object closure")?;
    refuse_signer_untrusted(signer, trust)?;
    let signed = runtime.sign_head(repository.id(), branch, version, signer, now_unix_secs())?;
    repository.set_head_with_runtime(&signed, Some(ours), runtime)?;
    Ok(MergeReport {
        kind: MergeKind::MergeCommit,
        version,
        first_parent: ours,
        second_parent: theirs,
        merge_base,
        branch: branch.to_string(),
        files: merged.len(),
    })
}

fn merge_text_conflict(
    repository: &Repository,
    base: &Leaf,
    ours: &Leaf,
    theirs: &Leaf,
    runtime: &mut RepositoryRuntime,
) -> Result<Option<Leaf>> {
    let Some((base_object, executable)) = merge_file_identity(base) else {
        return Ok(None);
    };
    let Some((ours_object, ours_executable)) = merge_file_identity(ours) else {
        return Ok(None);
    };
    let Some((theirs_object, theirs_executable)) = merge_file_identity(theirs) else {
        return Ok(None);
    };
    if executable != ours_executable || executable != theirs_executable {
        return Ok(None);
    }
    let Some(base_bytes) = read_merge_file(repository, base, base_object, runtime)? else {
        return Ok(None);
    };
    let Some(ours_bytes) = read_merge_file(repository, ours, ours_object, runtime)? else {
        return Ok(None);
    };
    let Some(theirs_bytes) = read_merge_file(repository, theirs, theirs_object, runtime)? else {
        return Ok(None);
    };
    let task = runtime.try_submit_cpu(move || {
        merge_text_bytes(&base_bytes, &ours_bytes, &theirs_bytes).map(|merged| {
            let hash = CasHash::digest(&merged);
            (merged, hash)
        })
    })?;
    let Some((merged, merged_hash)) = runtime.wait_cpu(task)? else {
        return Ok(None);
    };
    if merged_hash == ours_object.hash && merged.len() as u64 == ours_object.size {
        return Ok(Some(ours.clone()));
    }
    if merged_hash == theirs_object.hash && merged.len() as u64 == theirs_object.size {
        return Ok(Some(theirs.clone()));
    }
    if merged.len() < crate::archive_cas::chunking::MIN_CHUNK_BYTES {
        let object = repository.put_content_with_runtime(merged, runtime)?;
        return Ok(Some(if executable {
            Leaf::Executable(object)
        } else {
            Leaf::File(object)
        }));
    }
    let (proactor, handle, thermal, _) = runtime.parts_mut();
    let manifest = repository
        .store()
        .add_chunked_content_with_proactor_admitted(
            proactor,
            handle,
            &merged,
            super::MAX_CHUNK_MANIFEST_CHUNKS,
            |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
        )?;
    let file = ArchiveObject {
        hash: manifest.file_hash,
        size: manifest.file_size,
    };
    let task = runtime.try_submit_cpu(move || manifest.to_canonical_json())?;
    let manifest_bytes = runtime.wait_cpu(task)??;
    let manifest_object = repository.put_content_with_runtime(manifest_bytes, runtime)?;
    Ok(Some(Leaf::ChunkedFile {
        manifest: manifest_object,
        file,
        executable,
    }))
}

fn merge_file_identity(leaf: &Leaf) -> Option<(ArchiveObject, bool)> {
    match leaf {
        Leaf::File(object) => Some((*object, false)),
        Leaf::Executable(object) => Some((*object, true)),
        Leaf::ChunkedFile {
            file, executable, ..
        } => Some((*file, *executable)),
        Leaf::Symlink(_) => None,
    }
}

fn read_merge_file(
    repository: &Repository,
    leaf: &Leaf,
    file: ArchiveObject,
    runtime: &mut RepositoryRuntime,
) -> Result<Option<Vec<u8>>> {
    if file.size > MAX_TEXT_MERGE_BYTES as u64 {
        return Ok(None);
    }
    let bytes = match leaf {
        Leaf::File(_) | Leaf::Executable(_) => repository.read_object_with_runtime(
            file.hash,
            "merge file",
            MAX_TEXT_MERGE_BYTES,
            runtime,
        )?,
        Leaf::ChunkedFile { manifest, .. } => {
            let manifest = repository.file_manifest_with_runtime(*manifest, file, runtime)?;
            let mut bytes = Vec::with_capacity(file.size as usize);
            for chunk in &manifest.chunks {
                let (proactor, handle, thermal, _) = runtime.parts_mut();
                let chunk_bytes = repository.store().read_object_with_proactor_admitted(
                    proactor,
                    handle,
                    chunk.object,
                    crate::archive_cas::chunking::MAX_CHUNK_BYTES,
                    |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
                )?;
                if chunk_bytes.len() as u64 != chunk.object.size {
                    bail!("merge chunk {} has an unexpected size", chunk.object.hash);
                }
                bytes.extend_from_slice(&chunk_bytes);
            }
            bytes
        }
        Leaf::Symlink(_) => return Ok(None),
    };
    if bytes.len() as u64 != file.size || CasHash::digest(&bytes) != file.hash {
        bail!("merge file {} is damaged or has the wrong size", file.hash);
    }
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Ok(None);
    }
    Ok(Some(bytes))
}

#[derive(Clone)]
struct LineEdit<'a> {
    start: usize,
    end: usize,
    replacement: Vec<&'a [u8]>,
}

fn merge_text_bytes(base: &[u8], ours: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
    if base.len() > MAX_TEXT_MERGE_BYTES
        || ours.len() > MAX_TEXT_MERGE_BYTES
        || theirs.len() > MAX_TEXT_MERGE_BYTES
        || base.contains(&0)
        || ours.contains(&0)
        || theirs.contains(&0)
        || std::str::from_utf8(base).is_err()
        || std::str::from_utf8(ours).is_err()
        || std::str::from_utf8(theirs).is_err()
    {
        return None;
    }
    let base_lines = split_text_lines(base);
    let ours_lines = split_text_lines(ours);
    let theirs_lines = split_text_lines(theirs);
    let mut edits = line_edits(&base_lines, &ours_lines)?;
    edits.extend(line_edits(&base_lines, &theirs_lines)?);
    edits.sort_by_key(|edit| (edit.start, edit.end));

    let mut distinct = Vec::<LineEdit<'_>>::with_capacity(edits.len());
    for edit in edits {
        if let Some(previous) = distinct.last() {
            if edits_equal(previous, &edit) {
                continue;
            }
            if edits_overlap(previous, &edit) {
                return None;
            }
        }
        distinct.push(edit);
    }

    let mut merged = Vec::with_capacity(base.len() + ours.len().min(theirs.len()));
    let mut cursor = 0;
    for edit in distinct {
        if edit.start < cursor || edit.end > base_lines.len() {
            return None;
        }
        for line in &base_lines[cursor..edit.start] {
            merged.extend_from_slice(line);
        }
        for line in edit.replacement {
            merged.extend_from_slice(line);
        }
        cursor = edit.end;
    }
    for line in &base_lines[cursor..] {
        merged.extend_from_slice(line);
    }
    Some(merged)
}

fn split_text_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&bytes[start..=index]);
            start = index + 1;
        }
    }
    if start < bytes.len() {
        lines.push(&bytes[start..]);
    }
    lines
}

fn line_edits<'a>(base: &[&'a [u8]], changed: &[&'a [u8]]) -> Option<Vec<LineEdit<'a>>> {
    let width = changed.len().checked_add(1)?;
    let rows = base.len().checked_add(1)?;
    let cells = rows.checked_mul(width)?;
    if cells > MAX_TEXT_DIFF_CELLS {
        return None;
    }
    let mut lcs = vec![0_u32; cells];
    for base_index in (0..base.len()).rev() {
        for changed_index in (0..changed.len()).rev() {
            let index = base_index * width + changed_index;
            lcs[index] = if base[base_index] == changed[changed_index] {
                1 + lcs[(base_index + 1) * width + changed_index + 1]
            } else {
                lcs[(base_index + 1) * width + changed_index]
                    .max(lcs[base_index * width + changed_index + 1])
            };
        }
    }

    let mut edits = Vec::new();
    let (mut base_index, mut changed_index) = (0, 0);
    while base_index < base.len() || changed_index < changed.len() {
        if base_index < base.len()
            && changed_index < changed.len()
            && base[base_index] == changed[changed_index]
        {
            base_index += 1;
            changed_index += 1;
            continue;
        }
        let start = base_index;
        let mut replacement = Vec::new();
        while (base_index < base.len() || changed_index < changed.len())
            && !(base_index < base.len()
                && changed_index < changed.len()
                && base[base_index] == changed[changed_index])
        {
            let delete_score = if base_index < base.len() {
                lcs[(base_index + 1) * width + changed_index]
            } else {
                0
            };
            let insert_score = if changed_index < changed.len() {
                lcs[base_index * width + changed_index + 1]
            } else {
                0
            };
            if changed_index < changed.len()
                && (base_index == base.len() || insert_score > delete_score)
            {
                replacement.push(changed[changed_index]);
                changed_index += 1;
            } else if base_index < base.len() {
                base_index += 1;
            }
        }
        edits.push(LineEdit {
            start,
            end: base_index,
            replacement,
        });
    }
    Some(edits)
}

fn edits_equal(left: &LineEdit<'_>, right: &LineEdit<'_>) -> bool {
    left.start == right.start && left.end == right.end && left.replacement == right.replacement
}

fn edits_overlap(left: &LineEdit<'_>, right: &LineEdit<'_>) -> bool {
    match (left.start == left.end, right.start == right.end) {
        (true, true) => left.start == right.start,
        (true, false) => left.start >= right.start && left.start <= right.end,
        (false, true) => right.start >= left.start && right.start <= left.end,
        (false, false) => left.start < right.end && right.start < left.end,
    }
}

fn paths_contain_path(leaves: &BTreeMap<String, Leaf>, path: &str) -> bool {
    let prefix = format!("{path}/");
    leaves.contains_key(path)
        || leaves
            .range(prefix.clone()..)
            .next()
            .is_some_and(|(candidate, _)| candidate.starts_with(&prefix))
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
    commit_internal(tree, paths, message, author, signer, trust, None)
}

/// Like [`commit`], using the command runtime for changed-file and repository CAS I/O.
pub fn commit_with_runtime(
    tree: &Path,
    paths: &[String],
    message: &str,
    author: &str,
    signer: &Signer,
    trust: &Trust,
    runtime: &mut RepositoryRuntime,
) -> Result<CommitReport> {
    commit_internal(tree, paths, message, author, signer, trust, Some(runtime))
}

fn commit_internal(
    tree: &Path,
    paths: &[String],
    message: &str,
    author: &str,
    signer: &Signer,
    trust: &Trust,
    mut runtime: Option<&mut RepositoryRuntime>,
) -> Result<CommitReport> {
    if paths.is_empty() {
        bail!("name the paths to commit with --path");
    }
    if message.trim().is_empty() || author.trim().is_empty() {
        bail!("a commit needs a message and an author");
    }
    let status = if let Some(runtime) = runtime.as_deref_mut() {
        status_with_runtime(tree, runtime)?
    } else {
        status(tree)?
    };
    let branch = status.marker.branch.clone().ok_or_else(|| {
        anyhow!(
            "this tree is a checkout of one version, not of a branch; check out a branch to commit"
        )
    })?;
    let repository = &status.repository;
    let (head, _) = if let Some(runtime) = runtime.as_deref_mut() {
        repository.trusted_head_with_runtime(&branch, trust, runtime)?
    } else {
        repository.trusted_head(&branch, trust)?
    };
    refuse_signer_untrusted(signer, trust)?;
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
                let leaf = if let Some(runtime) = runtime.as_deref_mut() {
                    store_leaf_with_runtime(repository, tree, path, &status.found[path], runtime)?
                } else {
                    store_leaf(repository, tree, path, &status.found[path])?
                };
                leaves.insert(path.clone(), leaf);
            }
        }
    }
    let base = if let Some(runtime) = runtime.as_deref_mut() {
        repository.version_with_runtime(head, runtime)?
    } else {
        repository.version(head)?
    };
    let version = if let Some(runtime) = runtime.as_deref_mut() {
        record_with_runtime(
            repository,
            &leaves,
            Some((head, base.tree)),
            message,
            author,
            runtime,
        )?
    } else {
        record(
            repository,
            &leaves,
            Some((head, base.tree)),
            message,
            author,
        )?
    };
    refuse_signer_untrusted(signer, trust)?;
    let signed = if let Some(runtime) = runtime.as_deref_mut() {
        runtime.sign_head(repository.id(), &branch, version, signer, now_unix_secs())?
    } else {
        SignedHead::sign(repository.id(), &branch, version, signer, now_unix_secs())?
    };
    if let Some(runtime) = runtime {
        repository.set_head_with_runtime(&signed, Some(head), runtime)?;
    } else {
        repository.set_head(&signed, Some(head))?;
    }
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
    init_internal(
        repository, branch, tree, message, author, signer, trust, None,
    )
}

/// Like [`init`], using the command runtime to store changed files and repository objects.
#[allow(clippy::too_many_arguments)]
pub fn init_with_runtime(
    repository: &Repository,
    branch: &str,
    tree: &Path,
    message: &str,
    author: &str,
    signer: &Signer,
    trust: &Trust,
    runtime: &mut RepositoryRuntime,
) -> Result<CommitReport> {
    init_internal(
        repository,
        branch,
        tree,
        message,
        author,
        signer,
        trust,
        Some(runtime),
    )
}

#[allow(clippy::too_many_arguments)]
fn init_internal(
    repository: &Repository,
    branch: &str,
    tree: &Path,
    message: &str,
    author: &str,
    signer: &Signer,
    trust: &Trust,
    mut runtime: Option<&mut RepositoryRuntime>,
) -> Result<CommitReport> {
    super::validate_id(branch, "branch")?;
    // Refuse before writing anything: a head no trusted key verifies would leave the
    // branch stuck.
    refuse_signer_untrusted(signer, trust)?;
    if tree.join(MARKER_FILE).exists() {
        bail!("{} is a checkout already", tree.display());
    }
    let existing_head = if let Some(runtime) = runtime.as_deref_mut() {
        repository.head_with_runtime(branch, runtime)?
    } else {
        repository.head(branch)?
    };
    if existing_head.is_some() {
        bail!("{} has a branch {branch} already", repository.id());
    }
    if message.trim().is_empty() || author.trim().is_empty() {
        bail!("a first version needs a message and an author");
    }
    let found = if let Some(runtime) = runtime.as_deref_mut() {
        scan_with_runtime(tree, &BTreeMap::new(), repository, runtime)?
    } else {
        scan(tree, &BTreeMap::new())?
    };
    if found.is_empty() {
        bail!("{} holds nothing to record", tree.display());
    }
    let mut leaves = BTreeMap::new();
    for (path, leaf) in &found {
        let stored = if let Some(runtime) = runtime.as_deref_mut() {
            store_leaf_with_runtime(repository, tree, path, leaf, runtime)?
        } else {
            store_leaf(repository, tree, path, leaf)?
        };
        leaves.insert(path.clone(), stored);
    }
    let version = if let Some(runtime) = runtime.as_deref_mut() {
        record_with_runtime(repository, &leaves, None, message, author, runtime)?
    } else {
        record(repository, &leaves, None, message, author)?
    };
    refuse_signer_untrusted(signer, trust)?;
    let signed = if let Some(runtime) = runtime.as_deref_mut() {
        runtime.sign_head(repository.id(), branch, version, signer, now_unix_secs())?
    } else {
        SignedHead::sign(repository.id(), branch, version, signer, now_unix_secs())?
    };
    if let Some(runtime) = runtime {
        repository.set_head_with_runtime(&signed, None, runtime)?;
    } else {
        repository.set_head(&signed, None)?;
    }
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

/// Refuses a signer whose public key is not in the branch trust set, without
/// doing an otherwise unnecessary post-quantum sign/verify round trip.
pub(super) fn refuse_signer_untrusted(signer: &Signer, trust: &Trust) -> Result<()> {
    if trust.trusts_key(&signer.public)? {
        Ok(())
    } else {
        bail!(
            "{} would sign with a key the trusted keys do not hold; name a trusted key, or sign with one",
            signer.identity
        )
    }
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

fn record_with_runtime(
    repository: &Repository,
    leaves: &BTreeMap<String, Leaf>,
    parent: Option<(CasHash, CasHash)>,
    message: &str,
    author: &str,
    runtime: &mut RepositoryRuntime,
) -> Result<CasHash> {
    for path in leaves.keys() {
        for name in path.split('/') {
            validate_name(name).with_context(|| format!("path {path:?}"))?;
        }
    }
    let mut cache = TreeCache::default();
    let tree = repository.put_leaves_with_runtime(leaves, &mut cache, runtime)?;
    if parent.is_some_and(|(_, old)| old == tree) {
        bail!("the named changes leave the tree as it was; nothing to commit");
    }
    let changes = repository.changes_between_with_runtime(
        parent.map(|(version, _)| version),
        parent.map(|(_, tree)| tree),
        tree,
        &mut cache,
        runtime,
    )?;
    repository.put_version_with_runtime(
        &Version {
            format: VERSION_FORMAT.to_string(),
            repository_id: repository.id().to_string(),
            parents: parent.map(|(version, _)| version).into_iter().collect(),
            tree,
            author: author.to_string(),
            authored_at_unix_secs: now_unix_secs(),
            message: message.to_string(),
            changes,
            origin: None,
        },
        runtime,
    )
}

/// Stores the file or symlink at `path` in `tree`; returns what was stored (a file is
/// read again, so the stored bytes are what the version names).
fn store_leaf(repository: &Repository, tree: &Path, path: &str, seen: &Leaf) -> Result<Leaf> {
    let absolute = tree.join(path);
    match seen {
        Leaf::Symlink(target) => Ok(Leaf::Symlink(target.clone())),
        Leaf::ChunkedFile { .. } => Ok(seen.clone()),
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

fn store_leaf_with_runtime(
    repository: &Repository,
    tree: &Path,
    path: &str,
    seen: &Leaf,
    runtime: &mut RepositoryRuntime,
) -> Result<Leaf> {
    let absolute = tree.join(path);
    match seen {
        Leaf::Symlink(target) => Ok(Leaf::Symlink(target.clone())),
        Leaf::ChunkedFile { .. } => Ok(seen.clone()),
        Leaf::File(expected) | Leaf::Executable(expected) => {
            let executable = matches!(seen, Leaf::Executable(_));
            if expected.size < crate::archive_cas::chunking::MIN_CHUNK_BYTES as u64 {
                let (proactor, handle, thermal, _) = runtime.parts_mut();
                let actual = repository
                    .store()
                    .ingest_file_with_proactor_admitted(
                        proactor,
                        handle,
                        &absolute,
                        |proactor, handle| {
                            thermal.admit(proactor, handle).map_err(anyhow::Error::from)
                        },
                    )?
                    .object;
                if actual != *expected {
                    bail!(
                        "{} changed after status was computed: expected {} bytes hashing to {}, got {} bytes hashing to {}",
                        absolute.display(),
                        expected.size,
                        expected.hash,
                        actual.size,
                        actual.hash
                    );
                }
                return Ok(if executable {
                    Leaf::Executable(actual)
                } else {
                    Leaf::File(actual)
                });
            }
            let (proactor, handle, thermal, workers) = runtime.parts_mut();
            let manifest = repository
                .store()
                .ingest_chunked_file_with_worker_admitted(
                    proactor,
                    handle,
                    workers,
                    &absolute,
                    super::MAX_CHUNK_MANIFEST_CHUNKS,
                    |proactor, handle| thermal.admit(proactor, handle).map_err(anyhow::Error::from),
                )?;
            let actual = ArchiveObject {
                hash: manifest.file_hash,
                size: manifest.file_size,
            };
            if actual != *expected {
                bail!(
                    "{} changed after status was computed: expected {} bytes hashing to {}, got {} bytes hashing to {}",
                    absolute.display(),
                    expected.size,
                    expected.hash,
                    actual.size,
                    actual.hash
                );
            }
            let task = runtime.try_submit_cpu(move || manifest.to_canonical_json())?;
            let manifest_bytes = runtime.wait_cpu(task)??;
            let (proactor, handle, thermal, _) = runtime.parts_mut();
            let manifest = repository
                .store()
                .add_content_with_proactor_admitted(proactor, handle, manifest_bytes, |p, h| {
                    thermal.admit(p, h).map_err(anyhow::Error::from)
                })?
                .object;
            Ok(Leaf::ChunkedFile {
                manifest,
                file: actual,
                executable,
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

#[cfg(test)]
mod text_merge_tests {
    use super::merge_text_bytes;

    #[test]
    fn combines_non_overlapping_line_edits() {
        let merged = merge_text_bytes(
            b"one\ntwo\nthree\n",
            b"ONE\ntwo\nthree\n",
            b"one\ntwo\nTHREE\n",
        );
        assert_eq!(merged.as_deref(), Some(&b"ONE\ntwo\nTHREE\n"[..]));
    }

    #[test]
    fn combines_separate_insertions_and_preserves_missing_final_newline() {
        let merged = merge_text_bytes(
            b"one\ntwo\nthree",
            b"one\ninserted by ours\ntwo\nthree",
            b"one\ntwo\nthree\ninserted by theirs",
        );
        assert_eq!(
            merged.as_deref(),
            Some(&b"one\ninserted by ours\ntwo\nthree\ninserted by theirs"[..])
        );
    }

    #[test]
    fn identical_edits_collapse_and_overlapping_edits_conflict() {
        assert_eq!(
            merge_text_bytes(b"one\ntwo\n", b"one\nTWO\n", b"one\nTWO\n").as_deref(),
            Some(&b"one\nTWO\n"[..])
        );
        assert!(merge_text_bytes(
            b"one\ntwo\nthree\n",
            b"one\nOURS\nthree\n",
            b"one\nTHEIRS\nthree\n",
        )
        .is_none());
    }

    #[test]
    fn binary_content_is_not_auto_merged() {
        assert!(merge_text_bytes(b"a\0b", b"a\0ours", b"theirs\0b").is_none());
    }
}
