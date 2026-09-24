//! A read-only, signature-verified view of one Archive CAS snapshot, for callers that
//! should see archived files but never the store's write paths (a local model's tools).
//!
//! A snapshot is trusted only when all three hold: its signature file verifies against
//! a trusted public key ([`archive_cas_sign::verify_signature`]), the manifest it names
//! is stored as an intact CAS object ([`archive_cas_sign::present_root`]), and that
//! object's digest equals the signed root. Every file read is BLAKE3-checked against
//! the manifest before any byte is returned.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::archive_cas::{ArchiveCasStorage, ArchiveEntry, ArchiveManifest, ArchiveObject};
use crate::archive_cas_sign::{present_root, read_public_key, verify_signature, SignedArchiveRoot};
use crate::cas::CasHash;

/// Largest single object [`ArchiveView::read_file`] loads.
pub const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// One child in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewChild {
    pub name: String,
    pub kind: &'static str,
    pub object: Option<ArchiveObject>,
}

pub struct ArchiveView {
    store: ArchiveCasStorage,
    manifest: ArchiveManifest,
    root: CasHash,
    signer: String,
    signed_at_unix_secs: u64,
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
            signed.signer_identity,
            signed_at_unix_secs,
        ))
    }

    fn index(
        store: ArchiveCasStorage,
        manifest: ArchiveManifest,
        root: CasHash,
        signer: String,
        signed_at_unix_secs: u64,
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
            signer,
            signed_at_unix_secs,
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
    pub fn signer(&self) -> &str {
        &self.signer
    }
    pub fn signed_at_unix_secs(&self) -> u64 {
        self.signed_at_unix_secs
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

    /// Children of directory `dir` ("" or "." is the snapshot root).
    ///
    /// # Errors
    /// When `dir` is not a directory in the snapshot.
    pub fn list(&self, dir: &str) -> Result<Vec<ViewChild>> {
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
        assert_eq!(view.signer(), "test-signer");
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
    fn untrusted_key_and_unsigned_roots_are_refused() {
        let dir = scratch("trust");
        let (cas, _, stranger) = signed_snapshot(&dir);
        assert!(ArchiveView::open_newest_verified(&cas, &stranger).is_err());
        let empty = dir.join("empty");
        ArchiveCasStorage::new(&empty).unwrap();
        assert!(ArchiveView::open_newest_verified(&empty, &stranger).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
