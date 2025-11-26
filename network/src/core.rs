use anyhow::{anyhow, bail, Context, Result};
use data::{
    cas::{CasFlags, CasHash, CasStorage},
    Participant,
};
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlobKey {
    pub source: SocketAddr,
    pub time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobFinish {
    Complete(Vec<u8>),
    Stored(CasHash),
    Missing(Vec<u32>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentFinish {
    Stored(CasHash),
    Missing(Vec<u32>),
    Corrupt(CasHash),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentEnd {
    Stored(CasHash),
    Incomplete(Vec<u32>),
    Corrupt(CasHash),
}

#[derive(Debug)]
struct BlobAssembly {
    total_len: usize,
    buffer: Vec<u8>,
    written: usize,
    received: BTreeSet<u32>,
}

impl BlobAssembly {
    fn new(total_len: usize) -> Self {
        Self {
            total_len,
            buffer: vec![0u8; total_len],
            written: 0,
            received: BTreeSet::new(),
        }
    }

    fn insert(&mut self, chunk_size: usize, seq: u32, data: &[u8]) -> Result<()> {
        let offset = chunk_offset(self.total_len, chunk_size, seq, data.len())?;
        let end = offset + data.len();
        if self.received.contains(&seq) {
            if self.buffer[offset..end] != *data {
                bail!("duplicate blob chunk seq {seq} does not match existing bytes");
            }
            return Ok(());
        }

        self.buffer[offset..end].copy_from_slice(data);
        self.received.insert(seq);
        self.written += data.len();
        Ok(())
    }

    fn finish(self, chunk_size: usize) -> BlobFinish {
        let total_chunks = self.total_len.div_ceil(chunk_size);
        let missing = (0..total_chunks as u32)
            .filter(|seq| !self.received.contains(seq))
            .collect::<Vec<_>>();
        if missing.is_empty() && self.written == self.total_len {
            BlobFinish::Complete(self.buffer)
        } else {
            BlobFinish::Missing(missing)
        }
    }
}

#[derive(Debug)]
struct ContentAssembly {
    hash: CasHash,
    total_len: usize,
    written: usize,
    received: BTreeSet<u32>,
}

impl ContentAssembly {
    fn new(hash: CasHash, total_len: usize) -> Self {
        Self {
            hash,
            total_len,
            written: 0,
            received: BTreeSet::new(),
        }
    }

    fn chunk_offset(&self, chunk_size: usize, seq: u32, data_len: usize) -> Result<usize> {
        chunk_offset(self.total_len, chunk_size, seq, data_len)
    }

    fn has_sequence(&self, seq: u32) -> bool {
        self.received.contains(&seq)
    }

    fn mark_received(&mut self, seq: u32, data_len: usize) {
        if self.received.insert(seq) {
            self.written += data_len;
        }
    }

    fn finish(self, chunk_size: usize) -> ContentFinish {
        let total_chunks = self.total_len.div_ceil(chunk_size);
        let missing = (0..total_chunks as u32)
            .filter(|seq| !self.received.contains(seq))
            .collect::<Vec<_>>();
        if missing.is_empty() && self.written == self.total_len {
            ContentFinish::Stored(self.hash)
        } else {
            ContentFinish::Missing(missing)
        }
    }
}

fn chunk_offset(total_len: usize, chunk_size: usize, seq: u32, data_len: usize) -> Result<usize> {
    if chunk_size == 0 {
        bail!("chunk size must be greater than zero");
    }

    let offset = (seq as usize)
        .checked_mul(chunk_size)
        .ok_or_else(|| anyhow!("chunk seq {seq} overflows chunk offset"))?;
    let end = offset
        .checked_add(data_len)
        .ok_or_else(|| anyhow!("chunk seq {seq} overflows chunk end"))?;
    if end > total_len {
        bail!("chunk seq {seq} exceeds payload length: end={end} total={total_len}");
    }

    let expected = total_len.saturating_sub(offset).min(chunk_size);
    if data_len != expected {
        bail!("chunk seq {seq} has invalid length {data_len}; expected {expected}");
    }

    Ok(offset)
}

fn prepare_content_slot(storage: &CasStorage, hash: CasHash, total_len: usize) -> Result<()> {
    let size = u32::try_from(total_len).context("content transfer exceeds CAS size limit")?;
    match storage.get_size(hash)? {
        None => {
            storage.add_empty(hash, size)?;
        }
        Some(existing_size) if existing_size != size => {
            bail!("CAS entry {hash} exists with size {existing_size}, expected {size}");
        }
        Some(_) => {
            let flags = storage
                .flags(hash)?
                .ok_or_else(|| anyhow!("CAS entry {hash} disappeared during transfer setup"))?;
            if flags.contains(CasFlags::INCOMPLETE) {
                return Ok(());
            }
            bail!(
                "CAS entry {hash} already exists with incompatible flags {}",
                flags.bits()
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct NetworkCore {
    participants: HashMap<String, Participant>,
    inflight_blobs: HashMap<BlobKey, BlobAssembly>,
    inflight_content: HashMap<BlobKey, ContentAssembly>,
    chunk_size: usize,
}

impl NetworkCore {
    pub fn new() -> Self {
        Self::with_chunk_size(16 * 1024)
    }

    pub fn with_chunk_size(chunk_size: usize) -> Self {
        Self {
            participants: HashMap::new(),
            inflight_blobs: HashMap::new(),
            inflight_content: HashMap::new(),
            chunk_size,
        }
    }

    pub fn register_participant(&mut self, participant: Participant) {
        self.participants
            .insert(participant.ip.clone(), participant);
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn participant(&self, ip: &str) -> Option<&Participant> {
        self.participants.get(ip)
    }

    pub fn start_blob(&mut self, source: SocketAddr, time: u64, total_len: usize) -> BlobKey {
        let key = BlobKey { source, time };
        self.inflight_blobs
            .insert(key, BlobAssembly::new(total_len));
        key
    }

    pub fn push_blob_data(
        &mut self,
        source: SocketAddr,
        time: u64,
        seq: u32,
        data: &[u8],
    ) -> Result<()> {
        let key = BlobKey { source, time };
        let blob = self
            .inflight_blobs
            .get_mut(&key)
            .ok_or_else(|| anyhow!("in-flight blob not found for {source} at {time}"))?;
        blob.insert(self.chunk_size, seq, data)
    }

    pub fn finish_blob(&mut self, source: SocketAddr, time: u64) -> Result<BlobFinish> {
        let key = BlobKey { source, time };
        let blob = self
            .inflight_blobs
            .remove(&key)
            .ok_or_else(|| anyhow!("in-flight blob not found for {source} at {time}"))?;
        Ok(blob.finish(self.chunk_size))
    }

    pub fn finish_blob_into_cas(
        &mut self,
        source: SocketAddr,
        time: u64,
        storage: &CasStorage,
    ) -> Result<BlobFinish> {
        match self.finish_blob(source, time)? {
            BlobFinish::Complete(bytes) => {
                let (hash, _) = storage.add_content(&bytes)?;
                Ok(BlobFinish::Stored(hash))
            }
            other => Ok(other),
        }
    }

    pub fn start_content_transfer(
        &mut self,
        source: SocketAddr,
        time: u64,
        hash: CasHash,
        total_len: usize,
        storage: &CasStorage,
    ) -> Result<BlobKey> {
        prepare_content_slot(storage, hash, total_len)?;

        let key = BlobKey { source, time };
        if self.inflight_content.contains_key(&key) {
            bail!("content transfer already in flight for {source} at {time}");
        }
        self.inflight_content
            .insert(key, ContentAssembly::new(hash, total_len));
        Ok(key)
    }

    pub fn push_content_data(
        &mut self,
        source: SocketAddr,
        time: u64,
        seq: u32,
        data: &[u8],
        storage: &CasStorage,
    ) -> Result<()> {
        let key = BlobKey { source, time };
        let assembly = self
            .inflight_content
            .get_mut(&key)
            .ok_or_else(|| anyhow!("content transfer not found for {source} at {time}"))?;
        let offset = assembly.chunk_offset(self.chunk_size, seq, data.len())?;
        let offset_u32 =
            u32::try_from(offset).context("content transfer chunk offset exceeds CAS limit")?;
        let len_u32 =
            u32::try_from(data.len()).context("content transfer chunk size exceeds CAS limit")?;

        if assembly.has_sequence(seq) {
            let existing = storage.read(assembly.hash, offset_u32, len_u32)?;
            if existing != data {
                bail!("duplicate content chunk seq {seq} does not match existing bytes");
            }
            return Ok(());
        }

        storage.write_incomplete(assembly.hash, offset_u32, data)?;
        assembly.mark_received(seq, data.len());
        Ok(())
    }

    pub fn end_content_transfer(
        &mut self,
        source: SocketAddr,
        time: u64,
        storage: &CasStorage,
    ) -> Result<ContentEnd> {
        let key = BlobKey { source, time };
        let Some(assembly) = self.inflight_content.get(&key) else {
            bail!("content transfer not found for {source} at {time}");
        };

        let total_chunks = assembly.total_len.div_ceil(self.chunk_size);
        let missing = (0..total_chunks as u32)
            .filter(|seq| !assembly.received.contains(seq))
            .collect::<Vec<_>>();
        if !missing.is_empty() || assembly.written != assembly.total_len {
            return Ok(ContentEnd::Incomplete(missing));
        }

        let hash = assembly.hash;
        self.inflight_content.remove(&key);
        if storage.verify(hash)? {
            Ok(ContentEnd::Stored(hash))
        } else {
            Ok(ContentEnd::Corrupt(hash))
        }
    }

    pub fn finish_content_transfer(
        &mut self,
        source: SocketAddr,
        time: u64,
        storage: &CasStorage,
    ) -> Result<ContentFinish> {
        let key = BlobKey { source, time };
        let assembly = self
            .inflight_content
            .remove(&key)
            .ok_or_else(|| anyhow!("content transfer not found for {source} at {time}"))?;
        match assembly.finish(self.chunk_size) {
            ContentFinish::Stored(hash) => {
                if storage.verify(hash)? {
                    Ok(ContentFinish::Stored(hash))
                } else {
                    Ok(ContentFinish::Corrupt(hash))
                }
            }
            other => Ok(other),
        }
    }

    pub fn has_content_transfer(&self, source: SocketAddr, time: u64) -> bool {
        self.inflight_content
            .contains_key(&BlobKey { source, time })
    }
}

impl Default for NetworkCore {
    fn default() -> Self {
        Self::new()
    }
}
