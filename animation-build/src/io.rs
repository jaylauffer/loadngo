//! File I/O through loadngo-proactor: positioned reads and writes on the platform's
//! completion port (io_uring on Linux, IOCP on Windows, kqueue on macOS and iOS, epoll on
//! Android), every chunk of a request submitted before the first completion is awaited.
//!
//! Each file is registered with the port while it is read or written and released before
//! it closes, as `weights::reader` does, so IOCP associates its handle once. Opening,
//! sizing and renaming are ordinary blocking calls: metadata, not payload.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use loadngo_proactor::{IoBuf, IoPort, IoResult, Proactor, RawFdCompat};

/// The largest single read or write submitted (a completion counts bytes in a `u32`).
pub const CHUNK: usize = 4 * 1024 * 1024;

#[cfg(unix)]
fn raw(file: &File) -> RawFdCompat {
    use std::os::fd::AsRawFd;
    file.as_raw_fd()
}

#[cfg(windows)]
fn raw(file: &File) -> RawFdCompat {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle() as usize as u64
}

#[cfg(windows)]
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

/// Opens for completion reads (overlapped on Windows, where IOCP needs it).
pub fn open_read(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

/// Creates (truncating) for completion writes.
pub fn create_write(path: &Path) -> io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(FILE_FLAG_OVERLAPPED);
    }
    o.open(path)
}

/// Windows reports a read at the end as `ERROR_HANDLE_EOF` rather than zero bytes.
fn is_eof(e: &io::Error) -> bool {
    cfg!(windows) && e.raw_os_error() == Some(38)
}

/// A file being read in full: the requests are submitted at once; [`ReadFile::accept`]
/// takes each completion.
pub struct ReadFile {
    pub path: PathBuf,
    file: File,
    fd: RawFdCompat,
    pub bytes: Vec<u8>,
    pending: usize,
    error: Option<io::Error>,
}

/// A read completion, tagged with the request it answers.
pub struct ReadDone {
    pub id: usize,
    pub offset: u64,
    pub want: usize,
    pub result: IoResult,
}

impl ReadFile {
    /// Opens `path`, registers it and submits reads of all of it, each completion sent on
    /// `done` tagged with `id`.
    ///
    /// # Errors
    /// Opening, sizing, registering or submitting.
    pub fn start<P: IoPort>(
        proactor: &Proactor<P>,
        path: &Path,
        id: usize,
        done: &mpsc::Sender<ReadDone>,
    ) -> io::Result<Self> {
        let file = open_read(path)?;
        let len = usize::try_from(file.metadata()?.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::OutOfMemory, "file exceeds the address space")
        })?;
        let handle = proactor.handle();
        let fd = handle.register(raw(&file))?;
        let mut me = Self {
            path: path.to_owned(),
            file,
            fd,
            bytes: vec![0; len],
            pending: 0,
            error: None,
        };
        let mut at = 0usize;
        while at < len {
            let want = (len - at).min(CHUNK);
            if let Err(e) = me.submit(proactor, id, at as u64, want, done) {
                me.error = Some(e);
                break;
            }
            at += want;
        }
        Ok(me)
    }

    fn submit<P: IoPort>(
        &mut self,
        proactor: &Proactor<P>,
        id: usize,
        offset: u64,
        want: usize,
        done: &mpsc::Sender<ReadDone>,
    ) -> io::Result<()> {
        let tx = done.clone();
        proactor.handle().read(
            self.fd,
            IoBuf::with_capacity(want),
            offset,
            move |result: IoResult| {
                // The receiver outlives every read in flight.
                let _ = tx.send(ReadDone {
                    id,
                    offset,
                    want,
                    result,
                });
            },
        )?;
        self.pending += 1;
        Ok(())
    }

    /// Takes one completion; a short read is resubmitted for the rest.
    pub fn accept<P: IoPort>(
        &mut self,
        proactor: &Proactor<P>,
        d: ReadDone,
        done: &mpsc::Sender<ReadDone>,
    ) {
        self.pending -= 1;
        if self.error.is_some() {
            return;
        }
        match d.result {
            Ok(t) => {
                let got = t.bytes_transferred as usize;
                if got == 0 {
                    self.error = Some(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the file shrank while it was read",
                    ));
                    return;
                }
                let start = d.offset as usize;
                self.bytes[start..start + got].copy_from_slice(&t.buf.into_vec()[..got]);
                if got < d.want {
                    if let Err(e) =
                        self.submit(proactor, d.id, d.offset + got as u64, d.want - got, done)
                    {
                        self.error = Some(e);
                    }
                }
            }
            Err(e) if is_eof(&e) => {
                self.error = Some(io::Error::new(io::ErrorKind::UnexpectedEof, e));
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// No reads left in flight.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.pending == 0
    }

    /// Releases the file and returns its bytes or the first error.
    ///
    /// # Errors
    /// The first read error.
    pub fn finish<P: IoPort>(self, proactor: &Proactor<P>) -> io::Result<(PathBuf, Vec<u8>)> {
        proactor.handle().release(self.fd);
        drop(self.file);
        match self.error {
            Some(e) => Err(e),
            None => Ok((self.path, self.bytes)),
        }
    }
}

/// Reads one whole file through `proactor`, driving it until done.
///
/// # Errors
/// Opening or reading.
pub fn read_file<P: IoPort>(proactor: &Proactor<P>, path: &Path) -> io::Result<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let mut f = ReadFile::start(proactor, path, 0, &tx)?;
    while !f.finished() {
        proactor.run_once()?;
        while let Ok(d) = rx.try_recv() {
            f.accept(proactor, d, &tx);
        }
    }
    f.finish(proactor).map(|(_, b)| b)
}

/// Writes `bytes` to `path` through `proactor`: to `path` + `.partial` first, every chunk
/// submitted at once, then synced and renamed over `path`, so a reader never sees half a
/// file.
///
/// # Errors
/// Creating, writing, syncing or renaming.
pub fn write_file<P: IoPort>(proactor: &Proactor<P>, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let file = create_write(&partial)?;
    let handle = proactor.handle();
    let fd = handle.register(raw(&file))?;
    let (tx, rx) = mpsc::channel::<(u64, usize, IoResult)>();
    let mut pending = 0usize;
    let mut error: Option<io::Error> = None;
    let submit = |offset: u64, data: &[u8], pending: &mut usize| -> io::Result<()> {
        let tx = tx.clone();
        let want = data.len();
        handle.write(
            fd,
            IoBuf::from_vec(data.to_vec()),
            offset,
            move |r: IoResult| {
                let _ = tx.send((offset, want, r));
            },
        )?;
        *pending += 1;
        Ok(())
    };
    let mut at = 0usize;
    while at < bytes.len() {
        let n = (bytes.len() - at).min(CHUNK);
        if let Err(e) = submit(at as u64, &bytes[at..at + n], &mut pending) {
            error = Some(e);
            break;
        }
        at += n;
    }
    while pending > 0 {
        if let Err(e) = proactor.run_once() {
            error.get_or_insert(e);
            break;
        }
        while let Ok((offset, want, r)) = rx.try_recv() {
            pending -= 1;
            match r {
                Ok(t) => {
                    let got = t.bytes_transferred as usize;
                    if got == 0 {
                        error.get_or_insert(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "a write wrote nothing",
                        ));
                    } else if got < want && error.is_none() {
                        let start = offset as usize + got;
                        if let Err(e) = submit(
                            start as u64,
                            &bytes[start..offset as usize + want],
                            &mut pending,
                        ) {
                            error = Some(e);
                        }
                    }
                }
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
    }
    handle.release(fd);
    if let Some(e) = error {
        drop(file);
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_back_through_the_proactor() {
        let proactor = loadngo_proactor::new_platform_proactor().unwrap();
        let dir = std::env::temp_dir().join(format!("loadngo-anim-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bytes.bin");
        let data: Vec<u8> = (0..(CHUNK * 2 + 12345))
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        write_file(&proactor, &path, &data).unwrap();
        let back = read_file(&proactor, &path).unwrap();
        assert_eq!(back, data);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
