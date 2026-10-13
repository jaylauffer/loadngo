//! Models named by content: an Archive CAS object (BLAKE3 hash and size), never a path.
//!
//! A model is restored once from a CAS root into `~/.loadngo/models/<hash>.gguf`, checked
//! against its hash while copying and made read-only. The copy is hashed again before
//! every use, so a file swapped into the cache is refused, not loaded. Once cached, the
//! CAS drive need not be attached. `gpt_oss_serve` loads its model this way
//! (`docs/GPT_OSS.md`, "Serving").

use crate::archive_cas::{ArchiveCasStorage, ArchiveObject};
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// gpt-oss-20b (ggml-org's `MXFP4_MOE` GGUF), as archived in the signed pudding CAS
/// (`pudding-20260917` root `4d8babf2`, `loadngo/models/gpt-oss-20b-MXFP4_MOE.gguf`).
pub const GPT_OSS_20B_HASH: &str =
    "56fcc05caeabd1f4f352f7b9d6762cad2035b860973c07a5c330a7fa3944e8e1";
pub const GPT_OSS_20B_BYTES: u64 = 12_109_565_760;

/// gpt-oss-20b, by content.
#[must_use]
pub fn gpt_oss_20b() -> ArchiveObject {
    ArchiveObject {
        hash: GPT_OSS_20B_HASH
            .parse()
            .expect("GPT_OSS_20B_HASH is valid hex"),
        size: GPT_OSS_20B_BYTES,
    }
}

/// Where verified model copies live: `~/.loadngo/models`, one file per model, named by
/// its BLAKE3 hash.
#[must_use]
pub fn default_cache() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".loadngo")
        .join("models")
}

/// One model and where to keep and find it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStore {
    pub model: ArchiveObject,
    /// The Archive CAS root to restore the model from; `None` scans attached drives
    /// (`crate::cli::discover`) for a root that holds it.
    pub cas_root: Option<PathBuf>,
    pub cache: PathBuf,
}

/// Where the model would come from, found without copying or hashing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelLocation {
    /// A copy is in the cache; [`ModelStore::resolve`] hashes it before use.
    Cached(PathBuf),
    /// Not cached yet: [`ModelStore::resolve`] restores it from this CAS root.
    InCas(PathBuf),
}

impl ModelStore {
    /// `model` in the default cache, restored from any attached CAS root that holds it.
    #[must_use]
    pub fn new(model: ArchiveObject) -> Self {
        Self {
            model,
            cas_root: None,
            cache: default_cache(),
        }
    }

    /// The verified copy's path (it may not exist yet).
    #[must_use]
    pub fn cached_path(&self) -> PathBuf {
        self.cache
            .join(format!("{}.gguf", self.model.hash.to_hex()))
    }

    /// Where the model would come from.
    ///
    /// # Errors
    /// When it is not cached and no CAS root (the given one, or an attached one) holds it.
    pub fn locate(&self) -> Result<ModelLocation> {
        let cached = self.cached_path();
        if cached.exists() {
            return Ok(ModelLocation::Cached(cached));
        }
        let hash = self.model.hash;
        let holds = |root: &Path| {
            crate::cli::discover::is_archive_cas_root(root)
                && ArchiveCasStorage::new(root).is_ok_and(|store| store.has_object(hash))
        };
        if let Some(root) = &self.cas_root {
            if !crate::cli::discover::is_archive_cas_root(root) {
                bail!(
                    "not an Archive CAS root (no objects/ and manifests/): {}",
                    root.display()
                );
            }
            if !holds(root) {
                bail!(
                    "model {hash} is not in the Archive CAS at {}",
                    root.display()
                );
            }
            return Ok(ModelLocation::InCas(root.clone()));
        }
        crate::cli::discover::scan_for_cas_roots()
            .into_iter()
            .find(|root| holds(root))
            .map(ModelLocation::InCas)
            .ok_or_else(|| {
                anyhow!(
                    "model {hash} is neither cached at {} nor in any attached Archive CAS; \
                     attach the drive that holds it or pass --cas-root",
                    cached.display()
                )
            })
    }

    /// The verified model file: restored from the CAS on first use (checked while
    /// copying, then made read-only), and hashed again on every call. A cached copy that
    /// does not match the model's hash and size is refused and left for inspection.
    ///
    /// # Errors
    /// When the model cannot be found, restored or verified.
    pub fn resolve(&self) -> Result<PathBuf> {
        let cached = self.cached_path();
        if let ModelLocation::InCas(root) = self.locate()? {
            eprintln!(
                "model_store: restoring {} ({} bytes) from {} to {}",
                self.model.hash,
                self.model.size,
                root.display(),
                cached.display()
            );
            fs::create_dir_all(&self.cache)
                .with_context(|| format!("failed to create {}", self.cache.display()))?;
            ArchiveCasStorage::new(&root)?.restore_object_to_path(self.model, &cached)?;
            make_read_only(&cached)?;
            return Ok(cached);
        }
        verify_file(&cached, self.model)?;
        Ok(cached)
    }
}

/// Hashes `path` (BLAKE3) and checks it is exactly `expected`.
///
/// # Errors
/// When the file cannot be read, or its size or hash differ.
pub fn verify_file(path: &Path, expected: ArchiveObject) -> Result<()> {
    let size = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();
    if size != expected.size {
        bail!(
            "{} is {size} bytes, not the model's {}; refusing to load it",
            path.display(),
            expected.size
        );
    }
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 4 << 20];
    loop {
        let count = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()))
            }
        };
        hasher.update(&buffer[..count]);
    }
    let actual = CasHash::from_bytes(*hasher.finalize().as_bytes());
    if actual != expected.hash {
        bail!(
            "{} hashes to {actual}, not the model's {}; refusing to load it",
            path.display(),
            expected.hash
        );
    }
    Ok(())
}

fn make_read_only(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("failed to make {} read-only", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{gpt_oss_20b, ModelLocation, ModelStore, GPT_OSS_20B_BYTES};
    use crate::archive_cas::ArchiveCasStorage;
    use std::{fs, path::PathBuf};

    #[test]
    fn model_store_names_gpt_oss_by_content_with_no_path() {
        let store = ModelStore::new(gpt_oss_20b());
        assert_eq!(store.model.size, GPT_OSS_20B_BYTES);
        assert!(store
            .cached_path()
            .ends_with(format!(".loadngo/models/{}.gguf", store.model.hash)));
    }

    /// A CAS root holding one model, and a store that restores it into its own cache.
    fn cas_with_model(bytes: &[u8]) -> (tempfile::TempDir, ModelStore) {
        let dir = tempfile::tempdir().unwrap();
        let cas = ArchiveCasStorage::new(dir.path().join("cas")).unwrap();
        fs::create_dir_all(dir.path().join("cas/manifests")).unwrap();
        let object = cas.add_content(bytes).unwrap().object;
        let store = ModelStore {
            model: object,
            cas_root: Some(dir.path().join("cas")),
            cache: dir.path().join("models"),
        };
        (dir, store)
    }

    #[test]
    fn model_store_restores_from_the_cas_once_then_serves_the_verified_cache() {
        let (_dir, store) = cas_with_model(b"GGUF model bytes");
        assert!(matches!(store.locate().unwrap(), ModelLocation::InCas(_)));
        let path = store.resolve().unwrap();
        assert_eq!(path, store.cached_path());
        assert_eq!(fs::read(&path).unwrap(), b"GGUF model bytes");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        // Later launches need no CAS: the copy is hashed instead.
        let offline = ModelStore {
            cas_root: Some(PathBuf::from("/nonexistent-cas")),
            ..store.clone()
        };
        assert_eq!(
            offline.locate().unwrap(),
            ModelLocation::Cached(path.clone())
        );
        assert_eq!(offline.resolve().unwrap(), path);
    }

    #[test]
    fn model_store_refuses_a_swapped_cached_copy_and_leaves_it_in_place() {
        let (_dir, store) = cas_with_model(b"GGUF model bytes");
        let path = store.resolve().unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        // Same size, different bytes.
        fs::write(&path, b"GGUF evil  bytes").unwrap();
        let error = store.resolve().unwrap_err().to_string();
        assert!(error.contains("refusing to load it"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"GGUF evil  bytes");
        fs::write(&path, b"short").unwrap();
        let error = store.resolve().unwrap_err().to_string();
        assert!(error.contains("refusing to load it"), "{error}");
    }

    #[test]
    fn model_store_refuses_a_root_without_the_model_or_not_a_cas() {
        let (dir, store) = cas_with_model(b"GGUF model bytes");
        let missing = ModelStore {
            model: gpt_oss_20b(),
            ..store.clone()
        };
        let error = missing.locate().unwrap_err().to_string();
        assert!(error.contains("is not in the Archive CAS"), "{error}");
        let not_cas = ModelStore {
            cas_root: Some(dir.path().join("models-not-a-cas")),
            ..store
        };
        let error = not_cas.locate().unwrap_err().to_string();
        assert!(error.contains("not an Archive CAS root"), "{error}");
    }
}
