//! Purge: the step that actually frees space in an Archive CAS root.
//!
//! Removing a file from an archive writes a new version and leaves the old one on disk,
//! so the removed file's blob is still referenced and stays. A purge reclaims it, for
//! every archive in the root at once, without losing the history:
//!
//! 1. **Retire superseded versions:** every version a later version names as a parent
//!    (a newer version of the same archive, or an archive it was merged into), once a
//!    later version is signed by the trusted key. A retired
//!    version keeps its manifest, its stored manifest object, its signature and its
//!    logs: the record of what the archive was stays, and stays verifiable
//!    (`archive_cas_verify` reports its dropped objects as dropped on retirement). A
//!    superseded version with no signed later version is not retired.
//! 2. **Delete unreferenced objects:** every blob under `objects/` that only retired
//!    versions list.
//!
//! Deleting a whole archive ([`plan_archive_deletion`]) is the one operation that also
//! deletes manifests: every version of that archive.
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

use crate::archive_cas::ArchiveCasStorage;
use crate::archive_view::{list_archives, signed_successor, PublicKey};
use crate::cas::CasHash;

/// A version the purge retires (or, deleting an archive, deletes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredManifest {
    pub archive_id: String,
    pub root: CasHash,
    /// The version that supersedes it, or `None` when the whole archive is deleted.
    pub superseded_by: Option<CasHash>,
    /// The signed later version it is retired under.
    pub signed_successor: Option<CasHash>,
    /// Files of it that are deleted: none for a retired version, whose manifest,
    /// signature and logs stay; for a deleted archive, the manifest file, then its
    /// signature and logs where present.
    pub files: Vec<PathBuf>,
}

/// A superseded version left as it is because no later version of its archive is
/// signed by the trusted key yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedSuccession {
    pub archive_id: String,
    pub root: CasHash,
    pub superseded_by: CasHash,
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
    /// Superseded versions not retired: sign a later version, then plan again.
    pub unsigned: Vec<UnsignedSuccession>,
    pub objects: Vec<PurgeObject>,
    /// Bytes of the retired manifests' files (signatures and logs included).
    pub manifest_file_bytes: u64,
    /// Every manifest on disk when the plan was made, with its root; execution
    /// refuses unless this is unchanged.
    live: Vec<(PathBuf, CasHash)>,
}

impl PurgePlan {
    /// Whether the plan deletes nothing. A version retired by an earlier purge stays
    /// superseded, and is listed again with nothing left to delete.
    pub fn is_empty(&self) -> bool {
        self.file_count() == 0
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
/// the retired versions could free, or with [`Sweep::Full`] lists every object name.
/// Signatures are checked against `trusted`; without it nothing is retired. Deletes
/// nothing.
///
/// # Errors
/// An unreadable or non-canonical manifest, or an unreadable object directory.
pub fn plan_purge(
    store: &ArchiveCasStorage,
    sweep: Sweep,
    trusted: Option<&PublicKey>,
    progress: impl FnMut(PurgeProgress),
) -> Result<PurgePlan> {
    plan(store, sweep, None, trusted, progress)
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
    trusted: Option<&PublicKey>,
    progress: impl FnMut(PurgeProgress),
) -> Result<PurgePlan> {
    let plan = plan(store, Sweep::Retired, Some(archive_id), trusted, progress)?;
    if !plan.manifests.iter().any(|m| m.archive_id == archive_id) {
        bail!("no archive {archive_id:?} in {}", store.root().display());
    }
    Ok(plan)
}

fn plan(
    store: &ArchiveCasStorage,
    sweep: Sweep,
    delete: Option<&str>,
    trusted: Option<&PublicKey>,
    mut progress: impl FnMut(PurgeProgress),
) -> Result<PurgePlan> {
    let manifests = live_manifests(store, &mut progress)?;
    let listings = list_archives(store.root(), trusted)?;

    // A manifest is superseded when a later one names it as a parent: a newer version
    // of its archive, or an archive it was merged into.
    let mut successor: BTreeMap<CasHash, CasHash> = BTreeMap::new();
    for (_, manifest, root) in &manifests {
        for parent in manifest.parents() {
            successor.insert(parent, *root);
        }
    }

    let mut retired = Vec::new();
    let mut unsigned = Vec::new();
    let mut referenced = BTreeSet::new();
    let mut retired_roots = BTreeMap::new();
    let mut only_in_retired: BTreeMap<CasHash, String> = BTreeMap::new();
    for (path, manifest, root) in &manifests {
        let files = || {
            manifest
                .file_objects()
                .map(|(path, object)| (path, object.hash))
        };
        let superseded_by = successor.get(root).copied();
        let deleting = delete == Some(manifest.archive_id.as_str());
        let signed = superseded_by
            .and_then(|_| signed_successor(&listings, *root))
            .map(|listing| listing.root);
        if deleting {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let mut owned = vec![path.clone()];
            for sidecar in [
                "signature",
                "delete-log",
                "add-log",
                "merge-log",
                "unpack-log",
            ] {
                let side = store
                    .manifests_root()
                    .join(format!("{stem}.{sidecar}.json"));
                if side.exists() {
                    owned.push(side);
                }
            }
            retired_roots.insert(
                *root,
                format!("manifest of deleted archive {}", manifest.archive_id),
            );
            for (file, hash) in files() {
                // A file of an archive being deleted was not removed by anyone.
                only_in_retired.insert(hash, format!("file {}:{file}", manifest.archive_id));
            }
            for attached in &manifest.unverified_history {
                only_in_retired.insert(
                    attached.object.hash,
                    format!("attached {}:{}", manifest.archive_id, attached.name),
                );
            }
            retired.push(RetiredManifest {
                archive_id: manifest.archive_id.clone(),
                root: *root,
                superseded_by,
                signed_successor: signed,
                files: owned,
            });
        } else if let (Some(next), Some(signed)) = (superseded_by, signed) {
            // Retired: the version's own record stays (manifest, its stored object,
            // signature, logs, attachments); only objects no live version lists can go.
            referenced.insert(*root);
            referenced.extend(manifest.attached_objects().map(|object| object.hash));
            for (file, hash) in files() {
                only_in_retired
                    .entry(hash)
                    .or_insert_with(|| format!("removed file {}:{file}", manifest.archive_id));
            }
            retired.push(RetiredManifest {
                archive_id: manifest.archive_id.clone(),
                root: *root,
                superseded_by: Some(next),
                signed_successor: Some(signed),
                files: Vec::new(),
            });
        } else {
            if let Some(next) = superseded_by {
                unsigned.push(UnsignedSuccession {
                    archive_id: manifest.archive_id.clone(),
                    root: *root,
                    superseded_by: next,
                });
            }
            referenced.insert(*root);
            referenced.extend(files().map(|(_, hash)| hash));
            referenced.extend(manifest.attached_objects().map(|object| object.hash));
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
            // Drop what the remaining archives still list before touching the disk: a
            // retired archive can list hundreds of thousands of objects another archive
            // shares, and each lookup on a spinning drive costs ~10 ms.
            .filter(|hash| !referenced.contains(hash))
            .filter(|hash| store.has_object(*hash))
            .collect(),
    };
    let mut objects = Vec::new();
    for hash in candidates {
        if referenced.contains(&hash) {
            continue;
        }
        let origin = if let Some(what) = retired_roots.get(&hash) {
            what.clone()
        } else if let Some(what) = only_in_retired.get(&hash) {
            what.clone()
        } else {
            "not listed by any manifest".to_string()
        };
        objects.push(PurgeObject {
            hash,
            size: store.stored_bytes(hash).unwrap_or(0),
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
        unsigned,
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
    use crate::archive_cas::{ArchiveAttachment, ArchiveEntry, ArchiveManifest, ArchiveObject};
    use crate::archive_cas_sign::sign_manifest_and_write;
    use loadngo_pq_crypto::{default_registry, PqSchemeRegistry, SignatureSchemeId};
    use tempfile::tempdir;

    fn file(store: &ArchiveCasStorage, path: &str, bytes: &[u8]) -> ArchiveEntry {
        let object: ArchiveObject = store.add_content(bytes).unwrap().object;
        ArchiveEntry::File {
            path: path.to_string(),
            object,
            modified_at_unix_secs: None,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: ArchiveCasStorage,
        v1_path: PathBuf,
        v1_root: CasHash,
        v2: ArchiveManifest,
        secret: CasHash,
        key: PublicKey,
        private: loadngo_pq_crypto::PrivateKey,
    }

    impl Fixture {
        fn sign_v2(&self) -> CasHash {
            sign_manifest_and_write(&self.store, &self.v2, "test", &self.key, &self.private, 3)
                .unwrap();
            self.v2.digest().unwrap()
        }
    }

    /// Two archives sharing one blob; `docs` then has `secret.conf` removed (unsigned).
    fn fixture() -> Fixture {
        let dir = tempdir().unwrap();
        let store = ArchiveCasStorage::new(dir.path().join("cas")).unwrap();
        let shared = file(&store, "shared.txt", b"in both archives");
        let secret = file(&store, "secret.conf", b"PrivateKey = abc");
        let ArchiveEntry::File { object, .. } = &secret else {
            unreachable!()
        };
        let secret_hash = object.hash;
        let docs = ArchiveManifest::new(
            "docs",
            "Documents",
            1,
            vec![shared.clone(), secret, file(&store, "keep.txt", b"kept")],
        )
        .unwrap();
        let (v1_path, _) = store.write_manifest(&docs).unwrap();
        let other = ArchiveManifest::new("other", "Other", 1, vec![shared]).unwrap();
        store.write_manifest(&other).unwrap();
        let v2 = docs
            .with_entries_removed(&["secret.conf".to_string()], "test", "jay", 2)
            .unwrap();
        store.write_manifest(&v2).unwrap();
        let registry = default_registry();
        let (key, private) = registry
            .get(&SignatureSchemeId::Dilithium2)
            .unwrap()
            .keygen()
            .unwrap();
        Fixture {
            _dir: dir,
            v1_root: docs.digest().unwrap(),
            store,
            v1_path,
            v2,
            secret: secret_hash,
            key,
            private,
        }
    }

    #[test]
    fn a_retired_version_keeps_its_record_and_loses_only_what_no_live_version_lists() {
        let f = fixture();
        let v2_root = f.sign_v2();
        let signature = f.store.manifests_root().join(format!(
            "{}.signature.json",
            f.v1_path.file_stem().unwrap().to_str().unwrap()
        ));
        fs::write(&signature, b"{}").unwrap();
        let before = f.store.object_hashes_with_progress(|_| {}).unwrap().len();
        let plan = plan_purge(&f.store, Sweep::Full, Some(&f.key), |_| {}).unwrap();
        assert_eq!(plan.manifests.len(), 1);
        let retired = &plan.manifests[0];
        assert_eq!(retired.root, f.v1_root);
        assert_eq!(retired.signed_successor, Some(v2_root));
        assert!(retired.files.is_empty(), "nothing of its record is deleted");
        let origins: Vec<&str> = plan.objects.iter().map(|o| o.origin.as_str()).collect();
        assert_eq!(origins, ["removed file docs:secret.conf"]);
        assert_eq!(
            f.store.object_hashes_with_progress(|_| {}).unwrap().len(),
            before,
            "planning deletes nothing"
        );
        assert_eq!(
            plan.id(),
            plan_purge(&f.store, Sweep::Full, Some(&f.key), |_| {})
                .unwrap()
                .id()
        );

        let outcome = execute_purge(&f.store, &plan, |_| {}).unwrap();
        assert_eq!((outcome.objects_removed, outcome.files_removed), (1, 1));
        assert!(!f.store.has_object(f.secret));
        assert!(f.v1_path.exists() && signature.exists());
        assert!(f.store.has_object(f.v1_root), "its stored manifest stays");
        assert_eq!(
            f.store.read_manifest(&f.v1_path).unwrap().digest().unwrap(),
            f.v1_root
        );
        // Everything the live versions list is still there and verifies.
        for path in f.store.list_manifests().unwrap() {
            if path == f.v1_path {
                continue;
            }
            for entry in f.store.read_manifest(&path).unwrap().entries {
                if let ArchiveEntry::File { object, .. } = entry {
                    f.store.verify_object(object).unwrap();
                }
            }
        }
        assert!(plan_purge(&f.store, Sweep::Full, Some(&f.key), |_| {})
            .unwrap()
            .is_empty());
    }

    #[test]
    fn nothing_is_retired_until_a_later_version_is_signed_by_the_trusted_key() {
        let f = fixture();
        for plan in [
            plan_purge(&f.store, Sweep::Retired, Some(&f.key), |_| {}).unwrap(),
            plan_purge(&f.store, Sweep::Retired, None, |_| {}).unwrap(),
        ] {
            assert!(plan.is_empty(), "{plan:?}");
            assert_eq!(plan.unsigned.len(), 1);
            assert_eq!(plan.unsigned[0].root, f.v1_root);
        }
        f.sign_v2();
        assert!(plan_purge(&f.store, Sweep::Retired, None, |_| {})
            .unwrap()
            .is_empty());
        let (other_key, _) = default_registry()
            .get(&SignatureSchemeId::Dilithium2)
            .unwrap()
            .keygen()
            .unwrap();
        assert!(
            plan_purge(&f.store, Sweep::Retired, Some(&other_key), |_| {})
                .unwrap()
                .is_empty()
        );
        assert!(!plan_purge(&f.store, Sweep::Retired, Some(&f.key), |_| {})
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_plan_is_refused_after_the_archive_changes() {
        let f = fixture();
        f.sign_v2();
        let plan = plan_purge(&f.store, Sweep::Full, Some(&f.key), |_| {}).unwrap();
        let late = ArchiveManifest::new("late", "Late", 3, vec![]).unwrap();
        f.store.write_manifest(&late).unwrap();
        let before = f.store.object_hashes_with_progress(|_| {}).unwrap().len();
        assert!(execute_purge(&f.store, &plan, |_| {})
            .unwrap_err()
            .to_string()
            .contains("changed since this plan"));
        assert_eq!(
            f.store.object_hashes_with_progress(|_| {}).unwrap().len(),
            before
        );
    }

    #[test]
    fn a_full_sweep_also_finds_strays_the_default_leaves() {
        let f = fixture();
        f.sign_v2();
        let stray = f
            .store
            .add_content(b"left by an interrupted run")
            .unwrap()
            .object;
        let full = plan_purge(&f.store, Sweep::Full, Some(&f.key), |_| {}).unwrap();
        assert!(full
            .objects
            .iter()
            .any(|o| o.hash == stray.hash && o.origin == "not listed by any manifest"));
        let retired = plan_purge(&f.store, Sweep::Retired, Some(&f.key), |_| {}).unwrap();
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
        let f = fixture();
        let plan = plan_archive_deletion(&f.store, "docs", None, |_| {}).unwrap();
        assert_eq!(plan.manifests.len(), 2, "both versions of docs");
        assert!(plan.manifests.iter().all(|m| !m.files.is_empty()));
        let origins: BTreeSet<&str> = plan.objects.iter().map(|o| o.origin.as_str()).collect();
        assert!(origins.contains("file docs:secret.conf"), "{origins:?}");
        assert!(
            origins.contains("manifest of deleted archive docs"),
            "{origins:?}"
        );
        // keep.txt was only in docs; shared.txt is also in "other" and stays.
        assert!(plan
            .objects
            .iter()
            .any(|o| o.origin == "file docs:keep.txt"));
        assert!(!plan.objects.iter().any(|o| o.origin.contains("shared.txt")));
        execute_purge(&f.store, &plan, |_| {}).unwrap();
        assert!(!f.v1_path.exists());
        let left: Vec<String> = f
            .store
            .list_manifests()
            .unwrap()
            .iter()
            .map(|p| f.store.read_manifest(p).unwrap().archive_id)
            .collect();
        assert_eq!(left, ["other"]);
        for entry in f
            .store
            .read_manifest(&f.store.list_manifests().unwrap()[0])
            .unwrap()
            .entries
        {
            if let ArchiveEntry::File { object, .. } = entry {
                f.store.verify_object(object).unwrap();
            }
        }
        assert!(plan_archive_deletion(&f.store, "docs", None, |_| {}).is_err());
    }

    #[test]
    fn merged_sources_retire_under_the_signed_merged_archive_and_attachments_stay() {
        let f = fixture();
        // `docs` v1 carries an old log as unverified history; `other` stays as it was.
        let log = f.store.add_content(b"{\"old\":\"log\"}").unwrap().object;
        let docs = f.store.read_manifest(&f.v1_path).unwrap();
        let docs = docs
            .with_unverified_history(
                vec![ArchiveAttachment {
                    name: "docs.delete-log.json".into(),
                    object: log,
                }],
                2,
            )
            .unwrap();
        f.store.write_manifest(&docs).unwrap();
        let other_path = f
            .store
            .list_manifests()
            .unwrap()
            .into_iter()
            .find(|p| f.store.read_manifest(p).unwrap().archive_id == "other")
            .unwrap();
        let (other, other_root) = f.store.read_manifest_and_root(&other_path).unwrap();
        let docs_root = docs.digest().unwrap();
        let merged = ArchiveManifest::merged(
            "all",
            "All",
            4,
            &[("Docs", &docs, docs_root), ("Other", &other, other_root)],
            "jay",
            "one archive",
        )
        .unwrap();
        f.store.write_manifest(&merged).unwrap();

        let unsigned = plan_purge(&f.store, Sweep::Full, Some(&f.key), |_| {}).unwrap();
        assert!(unsigned.manifests.is_empty(), "nothing is retired unsigned");

        sign_manifest_and_write(&f.store, &merged, "test", &f.key, &f.private, 5).unwrap();
        let merged_root = merged.digest().unwrap();
        let plan = plan_purge(&f.store, Sweep::Full, Some(&f.key), |_| {}).unwrap();
        let retired: Vec<(&str, Option<CasHash>)> = plan
            .manifests
            .iter()
            .map(|m| (m.archive_id.as_str(), m.signed_successor))
            .collect();
        // docs v1 (by the unsigned v2 and the attaching version), docs with history, and
        // other: each is followed forward to the signed merged archive.
        assert_eq!(retired.len(), 3, "{retired:?}");
        assert!(retired
            .iter()
            .all(|(_, signed)| *signed == Some(merged_root)));
        // The retired version holding the attachment keeps it; the merged archive and
        // docs' live unsigned v2 between them list every file, so nothing goes.
        assert!(plan.objects.iter().all(|o| o.hash != log.hash));
        assert!(
            plan.objects.is_empty(),
            "the merged archive and the live v2 list everything: {:?}",
            plan.objects
        );
    }
}
