//! Purge: the step that actually frees space in an Archive CAS root.
//!
//! Removing a file from an archive writes a new manifest and leaves the old one on
//! disk, so the removed file's blob is still referenced and stays. A purge does both
//! halves of reclaiming it, for every archive in the root at once:
//!
//! 1. **Retire superseded manifests:** every manifest that a newer manifest of the same
//!    archive names as `supersedes_archive_root`, with its signature and its delete or
//!    add log.
//! 2. **Delete unreferenced objects:** every blob under `objects/` that no remaining
//!    manifest lists, including the retired manifests' own stored bytes.
//!
//! Step 2 has two depths ([`Sweep`]). The default checks only the objects a retired
//! manifest could have freed, one lookup each, which is all a purge after removals
//! needs. A full sweep also lists every object under `objects/`, to catch strays no
//! manifest ever listed (an interrupted run); on a spinning USB drive that listing
//! alone runs at about 90 objects a second.
//!
//! [`plan_purge`] reads everything and deletes nothing; [`execute_purge`] carries out
//! exactly that plan, and refuses if the root's manifests changed in between. The plan
//! names every file it will delete, so a person can approve the exact list.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive_cas::{ArchiveCasStorage, ArchiveEntry};
use crate::cas::CasHash;

/// A manifest the purge retires, and every file of it that is deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredManifest {
    pub archive_id: String,
    pub root: CasHash,
    /// The manifest that supersedes it, or `None` when the whole archive is deleted.
    pub superseded_by: Option<CasHash>,
    /// The manifest file, then its signature, delete log and add log where present.
    pub files: Vec<PathBuf>,
}

/// An object no remaining manifest references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeObject {
    pub hash: CasHash,
    pub size: u64,
    /// What it was: `archive:path` of a file only retired manifests listed, a retired
    /// manifest's own bytes, or nothing any manifest on disk lists.
    pub origin: String,
}

/// What a purge will delete. Built by [`plan_purge`]; nothing is deleted until
/// [`execute_purge`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgePlan {
    pub cas_root: PathBuf,
    pub manifests: Vec<RetiredManifest>,
    pub objects: Vec<PurgeObject>,
    /// Bytes of the retired manifests' files (signatures and logs included).
    pub manifest_file_bytes: u64,
    /// Every manifest on disk when the plan was made, with its root; execution
    /// refuses unless this is unchanged.
    live: Vec<(PathBuf, CasHash)>,
}

impl PurgePlan {
    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty() && self.objects.is_empty()
    }

    pub fn object_bytes(&self) -> u64 {
        self.objects.iter().map(|o| o.size).sum()
    }

    /// Everything the purge frees: objects plus the retired manifests' files.
    pub fn bytes(&self) -> u64 {
        self.object_bytes() + self.manifest_file_bytes
    }

    /// Number of files deleted, objects included.
    pub fn file_count(&self) -> usize {
        self.manifests.iter().map(|m| m.files.len()).sum::<usize>() + self.objects.len()
    }

    /// A short identity of exactly this plan (root, manifests, files and objects), so an
    /// approval can name the plan it approves.
    pub fn id(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.cas_root.to_string_lossy().as_bytes());
        for (path, root) in &self.live {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(root.as_bytes());
        }
        for manifest in &self.manifests {
            for file in &manifest.files {
                hasher.update(file.to_string_lossy().as_bytes());
            }
        }
        for object in &self.objects {
            hasher.update(object.hash.as_bytes());
        }
        hasher.finalize().to_hex()[..12].to_string()
    }
}

/// How far [`plan_purge`] looks for unreferenced objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sweep {
    /// Only objects the retired manifests list, and their own bytes.
    Retired,
    /// Every object under `objects/`, including strays no manifest lists.
    Full,
}

/// Progress of a plan or an execution, for a caller that shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeProgress {
    ReadingManifests { done: usize, total: usize },
    ListingObjects { found: usize },
    Deleting { done: usize, total: usize },
}

/// What [`execute_purge`] deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PurgeOutcome {
    pub files_removed: usize,
    pub objects_removed: usize,
    pub bytes_freed: u64,
}

/// Every manifest on disk with its root, sorted by path.
fn live_manifests(
    store: &ArchiveCasStorage,
    progress: &mut impl FnMut(PurgeProgress),
) -> Result<Vec<(PathBuf, crate::archive_cas::ArchiveManifest, CasHash)>> {
    let paths = store.list_manifests()?;
    let total = paths.len();
    let mut manifests = Vec::with_capacity(total);
    for (done, path) in paths.into_iter().enumerate() {
        progress(PurgeProgress::ReadingManifests { done, total });
        let (manifest, root) = store
            .read_manifest_and_root(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        manifests.push((path, manifest, root));
    }
    progress(PurgeProgress::ReadingManifests { done: total, total });
    Ok(manifests)
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

/// Works out what a purge of `store` deletes. Reads every manifest; looks up the objects
/// the retired manifests could free, or with [`Sweep::Full`] lists every object name.
/// Deletes nothing.
///
/// # Errors
/// An unreadable or non-canonical manifest, or an unreadable object directory.
pub fn plan_purge(
    store: &ArchiveCasStorage,
    sweep: Sweep,
    progress: impl FnMut(PurgeProgress),
) -> Result<PurgePlan> {
    plan(store, sweep, None, progress)
}

/// Works out what deleting archive `archive_id` entirely deletes: every manifest of it
/// (with signatures and logs) and every object no other archive in the root lists,
/// plus the superseded manifests of other archives, as [`plan_purge`] would. Deletes
/// nothing.
///
/// # Errors
/// As [`plan_purge`], or when the root holds no archive `archive_id`.
pub fn plan_archive_deletion(
    store: &ArchiveCasStorage,
    archive_id: &str,
    progress: impl FnMut(PurgeProgress),
) -> Result<PurgePlan> {
    let plan = plan(store, Sweep::Retired, Some(archive_id), progress)?;
    if !plan.manifests.iter().any(|m| m.archive_id == archive_id) {
        bail!("no archive {archive_id:?} in {}", store.root().display());
    }
    Ok(plan)
}

fn plan(
    store: &ArchiveCasStorage,
    sweep: Sweep,
    delete: Option<&str>,
    mut progress: impl FnMut(PurgeProgress),
) -> Result<PurgePlan> {
    let manifests = live_manifests(store, &mut progress)?;

    // A manifest is superseded when a newer one of the same archive names it.
    let mut successor: BTreeMap<(String, CasHash), CasHash> = BTreeMap::new();
    for (_, manifest, root) in &manifests {
        if let Some(previous) = manifest.supersedes_archive_root {
            successor.insert((manifest.archive_id.clone(), previous), *root);
        }
    }

    let mut retired = Vec::new();
    let mut referenced = BTreeSet::new();
    let mut retired_roots = BTreeMap::new();
    let mut only_in_retired: BTreeMap<CasHash, String> = BTreeMap::new();
    for (path, manifest, root) in &manifests {
        let files = || {
            manifest.entries.iter().filter_map(|entry| match entry {
                ArchiveEntry::File { path, object, .. } => Some((path, object.hash)),
                _ => None,
            })
        };
        let retire = match successor.get(&(manifest.archive_id.clone(), *root)) {
            Some(&next) => Some(Some(next)),
            None if delete == Some(manifest.archive_id.as_str()) => Some(None),
            None => None,
        };
        match retire {
            Some(superseded_by) => {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let mut owned = vec![path.clone()];
                for sidecar in ["signature", "delete-log", "add-log", "merge-log"] {
                    let side = store
                        .manifests_root()
                        .join(format!("{stem}.{sidecar}.json"));
                    if side.exists() {
                        owned.push(side);
                    }
                }
                retired_roots.insert(
                    *root,
                    if superseded_by.is_some() {
                        format!("superseded manifest of {}", manifest.archive_id)
                    } else {
                        format!("manifest of deleted archive {}", manifest.archive_id)
                    },
                );
                for (file, hash) in files() {
                    only_in_retired
                        .entry(hash)
                        .or_insert_with(|| format!("{}:{file}", manifest.archive_id));
                }
                retired.push(RetiredManifest {
                    archive_id: manifest.archive_id.clone(),
                    root: *root,
                    superseded_by,
                    files: owned,
                });
            }
            None => {
                referenced.insert(*root);
                referenced.extend(files().map(|(_, hash)| hash));
            }
        }
    }

    let candidates: Vec<CasHash> = match sweep {
        Sweep::Full => store.object_hashes_with_progress(|found| {
            progress(PurgeProgress::ListingObjects { found });
        })?,
        Sweep::Retired => retired_roots
            .keys()
            .chain(only_in_retired.keys())
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|hash| store.object_path(*hash).exists())
            .collect(),
    };
    let mut objects = Vec::new();
    for hash in candidates {
        if referenced.contains(&hash) {
            continue;
        }
        let origin = if let Some(what) = retired_roots.get(&hash) {
            what.clone()
        } else if let Some(file) = only_in_retired.get(&hash) {
            format!("removed file {file}")
        } else {
            "not listed by any manifest".to_string()
        };
        objects.push(PurgeObject {
            hash,
            size: file_size(&store.object_path(hash)),
            origin,
        });
    }
    objects.sort_by(|a, b| b.size.cmp(&a.size).then(a.hash.cmp(&b.hash)));

    let manifest_file_bytes = retired
        .iter()
        .flat_map(|m| &m.files)
        .map(|f| file_size(f))
        .sum();
    Ok(PurgePlan {
        cas_root: store.root().to_path_buf(),
        manifests: retired,
        objects,
        manifest_file_bytes,
        live: manifests
            .into_iter()
            .map(|(path, _, root)| (path, root))
            .collect(),
    })
}

/// Deletes exactly what `plan` lists: first the retired manifests' files, then the
/// objects. Refuses, deleting nothing, if the root's manifests are not the ones the plan
/// was made from (something was added, removed or rewritten since).
///
/// # Errors
/// A changed root, or a file that cannot be removed (earlier deletions stay done; plan
/// again to finish).
pub fn execute_purge(
    store: &ArchiveCasStorage,
    plan: &PurgePlan,
    mut progress: impl FnMut(PurgeProgress),
) -> Result<PurgeOutcome> {
    if store.root() != plan.cas_root {
        bail!("this plan is for {}", plan.cas_root.display());
    }
    let now: Vec<(PathBuf, CasHash)> = live_manifests(store, &mut progress)?
        .into_iter()
        .map(|(path, _, root)| (path, root))
        .collect();
    if now != plan.live {
        bail!("the archive's manifests changed since this plan was made; plan the purge again");
    }
    let total = plan.file_count();
    let mut outcome = PurgeOutcome::default();
    for file in plan.manifests.iter().flat_map(|m| &m.files) {
        progress(PurgeProgress::Deleting {
            done: outcome.files_removed,
            total,
        });
        let size = file_size(file);
        fs::remove_file(file).with_context(|| format!("failed to remove {}", file.display()))?;
        outcome.files_removed += 1;
        outcome.bytes_freed += size;
    }
    for object in &plan.objects {
        progress(PurgeProgress::Deleting {
            done: outcome.files_removed,
            total,
        });
        outcome.bytes_freed += store.remove_object(object.hash)?;
        outcome.files_removed += 1;
        outcome.objects_removed += 1;
    }
    progress(PurgeProgress::Deleting {
        done: outcome.files_removed,
        total,
    });
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_cas::{ArchiveManifest, ArchiveObject};
    use tempfile::tempdir;

    fn file(store: &ArchiveCasStorage, path: &str, bytes: &[u8]) -> ArchiveEntry {
        let object: ArchiveObject = store.add_content(bytes).unwrap().object;
        ArchiveEntry::File {
            path: path.to_string(),
            object,
            modified_at_unix_secs: None,
        }
    }

    /// Two archives sharing one blob; `docs` then has `secret.conf` removed.
    fn fixture() -> (tempfile::TempDir, ArchiveCasStorage, PathBuf, CasHash) {
        let dir = tempdir().unwrap();
        let store = ArchiveCasStorage::new(dir.path().join("cas")).unwrap();
        let shared = file(&store, "shared.txt", b"in both archives");
        let secret = file(&store, "secret.conf", b"PrivateKey = abc");
        let docs = ArchiveManifest::new(
            "docs",
            "Documents",
            1,
            vec![shared.clone(), secret, file(&store, "keep.txt", b"kept")],
        )
        .unwrap();
        let (docs_path, _) = store.write_manifest(&docs).unwrap();
        let other = ArchiveManifest::new("other", "Other", 1, vec![shared]).unwrap();
        store.write_manifest(&other).unwrap();
        let (v2, log) = docs
            .with_entries_removed(&["secret.conf".to_string()], "test", "jay", 2)
            .unwrap();
        let (v2_path, _) = store.write_manifest(&v2).unwrap();
        store.write_delete_log(&v2_path, &log).unwrap();
        let v1_root = docs.digest().unwrap();
        (dir, store, docs_path, v1_root)
    }

    #[test]
    fn plan_retires_the_superseded_manifest_and_its_unshared_objects_only() {
        let (_dir, store, v1_path, v1_root) = fixture();
        let before = store.object_hashes_with_progress(|_| {}).unwrap().len();
        let plan = plan_purge(&store, Sweep::Full, |_| {}).unwrap();
        assert_eq!(plan.manifests.len(), 1);
        assert_eq!(plan.manifests[0].root, v1_root);
        assert_eq!(plan.manifests[0].files, std::slice::from_ref(&v1_path));
        let origins: BTreeSet<&str> = plan.objects.iter().map(|o| o.origin.as_str()).collect();
        assert_eq!(
            origins,
            BTreeSet::from([
                "removed file docs:secret.conf",
                "superseded manifest of docs"
            ])
        );
        assert_eq!(
            store.object_hashes_with_progress(|_| {}).unwrap().len(),
            before,
            "planning deletes nothing"
        );

        let outcome = execute_purge(&store, &plan, |_| {}).unwrap();
        assert_eq!(outcome.objects_removed, 2);
        assert_eq!(outcome.files_removed, 3);
        assert!(!v1_path.exists());
        assert_eq!(
            store.object_hashes_with_progress(|_| {}).unwrap().len(),
            before - 2
        );
        // Everything the remaining manifests list is still there and verifies.
        for path in store.list_manifests().unwrap() {
            for entry in store.read_manifest(&path).unwrap().entries {
                if let ArchiveEntry::File { object, .. } = entry {
                    store.verify_object(object).unwrap();
                }
            }
        }
        assert!(plan_purge(&store, Sweep::Full, |_| {}).unwrap().is_empty());
    }

    #[test]
    fn a_plan_is_refused_after_the_archive_changes() {
        let (_dir, store, _, _) = fixture();
        let plan = plan_purge(&store, Sweep::Full, |_| {}).unwrap();
        let late = ArchiveManifest::new("late", "Late", 3, vec![]).unwrap();
        store.write_manifest(&late).unwrap();
        let before = store.object_hashes_with_progress(|_| {}).unwrap().len();
        assert!(execute_purge(&store, &plan, |_| {})
            .unwrap_err()
            .to_string()
            .contains("changed since this plan"));
        assert_eq!(
            store.object_hashes_with_progress(|_| {}).unwrap().len(),
            before
        );
    }

    #[test]
    fn signatures_and_logs_of_a_retired_manifest_are_listed_and_the_id_names_the_plan() {
        let (_dir, store, v1_path, _) = fixture();
        let stem = v1_path.file_stem().unwrap().to_str().unwrap().to_string();
        let signature = store
            .manifests_root()
            .join(format!("{stem}.signature.json"));
        fs::write(&signature, b"{}").unwrap();
        let plan = plan_purge(&store, Sweep::Full, |_| {}).unwrap();
        assert_eq!(plan.manifests[0].files, [v1_path, signature]);
        assert_eq!(
            plan.id(),
            plan_purge(&store, Sweep::Full, |_| {}).unwrap().id()
        );
        assert_eq!(plan.bytes(), plan.object_bytes() + plan.manifest_file_bytes);
    }

    #[test]
    fn a_full_sweep_also_finds_strays_the_default_leaves() {
        let (_dir, store, _, _) = fixture();
        let stray = store
            .add_content(b"left by an interrupted run")
            .unwrap()
            .object;
        let full = plan_purge(&store, Sweep::Full, |_| {}).unwrap();
        assert!(full
            .objects
            .iter()
            .any(|o| o.hash == stray.hash && o.origin == "not listed by any manifest"));
        let retired = plan_purge(&store, Sweep::Retired, |_| {}).unwrap();
        let without_stray: Vec<_> = full
            .objects
            .iter()
            .filter(|o| o.hash != stray.hash)
            .cloned()
            .collect();
        assert_eq!(retired.objects, without_stray);
        assert_eq!(retired.manifests, full.manifests);
    }

    #[test]
    fn deleting_an_archive_keeps_the_objects_other_archives_still_list() {
        let (_dir, store, v1_path, _) = fixture();
        let plan = plan_archive_deletion(&store, "docs", |_| {}).unwrap();
        assert_eq!(plan.manifests.len(), 2, "both versions of docs");
        assert!(plan.manifests.iter().any(|m| m.superseded_by.is_none()));
        let origins: BTreeSet<&str> = plan.objects.iter().map(|o| o.origin.as_str()).collect();
        assert!(
            origins.contains("removed file docs:secret.conf"),
            "{origins:?}"
        );
        assert!(
            origins.contains("manifest of deleted archive docs"),
            "{origins:?}"
        );
        // keep.txt was only in docs; shared.txt is also in "other" and stays.
        assert!(plan
            .objects
            .iter()
            .any(|o| o.origin == "removed file docs:keep.txt"));
        assert!(!plan.objects.iter().any(|o| o.origin.contains("shared.txt")));
        execute_purge(&store, &plan, |_| {}).unwrap();
        assert!(!v1_path.exists());
        let left: Vec<String> = store
            .list_manifests()
            .unwrap()
            .iter()
            .map(|p| store.read_manifest(p).unwrap().archive_id)
            .collect();
        assert_eq!(left, ["other"]);
        for entry in store
            .read_manifest(&store.list_manifests().unwrap()[0])
            .unwrap()
            .entries
        {
            if let ArchiveEntry::File { object, .. } = entry {
                store.verify_object(object).unwrap();
            }
        }
        assert!(plan_archive_deletion(&store, "docs", |_| {}).is_err());
    }
}
