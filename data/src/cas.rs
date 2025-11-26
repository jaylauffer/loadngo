//! Content-addressed storage used by the data and network layers.
//!
//! ```rust
//! use data::cas::CasStorage;
//! use std::fs;
//! use std::time::{SystemTime, UNIX_EPOCH};
//!
//! let unique = SystemTime::now()
//!     .duration_since(UNIX_EPOCH)
//!     .unwrap()
//!     .as_nanos();
//! let root = std::env::temp_dir().join(format!("loadngo_cas_doctest_{unique}"));
//!
//! let store = CasStorage::new(&root).unwrap();
//! let payload = b"shared voice clip bytes";
//!
//! let (hash, inserted) = store.add_content(payload).unwrap();
//! assert!(inserted);
//! assert_eq!(store.verified_read_all(hash).unwrap(), payload);
//!
//! let (same_hash, inserted_again) = store.add_content(payload).unwrap();
//! assert_eq!(same_hash, hash);
//! assert!(!inserted_again);
//!
//! fs::remove_dir_all(&root).unwrap();
//! ```

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CasHash([u8; 32]);

impl CasHash {
    pub const LEN: usize = 32;

    pub fn digest(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::LEN {
            bail!(
                "invalid CAS hash length: expected {} bytes, got {}",
                Self::LEN,
                bytes.len()
            );
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(bytes);
        Ok(Self(digest))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Display for CasHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for CasHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.len() != Self::LEN * 2 {
            bail!(
                "invalid CAS hash length: expected {} hex chars, got {}",
                Self::LEN * 2,
                s.len()
            );
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let text = std::str::from_utf8(chunk)?;
            bytes[i] = u8::from_str_radix(text, 16)
                .with_context(|| format!("invalid CAS hash byte '{text}'"))?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for CasHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for CasHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CasFlags(u16);

impl CasFlags {
    pub const NORMAL: Self = Self(0);
    pub const DELETED: Self = Self(1);
    pub const CORRUPT: Self = Self(2);
    pub const INCOMPLETE: Self = Self(4);

    pub fn bits(self) -> u16 {
        self.0
    }

    pub fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    pub fn only_deleted(self) -> Self {
        if self.contains(Self::DELETED) {
            Self::DELETED
        } else {
            Self::NORMAL
        }
    }
}

impl BitOr for CasFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for CasFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for CasFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for CasFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CasEntry {
    pub hash: CasHash,
    pub flags: CasFlags,
    pub size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedEntry {
    flags: CasFlags,
    size: u32,
}

#[derive(Debug)]
pub struct CasStorage {
    root: PathBuf,
    objects: PathBuf,
    index_path: PathBuf,
    lock: Mutex<()>,
}

impl CasStorage {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let objects = root.join("objects");
        fs::create_dir_all(&objects)?;
        Ok(Self {
            index_path: root.join("index.json"),
            root,
            objects,
            lock: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn verify_and_add(&self, expected: CasHash, bytes: &[u8]) -> Result<bool> {
        let actual = CasHash::digest(bytes);
        if actual != expected {
            bail!("CAS hash mismatch: expected {expected}, got {actual}");
        }
        let (_, inserted) = self.add_content(bytes)?;
        Ok(inserted)
    }

    pub fn add_content(&self, bytes: &[u8]) -> Result<(CasHash, bool)> {
        let hash = CasHash::digest(bytes);
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let mut index = self.load_index()?;
        let key = hash.to_hex();
        if index.contains_key(&key) {
            return Ok((hash, false));
        }

        let object_path = self.object_path(hash);
        self.ensure_object_parent(&object_path)?;
        fs::write(&object_path, bytes)?;
        index.insert(
            key,
            PersistedEntry {
                flags: CasFlags::NORMAL,
                size: u32::try_from(bytes.len()).context("content too large for CAS entry")?,
            },
        );
        self.save_index(&index)?;
        Ok((hash, true))
    }

    pub fn add_file(&self, path: impl AsRef<Path>) -> Result<(CasHash, u32, bool)> {
        let bytes = fs::read(path.as_ref())?;
        let size = u32::try_from(bytes.len()).context("file too large for CAS entry")?;
        let (hash, inserted) = self.add_content(&bytes)?;
        Ok((hash, size, inserted))
    }

    pub fn add_empty(&self, hash: CasHash, size: u32) -> Result<bool> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let mut index = self.load_index()?;
        let key = hash.to_hex();
        if index.contains_key(&key) {
            return Ok(false);
        }

        let object_path = self.object_path(hash);
        self.ensure_object_parent(&object_path)?;
        let file = File::create(&object_path)?;
        file.set_len(size as u64)?;
        index.insert(
            key,
            PersistedEntry {
                flags: CasFlags::INCOMPLETE,
                size,
            },
        );
        self.save_index(&index)?;
        Ok(true)
    }

    pub fn write_incomplete(&self, hash: CasHash, offset: u32, bytes: &[u8]) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let index = self.load_index()?;
        let entry = index
            .get(&hash.to_hex())
            .ok_or_else(|| anyhow!("CAS entry {hash} not found"))?;
        if !entry.flags.contains(CasFlags::INCOMPLETE) {
            bail!("CAS entry {hash} is not incomplete");
        }
        let end = u64::from(offset) + bytes.len() as u64;
        if end > u64::from(entry.size) {
            bail!("write past end of incomplete CAS entry {hash}");
        }

        let mut file = OpenOptions::new()
            .write(true)
            .open(self.object_path(hash))
            .with_context(|| format!("failed to open incomplete CAS entry {hash} for writing"))?;
        file.seek(SeekFrom::Start(offset as u64))?;
        file.write_all(bytes)?;
        file.flush()?;
        Ok(())
    }

    pub fn verify(&self, hash: CasHash) -> Result<bool> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let mut index = self.load_index()?;
        let entry = index
            .get_mut(&hash.to_hex())
            .ok_or_else(|| anyhow!("CAS entry {hash} not found"))?;
        let actual = self.compute_file_hash(self.object_path(hash))?;
        if actual == hash {
            entry.flags = entry.flags.only_deleted();
            self.save_index(&index)?;
            return Ok(true);
        }
        entry.flags |= CasFlags::CORRUPT;
        self.save_index(&index)?;
        Ok(false)
    }

    pub fn read(&self, hash: CasHash, offset: u32, size: u32) -> Result<Vec<u8>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let index = self.load_index()?;
        let entry = index
            .get(&hash.to_hex())
            .ok_or_else(|| anyhow!("CAS entry {hash} not found"))?;

        if offset > entry.size {
            bail!("offset {offset} past end of CAS entry {hash}");
        }

        let available = entry.size - offset;
        let amount = available.min(size);
        let mut file = File::open(self.object_path(hash))?;
        file.seek(SeekFrom::Start(offset as u64))?;
        let mut buf = vec![0u8; amount as usize];
        file.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub fn read_all(&self, hash: CasHash) -> Result<Vec<u8>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let index = self.load_index()?;
        let entry = index
            .get(&hash.to_hex())
            .ok_or_else(|| anyhow!("CAS entry {hash} not found"))?;
        let mut buf = vec![0u8; entry.size as usize];
        if entry.size == 0 {
            return Ok(buf);
        }
        let mut file = File::open(self.object_path(hash))?;
        file.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub fn verified_read_all(&self, hash: CasHash) -> Result<Vec<u8>> {
        let bytes = self.read_all(hash)?;
        let actual = CasHash::digest(&bytes);
        if actual != hash {
            bail!("CAS verification failed for {hash}: read hash {actual}");
        }
        Ok(bytes)
    }

    pub fn get_size(&self, hash: CasHash) -> Result<Option<u32>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let index = self.load_index()?;
        Ok(index.get(&hash.to_hex()).map(|entry| entry.size))
    }

    pub fn exists(&self, hash: CasHash) -> Result<bool> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let index = self.load_index()?;
        Ok(index
            .get(&hash.to_hex())
            .map(|entry| entry.flags == CasFlags::NORMAL)
            .unwrap_or(false))
    }

    pub fn flags(&self, hash: CasHash) -> Result<Option<CasFlags>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let index = self.load_index()?;
        Ok(index.get(&hash.to_hex()).map(|entry| entry.flags))
    }

    pub fn remove(&self, hash: CasHash) -> Result<bool> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let mut index = self.load_index()?;
        if let Some(entry) = index.get_mut(&hash.to_hex()) {
            entry.flags |= CasFlags::DELETED;
            self.save_index(&index)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn list_all_content(&self) -> Result<BTreeSet<CasHash>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| anyhow!("CAS storage lock poisoned"))?;
        let index = self.load_index()?;
        index
            .keys()
            .map(|key| CasHash::from_str(key))
            .collect::<Result<BTreeSet<_>>>()
    }

    fn object_path(&self, hash: CasHash) -> PathBuf {
        let hex = hash.to_hex();
        self.objects.join(&hex[0..2]).join(format!("{hex}.bin"))
    }

    fn ensure_object_parent(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    fn load_index(&self) -> Result<BTreeMap<String, PersistedEntry>> {
        if !self.index_path.exists() {
            return Ok(BTreeMap::new());
        }
        let text = fs::read_to_string(&self.index_path)?;
        if text.trim().is_empty() {
            return Ok(BTreeMap::new());
        }
        Ok(serde_json::from_str(&text)?)
    }

    fn save_index(&self, index: &BTreeMap<String, PersistedEntry>) -> Result<()> {
        let tmp = self.index_path.with_extension("json.tmp");
        let payload = serde_json::to_string_pretty(index)?;
        fs::write(&tmp, payload)?;
        if self.index_path.exists() {
            fs::remove_file(&self.index_path)?;
        }
        fs::rename(tmp, &self.index_path)?;
        Ok(())
    }

    fn compute_file_hash(&self, path: PathBuf) -> Result<CasHash> {
        let mut file = File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(CasHash::digest(&buf))
    }
}
