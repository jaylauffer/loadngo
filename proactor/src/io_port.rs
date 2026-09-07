//! Real, kernel-driven asynchronous I/O -- true proactor semantics, as
//! opposed to `ReadinessPort`'s "notify when ready, caller still does the
//! syscall" reactor semantics. See the design discussion this grew out of
//! for why the two are genuinely different and why both exist side by
//! side rather than one replacing the other.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

/// A raw OS handle to operate on: `RawFd` on Unix, `RawSocket` on
/// Windows. Kept as a small platform-neutral alias rather than exposing
/// `std::os::fd::RawFd` directly in this trait's signatures, since that
/// type doesn't exist on Windows at all.
#[cfg(unix)]
pub type RawFdCompat = std::os::fd::RawFd;
#[cfg(windows)]
pub type RawFdCompat = std::os::windows::io::RawSocket;

/// An owned, fixed-address buffer loaned to an in-flight `IoPort`
/// operation. Deliberately not `Clone`: ownership transfers to the
/// operation and only comes back via its completion handler, so nothing
/// on the Rust side can ever alias memory the kernel might still be
/// writing into. Backends access the raw pointer/length via
/// `pub(crate)` methods only -- application code never needs to.
pub struct IoBuf {
    inner: Vec<u8>,
}

impl IoBuf {
    /// A zero-filled buffer of exactly `cap` bytes, sized for a read of
    /// up to that many bytes.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            inner: vec![0u8; cap],
        }
    }

    /// Wraps an existing `Vec<u8>` -- typically for a write/send, where
    /// the buffer already holds the data to transfer.
    pub fn from_vec(inner: Vec<u8>) -> Self {
        Self { inner }
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.inner
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.inner.as_mut_ptr()
    }

    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.inner.as_ptr()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.inner.len()
    }

    /// Truncates the logical length to `n` bytes, for a read/recv backend
    /// to call once it knows how many bytes the kernel actually wrote.
    ///
    /// # Safety
    /// Caller must guarantee `n <= self.capacity()` and that the kernel
    /// (or equivalent) actually initialized `self.inner[..n]`.
    pub(crate) unsafe fn set_filled_len(&mut self, n: usize) {
        debug_assert!(n <= self.inner.len());
        self.inner.set_len(n);
    }
}

/// The outcome of a completed read/write/recv/send/recv_from/send_to:
/// the buffer handed back (for a read/recv, truncated to the bytes
/// actually transferred; for a write/send, unchanged), the transfer
/// count, and -- only for `recv_from`, which discovers who sent the
/// data -- the peer address.
pub struct IoTransfer {
    pub buf: IoBuf,
    pub bytes_transferred: u32,
    pub peer: Option<SocketAddr>,
}

pub type IoResult = io::Result<IoTransfer>;

pub trait IoCompletionHandler: Send + 'static {
    fn run(self: Box<Self>, result: IoResult);
}

impl<F> IoCompletionHandler for F
where
    F: FnOnce(IoResult) + Send + 'static,
{
    fn run(self: Box<Self>, result: IoResult) {
        (self)(result)
    }
}

/// Who connected, across every address family `accept` can land on.
///
/// Deliberately not `Option<SocketAddr>`, which was the obvious shape and
/// is wrong for three reasons:
///
/// - the idiomatic `if let Some(peer) = ... { ...check peer... }` skips
///   the check entirely when the peer is `None`, so an access-control
///   test written the natural way fails *open*. Matching an enum makes
///   that mistake visible instead;
/// - `None` would conflate "an `AF_UNIX` peer, which is completely
///   normal" with "a sockaddr we could not parse, which is a fault".
///   Those want opposite responses, and collapsing them is exactly what
///   hid the descriptor leak this type replaced;
/// - a Unix peer's meaningful identity is not an address at all --
///   unnamed and abstract sockets have no path -- so `None` would encode
///   *absence* where the truth is *a different kind of identity*. Keeping
///   a `Unix` variant leaves room to carry `SO_PEERCRED` later without
///   another breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerAddr {
    /// An IPv4 or IPv6 peer.
    Ip(SocketAddr),
    /// A Unix-domain peer.
    ///
    /// `path` is `None` both for an unnamed socket -- a client that never
    /// called `bind`, which is the overwhelmingly common case for a
    /// connecting client -- and for a Linux abstract-namespace socket,
    /// neither of which has a filesystem path. `None` here means "this
    /// Unix peer has no path", never "we failed to work out who this is";
    /// that second case is `Unknown`, or an `Err` from the accept itself.
    Unix { path: Option<PathBuf> },
    /// An address family this crate does not decode. `family` is the raw
    /// `sa_family_t` (`ADDRESS_FAMILY` on Windows) exactly as the kernel
    /// reported it.
    ///
    /// Treat this as "identity unknown". It is deliberately not silently
    /// merged with any other variant, so a caller gating on peer identity
    /// has to decide what to do about it rather than falling through.
    Unknown { family: u16 },
}

impl PeerAddr {
    /// The IP peer, if this is one. Convenience for the common case;
    /// note that a `None` here is not a reason to skip an access-control
    /// check -- see this type's own documentation.
    pub fn as_ip(&self) -> Option<SocketAddr> {
        match self {
            PeerAddr::Ip(addr) => Some(*addr),
            _ => None,
        }
    }
}

/// Decodes a kernel-filled `sockaddr_storage` into a [`PeerAddr`].
///
/// Shared by every Unix backend (`uring`, `kqueue`, `epoll`) because the
/// decoding is identical on all three and the previous per-backend copies
/// each carried the same descriptor-leaking bug.
///
/// `AF_UNIX` is checked *before* handing the storage to `socket2`, which
/// only decodes IP families and would otherwise report a perfectly normal
/// Unix peer as undecodable.
#[cfg(unix)]
pub(crate) fn peer_addr_from_storage(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> PeerAddr {
    let family = storage.ss_family as u16;

    if family == libc::AF_UNIX as u16 {
        return PeerAddr::Unix {
            path: unix_path_from_storage(storage, len),
        };
    }

    match unsafe { socket2::SockAddr::new(*storage, len) }.as_socket() {
        Some(addr) => PeerAddr::Ip(addr),
        None => PeerAddr::Unknown { family },
    }
}

/// Extracts the filesystem path from an `AF_UNIX` `sockaddr_un`, or
/// `None` when there isn't one.
///
/// Two distinct cases produce `None`, and both are normal rather than
/// errors:
///
/// - `len` no larger than the family field: an *unnamed* socket. This is
///   the usual case for an accepted peer, since a connecting client
///   rarely calls `bind`.
/// - a leading NUL in `sun_path`: Linux's abstract namespace, which has a
///   name but not a filesystem path.
#[cfg(unix)]
fn unix_path_from_storage(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let family_len = std::mem::size_of::<libc::sa_family_t>() as libc::socklen_t;
    if len <= family_len {
        return None;
    }

    // SAFETY: the family field was just checked to be AF_UNIX by the
    // caller, so the storage really is a `sockaddr_un`, and
    // `sockaddr_storage` is defined to be large enough and suitably
    // aligned for any address family.
    let sun = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_un>() };

    let path_len = (len - family_len) as usize;
    let raw = &sun.sun_path[..path_len.min(sun.sun_path.len())];
    if raw.first().is_none_or(|first| *first == 0) {
        return None;
    }

    let bytes: Vec<u8> = raw
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    if bytes.is_empty() {
        return None;
    }

    Some(PathBuf::from(OsString::from_vec(bytes)))
}

/// The outcome of a completed `accept`: a new connection's raw handle
/// plus the peer that connected. Distinct from `IoTransfer` because
/// accepting a connection doesn't transfer any buffer data at all -- its
/// payload is a new fd, not bytes.
pub struct AcceptTransfer {
    pub new_fd: RawFdCompat,
    pub peer: PeerAddr,
}

pub type AcceptResult = io::Result<AcceptTransfer>;

pub trait AcceptCompletionHandler: Send + 'static {
    fn run(self: Box<Self>, result: AcceptResult);
}

impl<F> AcceptCompletionHandler for F
where
    F: FnOnce(AcceptResult) + Send + 'static,
{
    fn run(self: Box<Self>, result: AcceptResult) {
        (self)(result)
    }
}

/// A completion carrying no payload at all beyond success/failure --
/// `connect`'s only outcome, since the caller already knows the target
/// address it asked to connect to.
pub trait UnitCompletionHandler: Send + 'static {
    fn run(self: Box<Self>, result: io::Result<()>);
}

impl<F> UnitCompletionHandler for F
where
    F: FnOnce(io::Result<()>) + Send + 'static,
{
    fn run(self: Box<Self>, result: io::Result<()>) {
        (self)(result)
    }
}

/// Names one in-flight `IoPort` operation, returned so callers can
/// request its cancellation via `IoPort::cancel_io`. Distinct from a
/// `ReadinessPort` token, which names a standing registration rather
/// than a single in-flight operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IoOpId(pub(crate) u64);

/// Real, kernel-driven asynchronous I/O. Every method that takes a `buf`
/// takes ownership of it and returns it only through the handler's
/// eventual call -- there is deliberately no way to reclaim it
/// synchronously or to cancel and get it back instantly, because the
/// kernel may genuinely still be writing into it. `cancel_io` only
/// *requests* cancellation; the original handler still fires, either
/// with the real result (if it won the race) or a cancelled error.
///
/// A `CompletionPort` that implements this should also override
/// `begin_shutdown`/`shutdown_complete` so `Proactor::run_until_stopped`
/// can safely drain every outstanding operation before returning --
/// never drop an `IoPort` implementation while operations may still be
/// in flight.
pub trait IoPort: super::CompletionPort {
    fn read(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId>;

    fn write(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId>;

    fn recv(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId>;

    fn recv_from(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId>;

    fn send(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId>;

    fn send_to(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        target: SocketAddr,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId>;

    fn accept(&self, fd: RawFdCompat, handler: impl AcceptCompletionHandler) -> io::Result<IoOpId>;

    fn connect(
        &self,
        fd: RawFdCompat,
        target: SocketAddr,
        handler: impl UnitCompletionHandler,
    ) -> io::Result<IoOpId>;

    fn cancel_io(&self, op: IoOpId) -> io::Result<()>;
}
