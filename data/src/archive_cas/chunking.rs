//! Versioned content-defined chunking metadata for Archive CAS files.
//!
//! This is a Loadngo profile inspired by Xet's CDC approach; it does not claim
//! Xet boundary or wire compatibility. The profile constants and manifest
//! format are part of the persisted representation and must not be changed in
//! place.

use crate::archive_cas::ArchiveObject;
use crate::cas::CasHash;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const PROFILE: &str = "loadngo-buzhash64-cdc-v2";
pub const LEGACY_PROFILE_V1: &str = "loadngo-buzhash64-cdc-v1";
pub const MANIFEST_FORMAT: &str = "loadngo-repository-chunk-manifest-v1";
pub const WINDOW_BYTES: usize = 64;
pub const MIN_CHUNK_BYTES: usize = 8 * 1024;
pub const TARGET_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_CHUNK_BYTES: usize = 128 * 1024;

/// Persisted ordered references to immutable CAS chunk objects.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileManifest {
    pub format: String,
    pub profile: String,
    pub file_size: u64,
    pub file_hash: CasHash,
    pub chunks: Vec<ChunkRef>,
}

/// A chunk reference. `inserted` is local ingest telemetry and is never serialized.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChunkRef {
    pub object: ArchiveObject,
    #[serde(skip)]
    pub inserted: bool,
}

impl FileManifest {
    /// Encodes a validated manifest as canonical JSON in field declaration order.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }

    /// Decodes canonical JSON while enforcing caller-selected byte and chunk limits.
    pub fn from_canonical_json(bytes: &[u8], max_bytes: usize, max_chunks: usize) -> Result<Self> {
        if bytes.len() > max_bytes {
            bail!(
                "chunk manifest is {} bytes, exceeding the {max_bytes}-byte limit",
                bytes.len()
            );
        }
        let manifest: Self = serde_json::from_slice(bytes)?;
        manifest.validate()?;
        if manifest.chunks.len() > max_chunks {
            bail!(
                "chunk manifest has {} chunks, exceeding the {max_chunks}-chunk limit",
                manifest.chunks.len()
            );
        }
        if manifest.to_canonical_json()?.as_slice() != bytes {
            bail!("chunk manifest is not in canonical JSON form");
        }
        Ok(manifest)
    }

    /// Checks the profile identifier, chunk lengths, and total file length.
    pub fn validate(&self) -> Result<()> {
        if self.format != MANIFEST_FORMAT
            || (self.profile != PROFILE && self.profile != LEGACY_PROFILE_V1)
        {
            bail!("unsupported chunk manifest format or profile");
        }
        let mut total = 0_u64;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let size = chunk.object.size;
            if size == 0 || size > MAX_CHUNK_BYTES as u64 {
                bail!("chunk {index} has invalid size {size}");
            }
            if index + 1 != self.chunks.len() && size < MIN_CHUNK_BYTES as u64 {
                bail!("non-final chunk {index} is smaller than {MIN_CHUNK_BYTES} bytes");
            }
            total = total
                .checked_add(size)
                .ok_or_else(|| anyhow::anyhow!("manifest size exceeds u64"))?;
        }
        if total != self.file_size {
            bail!(
                "manifest size is {}, chunk lengths total {total}",
                self.file_size
            );
        }
        if self.file_size == 0 && !self.chunks.is_empty() {
            bail!("empty files must have no chunks");
        }
        if self.file_size != 0 && self.chunks.is_empty() {
            bail!("non-empty files must have at least one chunk");
        }
        Ok(())
    }
}

/// Stateful splitter for the corrected v2 CDC profile.
pub struct ContentDefinedChunker {
    rolling: Buzhash64,
    current: Vec<u8>,
}

impl Default for ContentDefinedChunker {
    fn default() -> Self {
        Self {
            rolling: Buzhash64::default(),
            current: Vec::with_capacity(MAX_CHUNK_BYTES),
        }
    }
}

impl ContentDefinedChunker {
    /// Adds one byte. Returns an owned chunk when the profile reaches a boundary.
    pub fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        self.current.push(byte);
        let fingerprint = self.rolling.push(byte);
        let reached_target = self.current.len() >= MIN_CHUNK_BYTES
            && fingerprint & (TARGET_CHUNK_BYTES as u64 - 1) == 0;
        if reached_target || self.current.len() == MAX_CHUNK_BYTES {
            Some(std::mem::replace(
                &mut self.current,
                Vec::with_capacity(MAX_CHUNK_BYTES),
            ))
        } else {
            None
        }
    }

    /// Returns the final short chunk, or `None` when the stream ended at a boundary.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        if self.current.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.current))
        }
    }
}

/// Deterministic 64-byte rolling Buzhash state for the v2 profile.
#[derive(Clone)]
struct Buzhash64 {
    window: [u8; WINDOW_BYTES],
    cursor: usize,
    filled: usize,
    fingerprint: u64,
}

impl Default for Buzhash64 {
    fn default() -> Self {
        Self {
            window: [0; WINDOW_BYTES],
            cursor: 0,
            filled: 0,
            fingerprint: 0,
        }
    }
}

impl Buzhash64 {
    /// Adds a byte and returns the current rolling fingerprint.
    pub fn push(&mut self, byte: u8) -> u64 {
        let table = buzhash_table();
        let outgoing = if self.filled == self.window.len() {
            table[usize::from(self.window[self.cursor])].rotate_left(WINDOW_BYTES as u32)
        } else {
            0
        };
        self.fingerprint = self.fingerprint.rotate_left(1) ^ table[usize::from(byte)] ^ outgoing;
        self.window[self.cursor] = byte;
        self.cursor = (self.cursor + 1) % self.window.len();
        self.filled = (self.filled + 1).min(self.window.len());
        self.fingerprint
    }
}

fn table_value(byte: u8) -> u64 {
    let mut value = u64::from(byte).wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn buzhash_table() -> &'static [u64; 256] {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|index| table_value(index as u8)))
}

#[cfg(test)]
mod tests {
    use super::{
        buzhash_table, Buzhash64, ContentDefinedChunker, FileManifest, LEGACY_PROFILE_V1,
        MANIFEST_FORMAT, PROFILE, WINDOW_BYTES,
    };
    use crate::cas::CasHash;

    #[test]
    fn rolling_fingerprint_matches_recomputed_active_window() {
        let mut rolling = Buzhash64::default();
        for index in 0..(WINDOW_BYTES * 12) {
            let byte = (index.wrapping_mul(73) ^ (index >> 2) ^ index.wrapping_mul(index)) as u8;
            rolling.push(byte);
            if rolling.filled == WINDOW_BYTES {
                let table = buzhash_table();
                let expected = (0..WINDOW_BYTES).fold(0_u64, |fingerprint, position| {
                    let byte = rolling.window[(rolling.cursor + position) % WINDOW_BYTES];
                    fingerprint.rotate_left(1) ^ table[usize::from(byte)]
                });
                assert_eq!(rolling.fingerprint, expected, "window ending at {index}");
            }
        }
    }

    #[test]
    fn one_byte_prefix_insertion_resynchronizes_later_chunks() {
        fn split(bytes: &[u8]) -> Vec<Vec<u8>> {
            let mut chunker = ContentDefinedChunker::default();
            let mut chunks = Vec::new();
            for byte in bytes {
                if let Some(chunk) = chunker.push(*byte) {
                    chunks.push(chunk);
                }
            }
            if let Some(chunk) = chunker.finish() {
                chunks.push(chunk);
            }
            chunks
        }

        let mut state = 0x8a5c_31d7_u32;
        const INPUT_BYTES: usize = 4 * 1024 * 1024;
        let mut original = Vec::with_capacity(INPUT_BYTES);
        for _ in 0..INPUT_BYTES {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            original.push((state >> 24) as u8);
        }
        let mut edited = Vec::with_capacity(original.len() + 1);
        edited.push(0xA5);
        edited.extend_from_slice(&original);

        let original_chunks = split(&original);
        let edited_chunks = split(&edited);
        let matching_suffix = original_chunks
            .iter()
            .rev()
            .zip(edited_chunks.iter().rev())
            .take_while(|(left, right)| left == right)
            .count();
        assert!(
            matching_suffix >= 4,
            "expected later chunks to resynchronize, got {matching_suffix}"
        );
    }

    #[test]
    fn legacy_v1_manifests_remain_valid_and_new_profile_is_v2() {
        let legacy = FileManifest {
            format: MANIFEST_FORMAT.to_owned(),
            profile: LEGACY_PROFILE_V1.to_owned(),
            file_size: 0,
            file_hash: CasHash::digest(&[]),
            chunks: Vec::new(),
        };
        assert!(legacy.validate().is_ok());
        assert_eq!(PROFILE, "loadngo-buzhash64-cdc-v2");
    }
}
