//! Experimental immutable packed snapshots of raw Archive CAS objects.
//!
//! Loose objects remain authoritative and untouched. The snapshot index retains
//! each object's BLAKE3 identity and byte range; reads use the caller's proactor.

use super::{
    open_restore_source, raw_file_handle, read_some_with_proactor_buffer,
    write_all_owned_with_proactor_reuse, ArchiveCasRestoreIo, ArchiveCasStorage, ArchiveObject,
    PROACTOR_READ_BUFFER_BYTES,
};
use crate::cas::CasHash;
use anyhow::{anyhow, bail, Context, Result};
use loadngo_proactor::{CompletionPort, IoPort, Proactor, ProactorHandle};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const INDEX_MAGIC: &[u8; 8] = b"LNGOPK01";
const INDEX_ENTRY_BYTES: usize = 48;
const INDEX_HEADER_BYTES: usize = 24;
const INDEX_CHECKSUM_BYTES: usize = 32;
const INDEX_BATCH_ENTRIES: usize = 4096;
const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedObject {
    pub offset: u64,
    pub size: u64,
}

/// A seek-positioned reader limited to one object's packed byte range.
pub struct PackedObjectReader {
    file: File,
    remaining: u64,
}

impl Read for PackedObjectReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() || self.remaining == 0 {
            return Ok(0);
        }
        let amount = output
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.file.read(&mut output[..amount])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// A complete immutable pack snapshot and its validated in-memory index.
#[derive(Debug)]
pub struct PackedSnapshot {
    directory: PathBuf,
    pack_path: PathBuf,
    entries: BTreeMap<CasHash, PackedObject>,
    pack_bytes: u64,
}

impl PackedSnapshot {
    /// Opens valid snapshots directly below `packs_root`; unfinished directories
    /// without a published index are ignored.
    pub fn discover(packs_root: &Path) -> Result<Vec<Self>> {
        let mut directories = Vec::new();
        for entry in fs::read_dir(packs_root)
            .with_context(|| format!("failed to enumerate {}", packs_root.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.path().join("objects.idx").is_file() {
                directories.push(entry.path());
            }
        }
        directories.sort();
        directories.iter().map(|path| Self::open(path)).collect()
    }

    /// Packs every currently loose `.blob` object without changing the source CAS.
    /// The pack data is written in bounded proactor buffers. Index and data are
    /// synced before publication; the index is published last as the commit marker.
    pub fn create_from_loose_objects<P, R>(
        store: &ArchiveCasStorage,
        destination: &Path,
        proactor: &Proactor<P>,
        handle: &ProactorHandle<P>,
        restore_io: &mut R,
    ) -> Result<Self>
    where
        P: CompletionPort + IoPort,
        R: ArchiveCasRestoreIo<P>,
    {
        if destination.exists() {
            bail!("pack destination already exists: {}", destination.display());
        }
        let objects_root = store.root().join("objects");
        let mut sources = Vec::new();
        for shard in fs::read_dir(&objects_root)
            .with_context(|| format!("failed to enumerate {}", objects_root.display()))?
        {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("blob") {
                    continue;
                }
                if !entry.file_type()?.is_file() {
                    bail!("CAS object is not a regular file: {}", path.display());
                }
                let name = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| anyhow!("invalid CAS object filename: {}", path.display()))?;
                let hash: CasHash = name.parse()?;
                let shard_name = shard.file_name();
                if shard_name.to_str() != Some(&hash.to_hex()[..2]) {
                    bail!("CAS object is in the wrong hash shard: {}", path.display());
                }
                let metadata = entry.metadata()?;
                sources.push((hash, path, metadata.len()));
            }
        }
        sources.sort_by_key(|(hash, _, _)| *hash);
        if sources.is_empty() {
            bail!("CAS root has no loose .blob objects to pack");
        }
        let index_bytes = INDEX_HEADER_BYTES as u64
            + INDEX_ENTRY_BYTES as u64 * sources.len() as u64
            + INDEX_CHECKSUM_BYTES as u64;
        if index_bytes > MAX_INDEX_BYTES {
            bail!("packed index exceeds the {MAX_INDEX_BYTES}-byte limit");
        }

        let directory = destination.to_path_buf();
        restore_io.run_blocking(move || {
            fs::create_dir(&directory).with_context(|| {
                format!("failed to create pack directory {}", directory.display())
            })?;
            Ok(())
        })?;
        let pack_partial = destination.join("objects.pack.partial");
        let index_partial = destination.join("objects.idx.partial");
        let pack_final = destination.join("objects.pack");
        let index_final = destination.join("objects.idx");

        let result = (|| -> Result<()> {
            let path = pack_partial.clone();
            let pack_file =
                restore_io.run_blocking(move || super::create_restore_destination(&path))?;
            let pack_fd = handle.register(raw_file_handle(&pack_file))?;
            let mut pack_offset = 0_u64;
            let mut entries = Vec::with_capacity(sources.len());
            let mut reusable = vec![0_u8; PROACTOR_READ_BUFFER_BYTES];
            let pack_result = (|| -> Result<()> {
                for (hash, source_path, expected_size) in &sources {
                    let path = source_path.clone();
                    let source = restore_io.run_blocking(move || open_restore_source(&path))?;
                    let source_fd = handle.register(raw_file_handle(&source))?;
                    let object_offset = pack_offset;
                    let mut source_offset = 0_u64;
                    let mut hasher = blake3::Hasher::new();
                    let source_result = (|| -> Result<()> {
                        while source_offset < *expected_size {
                            restore_io
                                .admit(proactor, handle)
                                .context("thermal admission before pack source read")?;
                            let transfer = read_some_with_proactor_buffer(
                                proactor,
                                handle,
                                source_fd,
                                source_offset,
                                std::mem::take(&mut reusable),
                            )?;
                            let count = transfer.bytes_transferred as usize;
                            if count == 0 || count > transfer.buf.len() {
                                bail!("invalid or premature pack source read: {count} bytes");
                            }
                            source_offset = source_offset
                                .checked_add(count as u64)
                                .ok_or_else(|| anyhow!("source object offset exceeds u64"))?;
                            if source_offset > *expected_size {
                                bail!("source object is larger than its recorded size");
                            }
                            let mut bytes = transfer.buf.into_vec();
                            bytes.truncate(count);
                            hasher.update(&bytes);
                            restore_io
                                .admit(proactor, handle)
                                .context("thermal admission before pack write")?;
                            let (next_offset, returned) = write_all_owned_with_proactor_reuse(
                                proactor,
                                handle,
                                pack_fd,
                                pack_offset,
                                bytes,
                            )?;
                            if next_offset - pack_offset != count as u64 {
                                bail!("proactor pack write length differs from source read");
                            }
                            pack_offset = next_offset;
                            reusable = returned;
                        }
                        if source_offset != *expected_size {
                            bail!("source object length changed while packing");
                        }
                        let actual = CasHash::from_bytes(*hasher.finalize().as_bytes());
                        if actual != *hash {
                            bail!("source object hash mismatch: expected {hash}, found {actual}");
                        }
                        Ok(())
                    })();
                    handle.release(source_fd);
                    restore_io.run_blocking(move || {
                        drop(source);
                        Ok(())
                    })?;
                    source_result.with_context(|| format!("failed to pack object {hash}"))?;
                    entries.push((
                        *hash,
                        PackedObject {
                            offset: object_offset,
                            size: *expected_size,
                        },
                    ));
                }
                Ok(())
            })();
            handle.release(pack_fd);
            if let Err(error) = pack_result {
                restore_io.run_blocking(move || {
                    drop(pack_file);
                    Ok(())
                })?;
                return Err(error);
            }
            restore_io.run_blocking(move || {
                pack_file
                    .sync_all()
                    .context("failed to sync packed object data")?;
                drop(pack_file);
                Ok(())
            })?;

            write_index_with_restore_io(
                &index_partial,
                pack_offset,
                &entries,
                proactor,
                handle,
                restore_io,
            )?;
            let directory = destination.to_path_buf();
            restore_io.run_blocking(move || {
                fs::rename(&pack_partial, &pack_final)
                    .context("failed to publish packed object data")?;
                sync_directory(&directory)?;
                fs::rename(&index_partial, &index_final)
                    .context("failed to publish packed object index")?;
                sync_directory(&directory)?;
                Ok(())
            })?;
            Ok(())
        })();
        if let Err(error) = result {
            let directory = destination.to_path_buf();
            let _ = restore_io.run_blocking(move || {
                fs::remove_dir_all(directory)?;
                Ok(())
            });
            return Err(error);
        }
        Self::open(destination)
    }

    /// Opens and fully validates the fixed-width index and pack length.
    pub fn open(directory: &Path) -> Result<Self> {
        let index_path = directory.join("objects.idx");
        let pack_path = directory.join("objects.pack");
        let metadata = fs::metadata(&index_path)
            .with_context(|| format!("missing packed CAS index {}", index_path.display()))?;
        if metadata.len() > MAX_INDEX_BYTES {
            bail!("packed index exceeds the {MAX_INDEX_BYTES}-byte limit");
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(&index_path)?.read_to_end(&mut bytes)?;
        if bytes.len() < INDEX_HEADER_BYTES + INDEX_CHECKSUM_BYTES || &bytes[..8] != INDEX_MAGIC {
            bail!("invalid packed CAS index header");
        }
        let count = u64::from_be_bytes(bytes[8..16].try_into()?);
        let pack_bytes = u64::from_be_bytes(bytes[16..24].try_into()?);
        let entries_bytes = count
            .checked_mul(INDEX_ENTRY_BYTES as u64)
            .ok_or_else(|| anyhow!("packed index entry count overflows"))?;
        let expected_len = (INDEX_HEADER_BYTES as u64)
            .checked_add(entries_bytes)
            .and_then(|size| size.checked_add(INDEX_CHECKSUM_BYTES as u64))
            .ok_or_else(|| anyhow!("packed index length overflows"))?;
        if expected_len != bytes.len() as u64 {
            bail!("packed index length does not match its entry count");
        }
        let checksum_at = bytes.len() - INDEX_CHECKSUM_BYTES;
        if blake3::hash(&bytes[..checksum_at]).as_bytes() != &bytes[checksum_at..] {
            bail!("packed index checksum mismatch");
        }
        if fs::metadata(&pack_path)?.len() != pack_bytes {
            bail!("packed data length does not match its index");
        }
        let mut entries = BTreeMap::new();
        let mut cursor = INDEX_HEADER_BYTES;
        let mut expected_offset = 0_u64;
        let mut previous = None;
        for _ in 0..count {
            let hash = CasHash::from_slice(&bytes[cursor..cursor + 32])?;
            let offset = u64::from_be_bytes(bytes[cursor + 32..cursor + 40].try_into()?);
            let size = u64::from_be_bytes(bytes[cursor + 40..cursor + 48].try_into()?);
            cursor += INDEX_ENTRY_BYTES;
            if previous.is_some_and(|prior| prior >= hash) {
                bail!("packed index hashes are not strictly ordered");
            }
            if offset != expected_offset {
                bail!("packed object range overlaps or has a gap");
            }
            expected_offset = offset
                .checked_add(size)
                .ok_or_else(|| anyhow!("packed object range exceeds u64"))?;
            previous = Some(hash);
            entries.insert(hash, PackedObject { offset, size });
        }
        if expected_offset != pack_bytes {
            bail!("packed index does not cover the complete data file");
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            pack_path,
            entries,
            pack_bytes,
        })
    }

    pub fn object_count(&self) -> usize {
        self.entries.len()
    }

    pub fn objects(&self) -> impl Iterator<Item = (CasHash, PackedObject)> + '_ {
        self.entries.iter().map(|(hash, object)| (*hash, *object))
    }

    pub fn object(&self, hash: CasHash) -> Option<PackedObject> {
        self.entries.get(&hash).copied()
    }

    pub fn open_object_reader(&self, hash: CasHash) -> Result<Option<PackedObjectReader>> {
        let Some(object) = self.entries.get(&hash) else {
            return Ok(None);
        };
        let mut file = File::open(&self.pack_path)
            .with_context(|| format!("failed to open packed CAS {}", self.pack_path.display()))?;
        file.seek(SeekFrom::Start(object.offset))?;
        Ok(Some(PackedObjectReader {
            file,
            remaining: object.size,
        }))
    }

    pub fn pack_bytes(&self) -> u64 {
        self.pack_bytes
    }

    pub fn index_bytes(&self) -> Result<u64> {
        Ok(fs::metadata(self.directory.join("objects.idx"))?.len())
    }

    /// Reads a stream of bounded object ranges while keeping one pack handle open.
    /// Each request is `(hash, offset, maximum_bytes)`; the callback receives the
    /// actual bytes, which may be shorter at the object's end. The range buffer
    /// never exceeds one proactor read buffer, and the caller controls how many
    /// requests the iterator yields. Returned partial ranges are not independently
    /// hash-verifiable; callers must verify the complete object or validate the
    /// reconstructed whole file before trusting these bytes.
    pub fn read_ranges_with_proactor<P, R, I, F>(
        &self,
        proactor: &Proactor<P>,
        handle: &ProactorHandle<P>,
        restore_io: &mut R,
        ranges: I,
        mut on_range: F,
    ) -> Result<usize>
    where
        P: CompletionPort + IoPort,
        R: ArchiveCasRestoreIo<P>,
        I: IntoIterator<Item = (CasHash, u64, usize)>,
        F: FnMut(CasHash, u64, &[u8]) -> Result<()>,
    {
        let pack_path = self.pack_path.clone();
        let file = restore_io.run_blocking(move || open_restore_source(&pack_path))?;
        let fd = match handle.register(raw_file_handle(&file)) {
            Ok(fd) => fd,
            Err(error) => {
                restore_io.run_blocking(move || {
                    drop(file);
                    Ok(())
                })?;
                return Err(error).context("failed to register packed CAS for range reads");
            }
        };
        let result = (|| -> Result<usize> {
            let mut scratch = vec![0_u8; PROACTOR_READ_BUFFER_BYTES];
            let mut output = Vec::with_capacity(PROACTOR_READ_BUFFER_BYTES);
            let mut count_ranges = 0;
            for (hash, offset, maximum_bytes) in ranges {
                if maximum_bytes > PROACTOR_READ_BUFFER_BYTES {
                    bail!(
                        "packed range request is {maximum_bytes} bytes, exceeding the {}-byte limit",
                        PROACTOR_READ_BUFFER_BYTES
                    );
                }
                let object = self
                    .entries
                    .get(&hash)
                    .ok_or_else(|| anyhow!("packed object is missing: {hash}"))?;
                if offset > object.size {
                    bail!("offset {offset} past end of packed object {hash}");
                }
                let amount = usize::try_from((object.size - offset).min(maximum_bytes as u64))
                    .context("packed range length exceeds local addressing")?;
                output.clear();
                output.resize(amount, 0);
                let mut filled = 0usize;
                while filled < amount {
                    restore_io
                        .admit(proactor, handle)
                        .context("thermal admission before packed range read")?;
                    let absolute_offset = object
                        .offset
                        .checked_add(offset)
                        .and_then(|position| position.checked_add(filled as u64))
                        .ok_or_else(|| anyhow!("packed range offset exceeds u64"))?;
                    let transfer = read_some_with_proactor_buffer(
                        proactor,
                        handle,
                        fd,
                        absolute_offset,
                        std::mem::take(&mut scratch),
                    )?;
                    let transferred = transfer.bytes_transferred as usize;
                    if transferred == 0 || transferred > transfer.buf.len() {
                        bail!("invalid packed range read for {hash}: {transferred} bytes");
                    }
                    let read_bytes = transfer.buf.into_vec();
                    let copied = transferred.min(amount - filled);
                    output[filled..filled + copied].copy_from_slice(&read_bytes[..copied]);
                    filled += copied;
                    scratch = read_bytes;
                }
                on_range(hash, offset, &output)?;
                count_ranges += 1;
            }
            Ok(count_ranges)
        })();
        handle.release(fd);
        restore_io.run_blocking(move || {
            drop(file);
            Ok(())
        })?;
        result
    }

    /// Reads a caller-sized object range. Large outputs are divided into bounded
    /// requests; typical CAS buffers fit in one request and use one open pack handle.
    pub fn read_range_into_with_proactor<P, R>(
        &self,
        proactor: &Proactor<P>,
        handle: &ProactorHandle<P>,
        restore_io: &mut R,
        hash: CasHash,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize>
    where
        P: CompletionPort + IoPort,
        R: ArchiveCasRestoreIo<P>,
    {
        let object = self
            .entries
            .get(&hash)
            .ok_or_else(|| anyhow!("packed object is missing: {hash}"))?;
        if offset > object.size {
            bail!("offset {offset} past end of packed object {hash}");
        }
        let available = usize::try_from(
            (object.size - offset).min(u64::try_from(output.len()).unwrap_or(u64::MAX)),
        )
        .context("packed range length exceeds local addressing")?;
        if available == 0 {
            return Ok(0);
        }
        let mut filled = 0usize;
        while filled < available {
            let request = (available - filled).min(PROACTOR_READ_BUFFER_BYTES);
            let range_offset = offset
                .checked_add(filled as u64)
                .ok_or_else(|| anyhow!("packed range offset exceeds u64"))?;
            let mut received = 0usize;
            self.read_ranges_with_proactor(
                proactor,
                handle,
                restore_io,
                std::iter::once((hash, range_offset, request)),
                |_hash, _offset, bytes| {
                    let end = filled + bytes.len();
                    output[filled..end].copy_from_slice(bytes);
                    received = bytes.len();
                    Ok(())
                },
            )?;
            if received == 0 {
                bail!("packed range read made no progress for {hash}");
            }
            filled += received;
        }
        Ok(filled)
    }

    pub fn verify_object_with_proactor<P, R>(
        &self,
        proactor: &Proactor<P>,
        handle: &ProactorHandle<P>,
        restore_io: &mut R,
        expected: ArchiveObject,
    ) -> Result<()>
    where
        P: CompletionPort + IoPort,
        R: ArchiveCasRestoreIo<P>,
    {
        let object = self
            .entries
            .get(&expected.hash)
            .ok_or_else(|| anyhow!("packed object is missing: {}", expected.hash))?;
        if object.size != expected.size {
            bail!(
                "packed object size mismatch for {}: expected {}, got {}",
                expected.hash,
                expected.size,
                object.size
            );
        }
        let range_bytes = PROACTOR_READ_BUFFER_BYTES as u64;
        let range_count = expected.size.div_ceil(range_bytes);
        let ranges = (0..range_count).map(|index| {
            (
                expected.hash,
                index * range_bytes,
                PROACTOR_READ_BUFFER_BYTES,
            )
        });
        let mut hasher = blake3::Hasher::new();
        self.read_ranges_with_proactor(
            proactor,
            handle,
            restore_io,
            ranges,
            |_hash, _offset, bytes| {
                hasher.update(bytes);
                Ok(())
            },
        )?;
        let hash = CasHash::from_bytes(*hasher.finalize().as_bytes());
        super::check_object(expected, hash, object.size)
    }

    pub fn stream_object_to_with_proactor<P, R, W>(
        &self,
        proactor: &Proactor<P>,
        handle: &ProactorHandle<P>,
        restore_io: &mut R,
        expected: ArchiveObject,
        output: &mut W,
    ) -> Result<()>
    where
        P: CompletionPort + IoPort,
        R: ArchiveCasRestoreIo<P>,
        W: Write + ?Sized,
    {
        self.verify_object_with_proactor(proactor, handle, restore_io, expected)?;
        let mut buffer = vec![0_u8; PROACTOR_READ_BUFFER_BYTES];
        let mut hasher = blake3::Hasher::new();
        let mut offset = 0_u64;
        while offset < expected.size {
            let count = self.read_range_into_with_proactor(
                proactor,
                handle,
                restore_io,
                expected.hash,
                offset,
                &mut buffer,
            )?;
            if count == 0 {
                bail!("packed stream read made no progress for {}", expected.hash);
            }
            hasher.update(&buffer[..count]);
            restore_io.admit(proactor, handle)?;
            output.write_all(&buffer[..count])?;
            offset = offset
                .checked_add(count as u64)
                .ok_or_else(|| anyhow!("packed stream offset exceeds u64"))?;
        }
        super::check_object(
            expected,
            CasHash::from_bytes(*hasher.finalize().as_bytes()),
            offset,
        )
    }

    /// Reads and BLAKE3-verifies every packed object with one bounded reusable buffer.
    pub fn verify_all<P, R>(
        &self,
        proactor: &Proactor<P>,
        handle: &ProactorHandle<P>,
        restore_io: &mut R,
    ) -> Result<()>
    where
        P: CompletionPort + IoPort,
        R: ArchiveCasRestoreIo<P>,
    {
        let path = self.pack_path.clone();
        let file = restore_io.run_blocking(move || open_restore_source(&path))?;
        let fd = handle.register(raw_file_handle(&file))?;
        let result = (|| -> Result<()> {
            let mut buffer = vec![0_u8; PROACTOR_READ_BUFFER_BYTES];
            for (expected_hash, object) in &self.entries {
                let mut hash = blake3::Hasher::new();
                let mut consumed = 0_u64;
                while consumed < object.size {
                    restore_io
                        .admit(proactor, handle)
                        .context("thermal admission before packed verification read")?;
                    let offset = object
                        .offset
                        .checked_add(consumed)
                        .ok_or_else(|| anyhow!("packed object range offset exceeds u64"))?;
                    let transfer =
                        read_some_with_proactor_buffer(proactor, handle, fd, offset, buffer)?;
                    let count = transfer.bytes_transferred as usize;
                    if count == 0 || count > transfer.buf.len() {
                        bail!("invalid packed object read for {expected_hash}: {count} bytes");
                    }
                    let bytes = transfer.buf.into_vec();
                    let count =
                        count.min(usize::try_from(object.size - consumed).unwrap_or(usize::MAX));
                    hash.update(&bytes[..count]);
                    consumed += count as u64;
                    buffer = bytes;
                }
                let actual = CasHash::from_bytes(*hash.finalize().as_bytes());
                if actual != *expected_hash {
                    bail!("packed object hash mismatch: expected {expected_hash}, found {actual}");
                }
            }
            Ok(())
        })();
        handle.release(fd);
        restore_io.run_blocking(move || {
            drop(file);
            Ok(())
        })?;
        result
    }
}

fn write_index_with_restore_io<P, R>(
    path: &Path,
    pack_bytes: u64,
    entries: &[(CasHash, PackedObject)],
    proactor: &Proactor<P>,
    handle: &ProactorHandle<P>,
    restore_io: &mut R,
) -> Result<()>
where
    P: CompletionPort + IoPort,
    R: ArchiveCasRestoreIo<P>,
{
    let path_for_open = path.to_path_buf();
    let mut file = restore_io.run_blocking(move || {
        File::create(&path_for_open)
            .with_context(|| format!("failed to create packed index {}", path_for_open.display()))
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut header = Vec::with_capacity(INDEX_HEADER_BYTES);
    header.extend_from_slice(INDEX_MAGIC);
    header.extend_from_slice(&(entries.len() as u64).to_be_bytes());
    header.extend_from_slice(&pack_bytes.to_be_bytes());
    restore_io.admit(proactor, handle)?;
    (file, hasher) = restore_io.run_blocking(move || {
        file.write_all(&header)?;
        hasher.update(&header);
        Ok((file, hasher))
    })?;
    for batch in entries.chunks(INDEX_BATCH_ENTRIES) {
        let mut bytes = Vec::with_capacity(batch.len() * INDEX_ENTRY_BYTES);
        for (hash, object) in batch {
            bytes.extend_from_slice(hash.as_bytes());
            bytes.extend_from_slice(&object.offset.to_be_bytes());
            bytes.extend_from_slice(&object.size.to_be_bytes());
        }
        restore_io.admit(proactor, handle)?;
        (file, hasher) = restore_io.run_blocking(move || {
            file.write_all(&bytes)?;
            hasher.update(&bytes);
            Ok((file, hasher))
        })?;
    }
    restore_io.admit(proactor, handle)?;
    restore_io.run_blocking(move || {
        file.write_all(hasher.finalize().as_bytes())?;
        file.sync_all()?;
        drop(file);
        Ok(())
    })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}
