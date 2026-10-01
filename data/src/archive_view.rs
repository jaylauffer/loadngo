//! A read-only view of one Archive CAS archive, for callers that should see archived
//! files but never the store's write paths (a local model's tools), and a listing of
//! every archive under a root ([`list_archives`]), as the Archive CAS browser shows them.
//!
//! Any archive can be opened ([`ArchiveView::open`]); its [`Signature`] says whether it
//! is signed by the trusted key. A signature is trusted only when all three hold: its signature file verifies against
//! a trusted public key ([`archive_cas_sign::verify_signature`]), the manifest it names
//! is stored as an intact CAS object ([`archive_cas_sign::present_root`]), and that
//! object's digest equals the signed root. Every file read is BLAKE3-checked against
//! the manifest before any byte is returned.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::archive_cas::{
    ArchiveCasStorage, ArchiveEntry, ArchiveManifest, ArchiveObject, ArchiveRecord,
};
use crate::archive_cas_sign::{
    present_root, read_public_key, signature_path, verify_signature, SignedArchiveRoot,
};
use crate::cas::CasHash;
pub use loadngo_pq_crypto::PublicKey;

/// Largest single object [`ArchiveView::read_file`] loads.
pub const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// One child in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewChild {
    pub name: String,
    pub kind: &'static str,
    pub object: Option<ArchiveObject>,
}

/// Whether an archive is signed by the trusted key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    /// Its signature verifies against the trusted key, over this archive's root.
    Verified {
        signer: String,
        signed_at_unix_secs: u64,
    },
    /// It has no signature file.
    Unsigned,
    /// A signature file exists but does not verify (reason given).
    NotVerified(String),
}

impl Signature {
    /// `signed by <signer>`, `unsigned`, or `signature not verified (<reason>)`.
    pub fn describe(&self) -> String {
        match self {
            Self::Verified { signer, .. } => format!("signed by {signer}"),
            Self::Unsigned => "unsigned".to_string(),
            Self::NotVerified(reason) => format!("signature not verified ({reason})"),
        }
    }

    /// Whether the signature verified against the trusted key.
    #[must_use]
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    /// The signature of archive `archive_id` at `root`, checked against `trusted`.
    pub(crate) fn check(
        store: &ArchiveCasStorage,
        archive_id: &str,
        root: CasHash,
        trusted: Option<&PublicKey>,
    ) -> Self {
        let path = signature_path(store, archive_id, root);
        let Ok(bytes) = fs::read(&path) else {
            return Self::Unsigned;
        };
        let Some(key) = trusted else {
            return Self::NotVerified("no trusted key given".into());
        };
        let signed: SignedArchiveRoot = match serde_json::from_slice(&bytes) {
            Ok(signed) => signed,
            Err(error) => return Self::NotVerified(format!("unreadable: {error}")),
        };
        match verify_signature(&signed, key) {
            Ok(signed_root) if signed_root == root && signed.archive_id == archive_id => {
                Self::Verified {
                    signer: signed.signer_identity,
                    signed_at_unix_secs: signed.signed_at_unix_secs,
                }
            }
            Ok(_) => Self::NotVerified("it signs a different root".into()),
            Err(error) => Self::NotVerified(format!("{error:#}")),
        }
    }
}

/// What a directory of a snapshot holds, everything below it counted
/// ([`ArchiveView::tally`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    pub files: usize,
    /// The files' sizes added up ...
    pub bytes: u64,
    /// ... and the distinct contents among them, which is what the store holds: equal
    /// files are stored once.
    pub objects: usize,
    pub object_bytes: u64,
    pub dirs: usize,
    pub links: usize,
    /// Entries the capture could not read.
    pub unreadable: usize,
    /// Entries the owner excluded.
    pub excluded: usize,
    /// Each directory directly inside: the files below it and their bytes.
    pub subdirs: BTreeMap<String, (usize, u64)>,
}

/// One archive manifest under a root, from its header alone (no entries read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveListing {
    pub cas_root: PathBuf,
    pub manifest_path: PathBuf,
    pub archive_id: String,
    pub source_label: String,
    pub created_at_unix_secs: u64,
    /// The root the manifest's file name records; [`ArchiveView::open`] checks it.
    pub root: CasHash,
    /// The versions this one was made from: a merge has one per source, a fresh capture
    /// none.
    pub parents: Vec<CasHash>,
    /// How it was made from them (v3; none before).
    pub records: Vec<ArchiveRecord>,
    /// Old sidecar logs attached as found (v3).
    pub attachments: usize,
    /// A later version, of this archive or (by a merge) another, names this one as a
    /// parent.
    pub superseded: bool,
    pub signature: Signature,
}

/// The nearest later version after `root`, following parents forward through any
/// archive (a merge's sources continue in the merged archive), whose signature
/// verified, if any: the signed record a retired version's data may be dropped under.
/// `listings` is every version under one root, as [`list_archives`] returns them.
#[must_use]
pub fn signed_successor(listings: &[ArchiveListing], root: CasHash) -> Option<&ArchiveListing> {
    let mut seen = HashSet::from([root]);
    let mut frontier = vec![root];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for at in frontier {
            for later in listings.iter().filter(|l| l.parents.contains(&at)) {
                if later.signature.is_verified() {
                    return Some(later);
                }
                if seen.insert(later.root) {
                    next.push(later.root);
                }
            }
        }
        frontier = next;
    }
    None
}

#[derive(serde::Deserialize)]
struct ManifestHead {
    archive_id: String,
    source_label: String,
    created_at_unix_secs: u64,
    #[serde(default)]
    supersedes_archive_root: Option<CasHash>,
    #[serde(default)]
    parents: Vec<CasHash>,
    #[serde(default)]
    records: Vec<ArchiveRecord>,
    #[serde(default)]
    unverified_history: Vec<serde::de::IgnoredAny>,
}

/// Most bytes [`read_head`] reads looking for the end of a header.
const MAX_HEAD_BYTES: u64 = 64 * 1024 * 1024;

/// The fields before `entries` in a canonical manifest, read from the file's first bytes.
/// A manifest can be hundreds of megabytes; its header is a few hundred bytes, more
/// when its change records name many paths.
fn read_head(path: &Path) -> Result<ManifestHead> {
    const MARK: &[u8] = b"\n  \"entries\"";
    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .take(MAX_HEAD_BYTES);
    let mut start = Vec::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    let end = loop {
        let searched = start.len().saturating_sub(MARK.len());
        let read = file.read(&mut chunk)?;
        start.extend_from_slice(&chunk[..read]);
        if let Some(at) = start[searched..]
            .windows(MARK.len())
            .position(|w| w == MARK)
        {
            break searched + at;
        }
        if read == 0 {
            bail!("{} has no manifest header", path.display());
        }
    };
    let text = std::str::from_utf8(&start[..end])
        .with_context(|| format!("header of {} is not UTF-8", path.display()))?;
    let head = format!("{}\n}}", text.trim_end_matches(','));
    serde_json::from_str(&head).with_context(|| format!("bad header in {}", path.display()))
}

/// Every archive manifest under `cas_root`, oldest first within each archive, with
/// superseded versions marked and signatures checked against `trusted`. Reads only
/// each manifest's header and signature file.
///
/// # Errors
/// When the root's `manifests/` directory cannot be read.
pub fn list_archives(cas_root: &Path, trusted: Option<&PublicKey>) -> Result<Vec<ArchiveListing>> {
    let store = ArchiveCasStorage::new(cas_root)?;
    let mut listings = Vec::new();
    let mut replaced = HashSet::new();
    for path in store.list_manifests()? {
        let Some(root) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.rsplit_once('-'))
            .and_then(|(_, hex)| hex.parse::<CasHash>().ok())
        else {
            continue;
        };
        let Ok(head) = read_head(&path) else {
            continue;
        };
        let parents: Vec<CasHash> = head
            .supersedes_archive_root
            .into_iter()
            .chain(head.parents)
            .collect();
        replaced.extend(parents.iter().copied());
        let signature = Signature::check(&store, &head.archive_id, root, trusted);
        listings.push(ArchiveListing {
            cas_root: cas_root.to_path_buf(),
            manifest_path: path,
            archive_id: head.archive_id,
            source_label: head.source_label,
            created_at_unix_secs: head.created_at_unix_secs,
            root,
            parents,
            records: head.records,
            attachments: head.unverified_history.len(),
            superseded: false,
            signature,
        });
    }
    for listing in &mut listings {
        listing.superseded = replaced.contains(&listing.root);
    }
    listings.sort_by(|a, b| {
        (&a.archive_id, a.created_at_unix_secs).cmp(&(&b.archive_id, b.created_at_unix_secs))
    });
    Ok(listings)
}

pub struct ArchiveView {
    store: ArchiveCasStorage,
    manifest: ArchiveManifest,
    root: CasHash,
    signature: Signature,
    by_path: BTreeMap<String, usize>,
    children: BTreeMap<String, Vec<usize>>,
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn kind(entry: &ArchiveEntry) -> &'static str {
    match entry {
        ArchiveEntry::Directory { .. } => "dir",
        ArchiveEntry::File { .. } => "file",
        ArchiveEntry::Symlink { .. } => "link",
        ArchiveEntry::Unreadable { .. } => "unreadable",
        ArchiveEntry::Excluded { .. } => "excluded",
    }
}

impl ArchiveView {
    /// The most recently signed snapshot under `cas_root` that verifies against the
    /// public key at `trusted_key`. Unsigned manifests are never opened.
    ///
    /// # Errors
    /// When no signature verifies, or the verified manifest is missing or altered.
    pub fn open_newest_verified(cas_root: &Path, trusted_key: &Path) -> Result<Self> {
        let store = ArchiveCasStorage::new(cas_root)?;
        let key = read_public_key(trusted_key)?;
        let mut best: Option<(u64, SignedArchiveRoot, CasHash)> = None;
        let mut rejected = Vec::new();
        for entry in fs::read_dir(store.manifests_root())
            .with_context(|| format!("failed to list {}", store.manifests_root().display()))?
        {
            let path = entry?.path();
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".signature.json"))
            {
                continue;
            }
            let signed: SignedArchiveRoot = match fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(Into::into))
            {
                Ok(signed) => signed,
                Err(error) => {
                    rejected.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            match verify_signature(&signed, &key) {
                Ok(root) => {
                    if best
                        .as_ref()
                        .is_none_or(|(at, ..)| signed.signed_at_unix_secs > *at)
                    {
                        best = Some((signed.signed_at_unix_secs, signed, root));
                    }
                }
                Err(error) => rejected.push(format!("{}: {error:#}", path.display())),
            }
        }
        let Some((signed_at_unix_secs, signed, root)) = best else {
            bail!(
                "no snapshot under {} is signed by the trusted key{}",
                cas_root.display(),
                if rejected.is_empty() {
                    String::new()
                } else {
                    format!(" (rejected: {})", rejected.join("; "))
                }
            );
        };
        let manifest_path =
            store
                .manifests_root()
                .join(format!("{}-{}.json", signed.archive_id, root.to_hex()));
        let manifest = store.read_manifest(&manifest_path)?;
        let present = present_root(&store, &manifest)?;
        if present != root || manifest.archive_id != signed.archive_id {
            bail!(
                "{} does not match its signature (root {} vs signed {})",
                manifest_path.display(),
                present,
                root
            );
        }
        Ok(Self::index(
            store,
            manifest,
            root,
            Signature::Verified {
                signer: signed.signer_identity,
                signed_at_unix_secs,
            },
        ))
    }

    /// The archive whose manifest is at `manifest_path` under `cas_root`, signed or not.
    /// The manifest must be stored intact as a CAS object; its [`Signature`] is checked
    /// against `trusted`.
    ///
    /// # Errors
    /// When the manifest cannot be read, is not canonical, or is not stored intact.
    pub fn open(
        cas_root: &Path,
        manifest_path: &Path,
        trusted: Option<&PublicKey>,
    ) -> Result<Self> {
        let store = ArchiveCasStorage::new(cas_root)?;
        let manifest = store.read_manifest(manifest_path)?;
        let root = present_root(&store, &manifest)?;
        let signature = Signature::check(&store, &manifest.archive_id, root, trusted);
        Ok(Self::index(store, manifest, root, signature))
    }

    fn index(
        store: ArchiveCasStorage,
        manifest: ArchiveManifest,
        root: CasHash,
        signature: Signature,
    ) -> Self {
        let mut by_path = BTreeMap::new();
        let mut children: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, entry) in manifest.entries.iter().enumerate() {
            by_path.insert(entry.path().to_string(), i);
            children
                .entry(parent(entry.path()).to_string())
                .or_default()
                .push(i);
        }
        Self {
            store,
            manifest,
            root,
            signature,
            by_path,
            children,
        }
    }

    pub fn archive_id(&self) -> &str {
        &self.manifest.archive_id
    }
    pub fn root(&self) -> CasHash {
        self.root
    }
    pub fn signature(&self) -> &Signature {
        &self.signature
    }
    pub fn source_label(&self) -> &str {
        &self.manifest.source_label
    }
    pub fn created_at_unix_secs(&self) -> u64 {
        self.manifest.created_at_unix_secs
    }
    pub fn file_count(&self) -> usize {
        self.manifest.file_count()
    }

    fn normalise(path: &str) -> &str {
        path.trim_matches('/').trim_start_matches("./")
    }

    /// `dir` normalised, "" for the snapshot root.
    fn directory<'a>(&self, dir: &'a str) -> Result<&'a str> {
        let dir = Self::normalise(dir);
        let dir = if dir == "." { "" } else { dir };
        if !dir.is_empty()
            && !matches!(
                self.by_path.get(dir).map(|&i| &self.manifest.entries[i]),
                Some(ArchiveEntry::Directory { .. })
            )
        {
            bail!("{dir} is not a directory in this snapshot");
        }
        Ok(dir)
    }

    /// Children of directory `dir` ("" or "." is the snapshot root).
    ///
    /// # Errors
    /// When `dir` is not a directory in the snapshot.
    pub fn list(&self, dir: &str) -> Result<Vec<ViewChild>> {
        let dir = self.directory(dir)?;
        Ok(self
            .children
            .get(dir)
            .map(|list| {
                list.iter()
                    .map(|&i| {
                        let entry = &self.manifest.entries[i];
                        ViewChild {
                            name: entry.path().rsplit('/').next().unwrap_or("").to_string(),
                            kind: kind(entry),
                            object: match entry {
                                ArchiveEntry::File { object, .. } => Some(*object),
                                _ => None,
                            },
                        }
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Everything below directory `dir` ("" is the whole snapshot), counted. One pass
    /// over the manifest's entries.
    ///
    /// # Errors
    /// When `dir` is not a directory in the snapshot.
    pub fn tally(&self, dir: &str) -> Result<Tally> {
        let dir = self.directory(dir)?;
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut tally = Tally::default();
        let mut seen = HashSet::new();
        for entry in &self.manifest.entries {
            let Some(rest) = entry.path().strip_prefix(prefix.as_str()) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let child = rest.split_once('/').map(|(child, _)| child);
            match entry {
                ArchiveEntry::File { object, .. } => {
                    tally.files += 1;
                    tally.bytes += object.size;
                    if seen.insert(object.hash) {
                        tally.objects += 1;
                        tally.object_bytes += object.size;
                    }
                    if let Some(child) = child {
                        let below = tally.subdirs.entry(child.to_string()).or_default();
                        below.0 += 1;
                        below.1 += object.size;
                    }
                }
                ArchiveEntry::Directory { .. } => {
                    tally.dirs += 1;
                    if child.is_none() {
                        tally.subdirs.entry(rest.to_string()).or_default();
                    }
                }
                ArchiveEntry::Symlink { .. } => tally.links += 1,
                ArchiveEntry::Unreadable { .. } => tally.unreadable += 1,
                ArchiveEntry::Excluded { .. } => tally.excluded += 1,
            }
        }
        Ok(tally)
    }

    /// File paths in manifest order.
    pub fn files(&self) -> impl Iterator<Item = (&str, ArchiveObject)> {
        self.manifest.entries.iter().filter_map(|e| match e {
            ArchiveEntry::File { path, object, .. } => Some((path.as_str(), *object)),
            _ => None,
        })
    }

    /// The object a file path names.
    ///
    /// # Errors
    /// When `path` is not a file in the snapshot.
    pub fn object(&self, path: &str) -> Result<ArchiveObject> {
        match self
            .by_path
            .get(Self::normalise(path))
            .map(|&i| &self.manifest.entries[i])
        {
            Some(ArchiveEntry::File { object, .. }) => Ok(*object),
            Some(other) => bail!("{} is a {} in this snapshot, not a file", path, kind(other)),
            None => bail!("{path} is not in this snapshot"),
        }
    }

    /// A whole object's bytes, BLAKE3-verified against `object` before returning.
    ///
    /// # Errors
    /// When the object is missing, larger than [`MAX_OBJECT_BYTES`], or its bytes do not
    /// hash to the manifest's digest.
    pub fn read_object(&self, object: ArchiveObject) -> Result<Vec<u8>> {
        if object.size > MAX_OBJECT_BYTES {
            bail!(
                "object {} is {} bytes; larger than this view reads",
                object.hash,
                object.size
            );
        }
        let size = usize::try_from(object.size).context("object size exceeds memory")?;
        let bytes = self.store.read_range(object.hash, 0, size)?;
        let actual = CasHash::digest(&bytes);
        if bytes.len() != size || actual != object.hash {
            bail!(
                "object {} failed verification (read {} bytes hashing to {})",
                object.hash,
                bytes.len(),
                actual
            );
        }
        Ok(bytes)
    }

    /// The verified bytes of file `path`, with the object they came from.
    ///
    /// # Errors
    /// As [`Self::object`] and [`Self::read_object`].
    pub fn read_file(&self, path: &str) -> Result<(Vec<u8>, ArchiveObject)> {
        let object = self.object(path)?;
        Ok((self.read_object(object)?, object))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_cas_sign::sign_manifest_and_write;
    use loadngo_pq_crypto::{default_registry, PqSchemeRegistry, SignatureSchemeId};

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loadngo-archive-view-{}-{test}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A signed two-file snapshot plus the trusted and an untrusted public key path.
    fn signed_snapshot(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let cas = dir.join("cas");
        let store = ArchiveCasStorage::new(&cas).unwrap();
        let a = store
            .add_content(b"hello archive\nsecond line\n")
            .unwrap()
            .object;
        let b = store.add_content(&[0_u8, 1, 2, 3]).unwrap().object;
        let manifest = ArchiveManifest::new(
            "view-test",
            "unit test",
            1,
            vec![
                ArchiveEntry::Directory {
                    path: "src".into(),
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "src/a.txt".into(),
                    object: a,
                    modified_at_unix_secs: None,
                },
                ArchiveEntry::File {
                    path: "blob.bin".into(),
                    object: b,
                    modified_at_unix_secs: None,
                },
            ],
        )
        .unwrap();
        store.write_manifest(&manifest).unwrap();
        let registry = default_registry();
        let scheme = registry.get(&SignatureSchemeId::Dilithium2).unwrap();
        let mut paths = Vec::new();
        let mut signing = None;
        for name in ["trusted", "stranger"] {
            let (public, private) = scheme.keygen().unwrap();
            let path = dir.join(format!("{name}.pub"));
            fs::write(&path, hex::encode(public.to_bytes().unwrap())).unwrap();
            paths.push(path);
            signing.get_or_insert((public, private));
        }
        let (public, private) = signing.unwrap();
        sign_manifest_and_write(&store, &manifest, "test-signer", &public, &private, 7).unwrap();
        (cas, paths[0].clone(), paths[1].clone())
    }

    #[test]
    fn verified_snapshot_lists_and_reads_only_checked_bytes() {
        let dir = scratch("read");
        let (cas, trusted, _) = signed_snapshot(&dir);
        let view = ArchiveView::open_newest_verified(&cas, &trusted).unwrap();
        assert_eq!(view.archive_id(), "view-test");
        assert_eq!(view.signature().describe(), "signed by test-signer");
        let root: Vec<_> = view
            .list("")
            .unwrap()
            .into_iter()
            .map(|c| (c.name, c.kind))
            .collect();
        assert!(
            root.contains(&("src".into(), "dir")) && root.contains(&("blob.bin".into(), "file"))
        );
        assert_eq!(view.list("src").unwrap()[0].name, "a.txt");
        assert!(view.list("src/a.txt").is_err());
        let (bytes, _) = view.read_file("src/a.txt").unwrap();
        assert_eq!(bytes, b"hello archive\nsecond line\n");
        assert!(view.read_file("missing").is_err());
        assert!(view.read_file("src").is_err());

        // A tampered blob is refused, not returned.
        let object = view.object("src/a.txt").unwrap();
        let store = ArchiveCasStorage::new(&cas).unwrap();
        fs::write(
            store.object_path(object.hash),
            b"hello archivf\nsecond line\n",
        )
        .unwrap();
        assert!(view.read_file("src/a.txt").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tally_counts_everything_below_a_directory_and_equal_files_once() {
        let dir = scratch("tally");
        let cas = dir.join("cas");
        let store = ArchiveCasStorage::new(&cas).unwrap();
        let same = store.add_content(b"same bytes").unwrap().object;
        let other = store.add_content(b"other").unwrap().object;
        let entries = [("a", None), ("a/b", None), ("e", None)]
            .into_iter()
            .chain([
                ("a/x", Some(same)),
                ("a/b/y", Some(same)),
                ("z", Some(other)),
            ])
            .map(|(path, object)| match object {
                Some(object) => ArchiveEntry::File {
                    path: path.into(),
                    object,
                    modified_at_unix_secs: None,
                },
                None => ArchiveEntry::Directory {
                    path: path.into(),
                    modified_at_unix_secs: None,
                },
            })
            .collect();
        let manifest = ArchiveManifest::new("tally", "test", 1, entries).unwrap();
        let (path, _) = store.write_manifest(&manifest).unwrap();
        let view = ArchiveView::open(&cas, &path, None).unwrap();

        let all = view.tally("").unwrap();
        assert_eq!((all.files, all.bytes), (3, 25));
        assert_eq!((all.objects, all.object_bytes), (2, 15));
        assert_eq!(all.dirs, 3);
        assert_eq!(all.subdirs.get("a"), Some(&(2, 20)));
        assert_eq!(all.subdirs.get("e"), Some(&(0, 0)));
        assert_eq!(all.subdirs.len(), 2, "{:?}", all.subdirs);

        let a = view.tally("a/").unwrap();
        assert_eq!((a.files, a.objects, a.dirs), (2, 1, 1));
        assert_eq!(a.subdirs.get("b"), Some(&(1, 10)));
        assert!(view.tally("z").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn untrusted_key_and_unsigned_roots_are_refused() {
        let dir = scratch("trust");
        let (cas, _, stranger) = signed_snapshot(&dir);
        assert!(ArchiveView::open_newest_verified(&cas, &stranger).is_err());
        let empty = dir.join("empty");
        ArchiveCasStorage::new(&empty).unwrap();
        assert!(ArchiveView::open_newest_verified(&empty, &stranger).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn every_archive_is_listed_with_its_signature_and_unsigned_ones_open_too() {
        let dir = scratch("listing");
        let (cas, trusted, stranger) = signed_snapshot(&dir);
        let store = ArchiveCasStorage::new(&cas).unwrap();
        // A second, unsigned archive, then a newer version of it that supersedes it.
        let note = store.add_content(b"PrivateKey = abc\n").unwrap().object;
        let docs = ArchiveManifest::new(
            "docs",
            "Documents",
            5,
            vec![ArchiveEntry::File {
                path: "wg.conf".into(),
                object: note,
                modified_at_unix_secs: None,
            }],
        )
        .unwrap();
        let (old_path, _) = store.write_manifest(&docs).unwrap();
        let newer = docs
            .with_entries_removed(&["wg.conf".to_string()], "test", "jay", 6)
            .unwrap();
        let (new_path, _) = store.write_manifest(&newer).unwrap();

        let key = read_public_key(&trusted).unwrap();
        let listings = list_archives(&cas, Some(&key)).unwrap();
        let summary: Vec<(&str, bool, String)> = listings
            .iter()
            .map(|l| (l.archive_id.as_str(), l.superseded, l.signature.describe()))
            .collect();
        assert_eq!(
            summary,
            [
                ("docs", true, "unsigned".to_string()),
                ("docs", false, "unsigned".to_string()),
                ("view-test", false, "signed by test-signer".to_string()),
            ]
        );
        assert_eq!(listings[0].manifest_path, old_path);
        assert_eq!(listings[0].source_label, "Documents");
        assert_eq!(listings[1].created_at_unix_secs, 6);
        assert_eq!(listings[1].parents, vec![docs.digest().unwrap()]);
        assert_eq!(listings[1].records, newer.records);

        // A stranger's key does not verify the signed archive.
        let other = read_public_key(&stranger).unwrap();
        let listed = list_archives(&cas, Some(&other)).unwrap();
        assert!(matches!(listed[2].signature, Signature::NotVerified(_)));

        // Unsigned archives open and read verified bytes; the signed one keeps its signer.
        let view = ArchiveView::open(&cas, &old_path, Some(&key)).unwrap();
        assert_eq!(view.signature(), &Signature::Unsigned);
        assert_eq!(view.read_file("wg.conf").unwrap().0, b"PrivateKey = abc\n");
        let view = ArchiveView::open(&cas, &new_path, Some(&key)).unwrap();
        assert!(view.read_file("wg.conf").is_err());
        let view = ArchiveView::open(&cas, &listings[2].manifest_path, Some(&key)).unwrap();
        assert_eq!(view.signature().describe(), "signed by test-signer");
        assert_eq!(view.root(), listings[2].root);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_header_with_long_change_records_is_read_past_its_first_chunk() {
        let dir = scratch("long-header");
        let store = ArchiveCasStorage::new(&dir).unwrap();
        // 5,000 removed paths of 40 bytes: a header of about 220 KB.
        let entries: Vec<ArchiveEntry> = (0..5_001)
            .map(|i| ArchiveEntry::Directory {
                path: format!("a-directory-with-a-fairly-long-name-{i:05}"),
                modified_at_unix_secs: None,
            })
            .collect();
        let base = ArchiveManifest::new("long", "Long", 1, entries).unwrap();
        let named: Vec<String> = (0..5_000)
            .map(|i| format!("a-directory-with-a-fairly-long-name-{i:05}"))
            .collect();
        let next = base.with_entries_removed(&named, "tidy", "jay", 2).unwrap();
        let (path, _) = store.write_manifest(&next).unwrap();
        assert!(fs::metadata(&path).unwrap().len() > 200_000);
        let listings = list_archives(&dir, None).unwrap();
        let listed = listings.iter().find(|l| l.manifest_path == path).unwrap();
        assert_eq!(listed.records, next.records);
        assert_eq!(listed.parents, vec![base.digest().unwrap()]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
