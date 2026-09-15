//! Reading tensor bytes through `loadngo-proactor`.
//!
//! A [`TensorReader`] opens every shard of a [`ShardSet`] once and serves reads as
//! positioned completion I/O on whichever port the platform uses: `io_uring` on Linux,
//! IOCP on Windows, kqueue on macOS and iOS, epoll on Android. A batch of tensors is
//! submitted in full before anything waits, so on ports that service reads concurrently
//! the batch overlaps instead of queueing one read behind another.
//!
//! Large tensors are split into chunks of at most [`MAX_CHUNK_BYTES`]: a completion
//! reports its transfer count as a `u32`, and one multi-gigabyte read would also pin that
//! much buffer in a single operation. A short read resumes where it stopped; a read that
//! returns nothing before the tensor's end means the file shrank after its header was
//! validated, and is reported as [`io::ErrorKind::UnexpectedEof`].

use std::{fs::File, io, path::PathBuf, sync::mpsc};

use loadngo_proactor::{IoBuf, IoPort, IoResult, Proactor, RawFdCompat};

use crate::shards::ShardSet;

/// The largest single read submitted to the proactor.
pub const MAX_CHUNK_BYTES: usize = 256 * 1024 * 1024;

/// Why tensor bytes could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("cannot open shard {path}: {source}")]
    Open { path: PathBuf, source: io::Error },
    #[error("cannot start the proactor: {0}")]
    Proactor(io::Error),
    #[error("no tensor named {0:?}")]
    UnknownTensor(String),
    #[error("reading {name:?} at file offset {offset}: {source}")]
    Io {
        name: String,
        offset: u64,
        source: io::Error,
    },
}

/// Positioned reads of tensor data from an open checkpoint.
pub struct TensorReader<P: IoPort> {
    set: ShardSet,
    files: Vec<File>,
    proactor: Proactor<P>,
    chunk_limit: usize,
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "android",
    windows
))]
impl TensorReader<loadngo_proactor::PlatformPort> {
    /// Opens every shard of `set` for reading on this platform's completion port.
    pub fn open(set: ShardSet) -> Result<Self, ReadError> {
        let proactor = loadngo_proactor::new_platform_proactor().map_err(ReadError::Proactor)?;
        Self::with_proactor(set, proactor)
    }
}

impl<P: IoPort> TensorReader<P> {
    /// Opens every shard of `set`, reading through `proactor`.
    pub fn with_proactor(set: ShardSet, proactor: Proactor<P>) -> Result<Self, ReadError> {
        let files = set
            .shards()
            .iter()
            .map(|shard| {
                open_for_completion_reads(&shard.path).map_err(|source| ReadError::Open {
                    path: shard.path.clone(),
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            set,
            files,
            proactor,
            chunk_limit: MAX_CHUNK_BYTES,
        })
    }

    /// The checkpoint this reader serves.
    pub fn shards(&self) -> &ShardSet {
        &self.set
    }

    /// The bytes of each named tensor, in the order asked. Every chunk of every tensor
    /// is submitted before the first completion is awaited.
    ///
    /// # Errors
    ///
    /// Returns [`ReadError`] for an unknown name or a failed read. Reads already in flight
    /// are completed and discarded before the error is returned, so the reader stays
    /// usable. The exception is [`ReadError::Proactor`]: when the port itself fails, the
    /// outstanding reads cannot be driven to completion and the reader should be dropped.
    pub fn read_tensors(&self, names: &[&str]) -> Result<Vec<Vec<u8>>, ReadError> {
        let mut plans = Vec::with_capacity(names.len());
        for &name in names {
            let (shard, tensor) = self
                .set
                .shards()
                .iter()
                .enumerate()
                .find_map(|(i, shard)| shard.header.get(name).map(|t| (i, t)))
                .ok_or_else(|| ReadError::UnknownTensor(name.to_owned()))?;
            let len = usize::try_from(tensor.len).map_err(|_| ReadError::Io {
                name: name.to_owned(),
                offset: tensor.offset,
                source: io::Error::new(io::ErrorKind::OutOfMemory, "tensor exceeds address space"),
            })?;
            plans.push((name, shard, tensor.offset, len));
        }

        // One chunk per (tensor, start); each is read into its own buffer and copied into
        // place, so a short read of one chunk never shifts another.
        let mut outputs: Vec<Vec<u8>> = plans.iter().map(|&(_, _, _, len)| vec![0; len]).collect();
        let (tx, rx) = mpsc::channel::<(usize, usize, u64, usize, IoResult)>();
        let handle = self.proactor.handle();
        let mut pending = 0_usize;
        let mut first_error: Option<ReadError> = None;

        let submit = |tensor: usize,
                      start: usize,
                      want: usize,
                      pending: &mut usize|
         -> Result<(), ReadError> {
            let (name, shard, base, _) = plans[tensor];
            let offset = base + start as u64;
            let tx = tx.clone();
            handle
                .read(
                    raw_handle(&self.files[shard]),
                    IoBuf::with_capacity(want),
                    offset,
                    move |result: IoResult| {
                        // The receiver outlives every in-flight read of this call.
                        let _ = tx.send((tensor, start, offset, want, result));
                    },
                )
                .map_err(|source| ReadError::Io {
                    name: name.to_owned(),
                    offset,
                    source,
                })?;
            *pending += 1;
            Ok(())
        };

        'submit: for (tensor, &(_, _, _, len)) in plans.iter().enumerate() {
            let mut start = 0;
            while start < len {
                let want = (len - start).min(self.chunk_limit);
                if let Err(error) = submit(tensor, start, want, &mut pending) {
                    first_error = Some(error);
                    break 'submit;
                }
                start += want;
            }
        }

        while pending > 0 {
            if let Err(source) = self.proactor.run_once() {
                first_error.get_or_insert(ReadError::Proactor(source));
                break;
            }
            while let Ok((tensor, start, offset, want, result)) = rx.try_recv() {
                pending -= 1;
                if first_error.is_some() {
                    continue;
                }
                let name = plans[tensor].0;
                match result {
                    Ok(transfer) => {
                        let got = transfer.bytes_transferred as usize;
                        let bytes = transfer.buf.into_vec();
                        if got == 0 {
                            first_error = Some(ReadError::Io {
                                name: name.to_owned(),
                                offset,
                                source: io::Error::new(
                                    io::ErrorKind::UnexpectedEof,
                                    "the shard ended before the tensor did",
                                ),
                            });
                            continue;
                        }
                        outputs[tensor][start..start + got].copy_from_slice(&bytes[..got]);
                        if got < want {
                            if let Err(error) =
                                submit(tensor, start + got, want - got, &mut pending)
                            {
                                first_error = Some(error);
                            }
                        }
                    }
                    Err(source) => {
                        first_error = Some(ReadError::Io {
                            name: name.to_owned(),
                            offset,
                            source: normalise_eof(source),
                        });
                    }
                }
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(outputs),
        }
    }

    #[cfg(test)]
    fn set_chunk_limit(&mut self, bytes: usize) {
        self.chunk_limit = bytes.max(1);
    }
}

/// Windows reports a read at or past the end of a file as `ERROR_HANDLE_EOF` (38) rather
/// than as zero bytes; both mean the same thing here.
fn normalise_eof(error: io::Error) -> io::Error {
    const ERROR_HANDLE_EOF: i32 = 38;
    if cfg!(windows) && error.raw_os_error() == Some(ERROR_HANDLE_EOF) {
        io::Error::new(io::ErrorKind::UnexpectedEof, error)
    } else {
        error
    }
}

#[cfg(unix)]
fn open_for_completion_reads(path: &std::path::Path) -> io::Result<File> {
    File::open(path)
}

/// IOCP queues completions only for handles opened with `FILE_FLAG_OVERLAPPED`; a plain
/// handle would complete synchronously and never report through the port.
#[cfg(windows)]
fn open_for_completion_reads(path: &std::path::Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(path)
}

#[cfg(unix)]
fn raw_handle(file: &File) -> RawFdCompat {
    use std::os::fd::AsRawFd;
    file.as_raw_fd()
}

#[cfg(windows)]
fn raw_handle(file: &File) -> RawFdCompat {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle() as usize as u64
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::{safetensors::tests::file_bytes, shards::INDEX_FILE};

    /// Writes a shard whose tensors hold distinct, position-dependent bytes and returns
    /// each tensor's expected contents.
    fn shard(
        dir: &Path,
        file: &str,
        tensors: &[(&str, usize)],
        seed: u8,
    ) -> Vec<(String, Vec<u8>)> {
        let mut entries = Vec::new();
        let mut data = Vec::new();
        let mut expected = Vec::new();
        for (i, &(name, len)) in tensors.iter().enumerate() {
            let bytes: Vec<u8> = (0..len)
                .map(|j| {
                    (j as u8)
                        .wrapping_mul(31)
                        .wrapping_add(seed)
                        .wrapping_add(i as u8 * 97)
                })
                .collect();
            entries.push(format!(
                r#""{name}":{{"dtype":"U8","shape":[{len}],"data_offsets":[{},{}]}}"#,
                data.len(),
                data.len() + len
            ));
            data.extend_from_slice(&bytes);
            expected.push((name.to_owned(), bytes));
        }
        let header = format!("{{{}}}", entries.join(","));
        fs::write(dir.join(file), file_bytes(&header, &data)).unwrap();
        expected
    }

    fn checkpoint(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut expected = shard(
            dir,
            "s1.safetensors",
            &[("a", 1000), ("b", 3), ("empty", 0)],
            7,
        );
        expected.extend(shard(dir, "s2.safetensors", &[("c", 70_000)], 91));
        let map: Vec<String> = expected
            .iter()
            .map(|(name, _)| {
                let file = if name == "c" { "s2" } else { "s1" };
                format!(r#""{name}":"{file}.safetensors""#)
            })
            .collect();
        fs::write(
            dir.join(INDEX_FILE),
            format!(r#"{{"weight_map":{{{}}}}}"#, map.join(",")),
        )
        .unwrap();
        expected
    }

    #[test]
    fn a_batch_across_shards_returns_every_tensor_byte_exact_in_the_order_asked() {
        let dir = tempfile::tempdir().unwrap();
        let expected = checkpoint(dir.path());
        let reader = TensorReader::open(ShardSet::open(dir.path()).unwrap()).expect("reader opens");
        let got = reader
            .read_tensors(&["c", "a", "empty", "b", "a"])
            .expect("reads");
        let want = |name: &str| &expected.iter().find(|(n, _)| n == name).unwrap().1;
        assert_eq!(got.len(), 5);
        for (bytes, name) in got.iter().zip(["c", "a", "empty", "b", "a"]) {
            assert_eq!(bytes, want(name), "tensor {name}");
        }
    }

    #[test]
    fn tensors_larger_than_a_chunk_are_reassembled_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let expected = checkpoint(dir.path());
        let mut reader =
            TensorReader::open(ShardSet::open(dir.path()).unwrap()).expect("reader opens");
        // 70,000 bytes in 7-byte chunks: 10,000 reads in one batch.
        reader.set_chunk_limit(7);
        let got = reader.read_tensors(&["c", "a"]).expect("reads");
        assert_eq!(got[0], expected.iter().find(|(n, _)| n == "c").unwrap().1);
        assert_eq!(got[1], expected.iter().find(|(n, _)| n == "a").unwrap().1);
    }

    #[test]
    fn a_shard_that_shrank_after_opening_is_an_error_and_the_reader_stays_usable() {
        let dir = tempfile::tempdir().unwrap();
        let expected = checkpoint(dir.path());
        let reader = TensorReader::open(ShardSet::open(dir.path()).unwrap()).expect("reader opens");

        let path = dir.path().join("s2.safetensors");
        let full = fs::metadata(&path).unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(full - 50_000)
            .unwrap();

        let error = reader
            .read_tensors(&["a", "c"])
            .expect_err("c is truncated");
        match error {
            ReadError::Io { name, source, .. } => {
                assert_eq!(name, "c");
                assert_eq!(source.kind(), io::ErrorKind::UnexpectedEof, "{source}");
            }
            other => panic!("unexpected error {other}"),
        }

        let again = reader
            .read_tensors(&["a"])
            .expect("the other shard still reads");
        assert_eq!(again[0], expected.iter().find(|(n, _)| n == "a").unwrap().1);
    }

    #[test]
    fn an_unknown_tensor_is_refused_before_anything_is_submitted() {
        let dir = tempfile::tempdir().unwrap();
        checkpoint(dir.path());
        let reader = TensorReader::open(ShardSet::open(dir.path()).unwrap()).expect("reader opens");
        assert!(matches!(
            reader.read_tensors(&["a", "missing"]),
            Err(ReadError::UnknownTensor(name)) if name == "missing"
        ));
    }
}
