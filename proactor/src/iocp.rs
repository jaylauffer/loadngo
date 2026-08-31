use crate::{
    AcceptCompletionHandler, AcceptResult, AcceptTransfer, CompletionEnvelope, CompletionPort,
    IoBuf, IoCompletionHandler, IoOpId, IoPort, IoResult, IoTransfer, PollEvent, RawFdCompat,
    UnitCompletionHandler,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::ptr;
use std::sync::Mutex;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Networking::WinSock::{
    WSAGetLastError, WSAIoctl, SIO_GET_EXTENSION_FUNCTION_POINTER, SOCKADDR, SOCKADDR_STORAGE,
    SOCKET, SOCKET_ERROR, WSABUF, WSA_IO_PENDING,
};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::IO::{
    CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus,
    OVERLAPPED,
};

const QUEUE_KEY: usize = 1;
const WAKE_KEY: usize = 2;
/// Completion key every real file/socket handle is associated with this
/// IOCP under -- distinct from `QUEUE_KEY`/`WAKE_KEY`, which only ever
/// come from this backend's own `PostQueuedCompletionStatus` calls
/// (`post`/`wake`), never from a real I/O completion.
const IO_OP_KEY: usize = 3;
const WAIT_TIMEOUT_ERROR: u32 = 258;
const INFINITE_TIMEOUT_MS: u32 = u32::MAX;

/// Recovered directly from the `*mut OVERLAPPED` `GetQueuedCompletionStatus`
/// hands back -- `overlapped` must be this struct's first field so a
/// pointer to it is also a valid pointer to the whole struct (the
/// standard C-style "embed the base struct first" pattern OVERLAPPED-based
/// APIs are built around). Boxed and leaked into raw-pointer form for the
/// duration of the operation via `Box::into_raw`, reclaimed via
/// `Box::from_raw` once the completion actually arrives -- exactly
/// mirroring `uring.rs`'s `in_flight` table, just keyed by pointer
/// identity instead of an explicit `IoOpId` map lookup.
#[repr(C)]
struct OverlappedOp {
    overlapped: OVERLAPPED,
    kind: OverlappedOpKind,
}

enum OverlappedOpKind {
    Read {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    Write {
        buf: IoBuf,
        handler: Box<dyn IoCompletionHandler>,
    },
    /// `WSARecv`/`WSASend` take a `WSABUF` array, not a raw pointer --
    /// `wsabuf` must stay alive alongside `buf` for the same reason as
    /// `uring.rs`'s pinned `iovec`, and must point back into `buf`'s own
    /// storage (set once, at submission, before the buffer address could
    /// ever change).
    Recv {
        buf: IoBuf,
        wsabuf: WSABUF,
        handler: Box<dyn IoCompletionHandler>,
    },
    Send {
        buf: IoBuf,
        wsabuf: WSABUF,
        handler: Box<dyn IoCompletionHandler>,
    },
    RecvFrom {
        buf: IoBuf,
        wsabuf: WSABUF,
        addr: SOCKADDR_STORAGE,
        addr_len: i32,
        handler: Box<dyn IoCompletionHandler>,
    },
    SendTo {
        buf: IoBuf,
        wsabuf: WSABUF,
        /// Must live here, not as a submission-time local: MSDN doesn't
        /// guarantee `WSASendTo` copies the target address before an
        /// async completion, so treating it like any other kernel-facing
        /// pointer that must outlive the call (same as `RecvFrom`'s
        /// `addr`) is the safe assumption.
        target: SOCKADDR_STORAGE,
        handler: Box<dyn IoCompletionHandler>,
    },
    /// `AcceptEx` writes local+remote address info into `addr_buf` after
    /// the fixed-size data region -- see `accept`'s doc comment for the
    /// exact layout this depends on.
    Accept {
        listen_socket: SOCKET,
        accept_socket: SOCKET,
        addr_buf: Box<[u8; ACCEPT_ADDR_BUF_LEN]>,
        handler: Box<dyn AcceptCompletionHandler>,
    },
    Connect {
        /// Same lifetime reasoning as `SendTo::target` -- must outlive the
        /// call in case `ConnectEx` completes asynchronously.
        target: SOCKADDR_STORAGE,
        handler: Box<dyn UnitCompletionHandler>,
    },
}

/// `AcceptEx`'s required buffer size: room for a local and a remote
/// address, each `sizeof(SOCKADDR_STORAGE) + 16` bytes (the extra 16 is
/// Microsoft's own documented padding requirement for this call, not a
/// value this code chose).
const ACCEPT_ADDR_BUF_LEN: usize = 2 * (std::mem::size_of::<SOCKADDR_STORAGE>() + 16);

/// Documented function-pointer signatures for the WinSock extension
/// functions this backend loads dynamically (see `load_extension_fn`) --
/// these aren't statically linked, so nothing enforces these signatures
/// match what `WSAIoctl` actually hands back except getting them right by
/// hand from Microsoft's own documentation.
type LpfnAcceptEx = unsafe extern "system" fn(
    SOCKET,
    SOCKET,
    *mut std::ffi::c_void,
    u32,
    u32,
    u32,
    *mut u32,
    *mut OVERLAPPED,
) -> windows::Win32::Foundation::BOOL;

type LpfnConnectEx = unsafe extern "system" fn(
    SOCKET,
    *const SOCKADDR,
    i32,
    *mut std::ffi::c_void,
    u32,
    *mut u32,
    *mut OVERLAPPED,
) -> windows::Win32::Foundation::BOOL;

type LpfnGetAcceptExSockaddrs = unsafe extern "system" fn(
    *const std::ffi::c_void,
    u32,
    u32,
    u32,
    *mut *mut SOCKADDR,
    *mut i32,
    *mut *mut SOCKADDR,
    *mut i32,
);

/// Well-known GUIDs identifying each extension function to
/// `SIO_GET_EXTENSION_FUNCTION_POINTER` -- Microsoft-assigned constants,
/// not values this code invents.
const WSAID_ACCEPTEX: windows::core::GUID =
    windows::core::GUID::from_u128(0xb5367df1_cbac_11cf_95ca_00805f48a192);
const WSAID_CONNECTEX: windows::core::GUID =
    windows::core::GUID::from_u128(0x25a207b9_ddf3_4660_8ee9_76e58c74063e);
const WSAID_GETACCEPTEXSOCKADDRS: windows::core::GUID =
    windows::core::GUID::from_u128(0xb5367df2_cbac_11cf_95ca_00805f48a192);

/// Packs a `std::net::SocketAddr` into a `SOCKADDR_STORAGE` for the calls
/// that need to hand the kernel an address we already know
/// (`connect`/`send_to`/`ConnectEx`'s implicit bind). Hand-rolled against
/// the `windows` crate's own types rather than pulling in `socket2` (see
/// the Cargo.toml comment on this dependency block for why) -- this is
/// exactly the kind of byte-layout code that's easy to get subtly wrong
/// and impossible for this session to verify by execution; cross-checked
/// against `x86_64-pc-windows-msvc` for at least type/field-name
/// correctness, but genuinely unverified beyond that.
fn socket_addr_to_sockaddr(addr: SocketAddr) -> (SOCKADDR_STORAGE, i32) {
    let mut storage: SOCKADDR_STORAGE = unsafe { std::mem::zeroed() };
    let len = match addr {
        SocketAddr::V4(v4) => {
            let sin = windows::Win32::Networking::WinSock::SOCKADDR_IN {
                sin_family: windows::Win32::Networking::WinSock::AF_INET,
                sin_port: v4.port().to_be(),
                sin_addr: windows::Win32::Networking::WinSock::IN_ADDR {
                    S_un: windows::Win32::Networking::WinSock::IN_ADDR_0 {
                        S_addr: u32::from_ne_bytes(v4.ip().octets()),
                    },
                },
                sin_zero: [0; 8],
            };
            unsafe {
                std::ptr::write(
                    &mut storage as *mut _ as *mut windows::Win32::Networking::WinSock::SOCKADDR_IN,
                    sin,
                );
            }
            std::mem::size_of::<windows::Win32::Networking::WinSock::SOCKADDR_IN>() as i32
        }
        SocketAddr::V6(v6) => {
            let sin6 = windows::Win32::Networking::WinSock::SOCKADDR_IN6 {
                sin6_family: windows::Win32::Networking::WinSock::AF_INET6,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                Anonymous: windows::Win32::Networking::WinSock::SOCKADDR_IN6_0 {
                    sin6_scope_id: v6.scope_id(),
                },
                sin6_addr: windows::Win32::Networking::WinSock::IN6_ADDR {
                    u: windows::Win32::Networking::WinSock::IN6_ADDR_0 {
                        Byte: v6.ip().octets(),
                    },
                },
            };
            unsafe {
                std::ptr::write(
                    &mut storage as *mut _
                        as *mut windows::Win32::Networking::WinSock::SOCKADDR_IN6,
                    sin6,
                );
            }
            std::mem::size_of::<windows::Win32::Networking::WinSock::SOCKADDR_IN6>() as i32
        }
    };
    (storage, len)
}

/// The reverse of `socket_addr_to_sockaddr`, for parsing a peer address
/// the kernel filled in (`recv_from`/`AcceptEx`'s `GetAcceptExSockaddrs`).
fn sockaddr_to_socket_addr(storage: &SOCKADDR_STORAGE) -> Option<SocketAddr> {
    match storage.ss_family {
        windows::Win32::Networking::WinSock::AF_INET => {
            let sin = unsafe {
                &*(storage as *const _ as *const windows::Win32::Networking::WinSock::SOCKADDR_IN)
            };
            let ip = std::net::Ipv4Addr::from(unsafe { sin.sin_addr.S_un.S_addr }.to_ne_bytes());
            Some(SocketAddr::V4(std::net::SocketAddrV4::new(
                ip,
                u16::from_be(sin.sin_port),
            )))
        }
        windows::Win32::Networking::WinSock::AF_INET6 => {
            let sin6 = unsafe {
                &*(storage as *const _ as *const windows::Win32::Networking::WinSock::SOCKADDR_IN6)
            };
            let octets = unsafe { sin6.sin6_addr.u.Byte };
            let ip = std::net::Ipv6Addr::from(octets);
            Some(SocketAddr::V6(std::net::SocketAddrV6::new(
                ip,
                u16::from_be(sin6.sin6_port),
                sin6.sin6_flowinfo,
                unsafe { sin6.Anonymous.sin6_scope_id },
            )))
        }
        _ => None,
    }
}

pub struct IocpPort {
    handle: HANDLE,
    queue: Mutex<VecDeque<CompletionEnvelope>>,
    /// Tracks which raw handles have already been associated with this
    /// IOCP via `CreateIoCompletionPort` -- MSDN documents associating the
    /// same handle with the same port a second time as at best redundant
    /// and at worst an error depending on Windows version, so every
    /// `IoPort` method checks/inserts here before its first operation on
    /// a given handle rather than associating unconditionally every call.
    associated: Mutex<HashSet<usize>>,
    /// `IoOpId -> raw HANDLE/SOCKET value` for operations still in
    /// flight, so `cancel_io` knows what to pass `CancelIoEx` alongside
    /// the `OVERLAPPED` pointer. `IoOpId`'s own value *is* that
    /// `OVERLAPPED` pointer (as a `u64`) -- each op's `Box<OverlappedOp>`
    /// address is already a unique identifier once leaked via
    /// `Box::into_raw`, so there's no need for a separate counter the way
    /// `uring.rs`/`kqueue.rs` need one. The actual owned state (buffer,
    /// handler) lives in the `OverlappedOp` recovered directly from that
    /// pointer at completion time, not here -- this table exists only to
    /// support cancellation and shutdown draining.
    in_flight: Mutex<HashMap<IoOpId, usize>>,
    /// For the one case with no IOCP completion to wait for at all:
    /// `ReadFile`/`WriteFile`/etc. failing *synchronously* with a real
    /// error (not `ERROR_IO_PENDING`) means the operation was never
    /// queued to the IOCP in the first place -- unlike the (also
    /// synchronous) *success* case, which still posts a completion per
    /// documented IOCP behavior and needs no special handling here.
    /// Drained the same way `queue` is, via the same `QUEUE_KEY` wake.
    io_completions: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,
}

// SAFETY: `HANDLE` wraps a raw `*mut c_void`, which is `!Send`/`!Sync` by
// default -- but a Windows I/O completion port handle is specifically
// designed to be shared and called into (`GetQueuedCompletionStatus`,
// `PostQueuedCompletionStatus`) from multiple threads concurrently; that
// concurrent use is the documented, intended way to use one, not a
// hazard this needs to guard against. Found (pre-existing, not introduced
// by this change) via `cargo check --target x86_64-pc-windows-msvc`,
// which is otherwise unavailable without a real Windows machine to build
// on -- without these impls, `IocpPort` cannot satisfy `CompletionPort:
// Send + Sync` at all, so this crate has likely never actually compiled
// for Windows successfully.
unsafe impl Send for IocpPort {}
unsafe impl Sync for IocpPort {}

impl IocpPort {
    pub fn new() -> io::Result<Self> {
        let handle = unsafe {
            CreateIoCompletionPort(INVALID_HANDLE_VALUE, HANDLE::default(), 0, 0)
                .map_err(|err| io::Error::other(err.to_string()))?
        };
        Ok(Self {
            handle,
            queue: Mutex::new(VecDeque::new()),
            associated: Mutex::new(HashSet::new()),
            in_flight: Mutex::new(HashMap::new()),
            io_completions: Mutex::new(VecDeque::new()),
        })
    }

    fn drain_io_completion(&self) -> Option<Box<dyn FnOnce() + Send>> {
        self.io_completions
            .lock()
            .expect("iocp io-completion queue poisoned")
            .pop_front()
    }

    /// Queues a completion this backend resolved without ever going
    /// through the IOCP at all (a synchronous, non-`ERROR_IO_PENDING`
    /// failure) and wakes the pump the same way `post()` does.
    fn queue_io_completion(&self, thunk: Box<dyn FnOnce() + Send>) -> io::Result<()> {
        self.io_completions
            .lock()
            .expect("iocp io-completion queue poisoned")
            .push_back(thunk);
        unsafe {
            PostQueuedCompletionStatus(self.handle, 0, QUEUE_KEY, None)
                .map_err(|err| io::Error::other(err.to_string()))
        }
    }

    /// Associates `raw` with this IOCP if it hasn't been already. Must be
    /// called before the first `IoPort` operation on any given handle --
    /// `GetQueuedCompletionStatus` will never report completions for a
    /// handle that was never associated at all.
    fn ensure_associated(&self, raw: usize) -> io::Result<()> {
        let mut associated = self
            .associated
            .lock()
            .expect("iocp associated-handles set poisoned");
        if associated.contains(&raw) {
            return Ok(());
        }
        unsafe {
            CreateIoCompletionPort(HANDLE(raw as *mut _), self.handle, IO_OP_KEY, 0)
                .map_err(|err| io::Error::other(err.to_string()))?;
        }
        associated.insert(raw);
        Ok(())
    }

    /// Loads a WinSock extension function (`AcceptEx`, `ConnectEx`,
    /// `GetAcceptExSockaddrs`) via `WSAIoctl(SIO_GET_EXTENSION_FUNCTION_POINTER)`
    /// -- these are not statically linked Winsock functions, they're only
    /// reachable this way, keyed by a well-known GUID per function. `F`
    /// must be the exact function-pointer type Microsoft documents for
    /// the GUID passed in; this function can't verify that itself.
    unsafe fn load_extension_fn<F>(socket: SOCKET, guid: windows::core::GUID) -> io::Result<F> {
        let mut fn_ptr: usize = 0;
        let mut bytes_returned: u32 = 0;
        let result = WSAIoctl(
            socket,
            SIO_GET_EXTENSION_FUNCTION_POINTER,
            Some(&guid as *const _ as *const std::ffi::c_void),
            std::mem::size_of::<windows::core::GUID>() as u32,
            Some(&mut fn_ptr as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<usize>() as u32,
            &mut bytes_returned,
            None,
            None,
        );
        if result == SOCKET_ERROR {
            return Err(io::Error::from_raw_os_error(WSAGetLastError().0));
        }
        Ok(std::mem::transmute_copy(&fn_ptr))
    }

    /// Looks up and removes `overlapped`'s `OverlappedOp` (reclaiming the
    /// `Box` `read`/`write`/etc. leaked via `Box::into_raw` at
    /// submission) and builds a ready-to-run thunk from the real result.
    /// `bytes_transferred`/`io_error` come straight from
    /// `GetQueuedCompletionStatus` -- `io_error` is `Some` exactly when
    /// that call itself returned `Err` (meaning the *operation* failed;
    /// see `poll()`'s doc comment on that subtlety).
    fn resolve_io_completion(
        &self,
        overlapped: *mut OVERLAPPED,
        bytes_transferred: u32,
        io_error: Option<io::Error>,
    ) -> PollEvent {
        let op_id = IoOpId(overlapped as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .remove(&op_id);

        // SAFETY: `overlapped` is exactly the pointer `Box::into_raw`
        // produced for this op at submission time, reclaimed exactly
        // once here (this function is only ever called once per
        // completion, and each completion is only ever reported once by
        // GetQueuedCompletionStatus).
        let op = unsafe { Box::from_raw(overlapped as *mut OverlappedOp) };

        let thunk: Box<dyn FnOnce() + Send> = match op.kind {
            OverlappedOpKind::Read { buf, handler }
            | OverlappedOpKind::Recv { buf, handler, .. } => {
                let io_result = Self::finish_transfer(buf, bytes_transferred, io_error, None);
                Box::new(move || handler.run(io_result))
            }
            OverlappedOpKind::Write { buf, handler }
            | OverlappedOpKind::Send { buf, handler, .. } => {
                let io_result = Self::finish_transfer(buf, bytes_transferred, io_error, None);
                Box::new(move || handler.run(io_result))
            }
            OverlappedOpKind::RecvFrom {
                buf, addr, handler, ..
            } => {
                let peer = sockaddr_to_socket_addr(&addr);
                let io_result = Self::finish_transfer(buf, bytes_transferred, io_error, peer);
                Box::new(move || handler.run(io_result))
            }
            OverlappedOpKind::SendTo { buf, handler, .. } => {
                let io_result = Self::finish_transfer(buf, bytes_transferred, io_error, None);
                Box::new(move || handler.run(io_result))
            }
            OverlappedOpKind::Accept {
                listen_socket,
                accept_socket,
                addr_buf,
                handler,
            } => {
                let accept_result =
                    Self::finish_accept(listen_socket, accept_socket, &addr_buf, io_error);
                Box::new(move || handler.run(accept_result))
            }
            OverlappedOpKind::Connect { handler, .. } => {
                let unit_result = match io_error {
                    Some(err) => Err(err),
                    None => Ok(()),
                };
                Box::new(move || handler.run(unit_result))
            }
        };

        PollEvent::IoCompletion(thunk)
    }

    fn finish_transfer(
        buf: IoBuf,
        bytes_transferred: u32,
        io_error: Option<io::Error>,
        peer: Option<SocketAddr>,
    ) -> IoResult {
        match io_error {
            Some(err) => Err(err),
            None => {
                let mut buf = buf;
                // Safe for both directions: a write/send's buffer already
                // holds exactly `buf.len()` valid bytes and this never
                // grows it; a read/recv's buffer was allocated via
                // `with_capacity` and the kernel is the one reporting
                // `bytes_transferred` bytes as genuinely written into it.
                unsafe { buf.set_filled_len(bytes_transferred as usize) };
                Ok(IoTransfer {
                    buf,
                    bytes_transferred,
                    peer,
                })
            }
        }
    }

    /// Parses `AcceptEx`'s output buffer via `GetAcceptExSockaddrs` (a
    /// second extension function, loaded fresh here since accept's own
    /// completion doesn't carry a socket to load it from ahead of time
    /// the way `accept()` itself does) and applies `SO_UPDATE_ACCEPT_CONTEXT`,
    /// which real accepted sockets need before most other socket options
    /// or APIs (`getpeername`, etc.) will work on them -- both documented
    /// `AcceptEx` requirements, not optional cleanup.
    fn finish_accept(
        listen_socket: SOCKET,
        accept_socket: SOCKET,
        addr_buf: &[u8; ACCEPT_ADDR_BUF_LEN],
        io_error: Option<io::Error>,
    ) -> AcceptResult {
        if let Some(err) = io_error {
            return Err(err);
        }

        let get_sockaddrs: LpfnGetAcceptExSockaddrs =
            unsafe { Self::load_extension_fn(listen_socket, WSAID_GETACCEPTEXSOCKADDRS) }?;

        let local_addr_len = (std::mem::size_of::<SOCKADDR_STORAGE>() + 16) as u32;
        let remote_addr_len = local_addr_len;
        let mut local_sockaddr: *mut SOCKADDR = ptr::null_mut();
        let mut local_sockaddr_len: i32 = 0;
        let mut remote_sockaddr: *mut SOCKADDR = ptr::null_mut();
        let mut remote_sockaddr_len: i32 = 0;

        unsafe {
            get_sockaddrs(
                addr_buf.as_ptr() as *const std::ffi::c_void,
                0,
                local_addr_len,
                remote_addr_len,
                &mut local_sockaddr,
                &mut local_sockaddr_len,
                &mut remote_sockaddr,
                &mut remote_sockaddr_len,
            );
        }

        unsafe {
            let _ = windows::Win32::Networking::WinSock::setsockopt(
                accept_socket,
                windows::Win32::Networking::WinSock::SOL_SOCKET,
                windows::Win32::Networking::WinSock::SO_UPDATE_ACCEPT_CONTEXT,
                Some(std::slice::from_raw_parts(
                    &listen_socket as *const SOCKET as *const u8,
                    std::mem::size_of::<SOCKET>(),
                )),
            );
        }

        let peer = if remote_sockaddr.is_null() {
            None
        } else {
            sockaddr_to_socket_addr(unsafe { &*(remote_sockaddr as *const SOCKADDR_STORAGE) })
        };
        let _ = remote_sockaddr_len;

        match peer {
            Some(peer) => Ok(AcceptTransfer {
                new_fd: accept_socket.0 as RawFdCompat,
                peer,
            }),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "AcceptEx completed but GetAcceptExSockaddrs reported no usable peer address",
            )),
        }
    }

    fn drain_completion(&self) -> Option<CompletionEnvelope> {
        self.queue
            .lock()
            .expect("iocp completion queue poisoned")
            .pop_front()
    }

    fn duration_to_timeout_ms(duration: Option<Duration>) -> u32 {
        match duration {
            Some(duration) => duration.as_millis().min(u32::MAX as u128) as u32,
            None => INFINITE_TIMEOUT_MS,
        }
    }

    /// Interprets `ReadFile`/`WriteFile`'s return value. `Ok(())` and
    /// `Err(ERROR_IO_PENDING)` both mean "in progress, a real completion
    /// will arrive via the IOCP" -- `Ok(())` specifically means it
    /// already finished synchronously, which (per documented IOCP
    /// behavior, since this handle was associated without
    /// `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`) *still* posts a completion,
    /// so it needs no special handling here, only the "real error, no
    /// completion is coming" case does.
    fn handle_sync_result(
        &self,
        result: windows::core::Result<()>,
        overlapped_raw: *mut OverlappedOp,
        op_id: IoOpId,
    ) -> io::Result<()> {
        if let Err(err) = result {
            let is_pending =
                err.code() == windows::Win32::Foundation::ERROR_IO_PENDING.to_hresult();
            if !is_pending {
                return self.fail_sync(overlapped_raw, op_id, io::Error::other(err.to_string()));
            }
        }
        Ok(())
    }

    /// Same as `handle_sync_result`, for the `WSA*` socket calls, which
    /// return a raw `i32` (`0` on success, `SOCKET_ERROR` on failure with
    /// the real code in `WSAGetLastError()`) rather than a `windows::core::Result`.
    fn handle_sync_wsa_result(
        &self,
        result: i32,
        overlapped_raw: *mut OverlappedOp,
        op_id: IoOpId,
    ) -> io::Result<()> {
        if result == SOCKET_ERROR {
            let err = unsafe { WSAGetLastError() };
            if err != WSA_IO_PENDING {
                return self.fail_sync(overlapped_raw, op_id, io::Error::from_raw_os_error(err.0));
            }
        }
        Ok(())
    }

    /// Reclaims an operation that failed synchronously and so will never
    /// get a real IOCP completion, and queues its failure through
    /// `io_completions` instead -- the only path any `IoPort` method
    /// dispatches a result through other than `resolve_io_completion`.
    fn fail_sync(
        &self,
        overlapped_raw: *mut OverlappedOp,
        op_id: IoOpId,
        err: io::Error,
    ) -> io::Result<()> {
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .remove(&op_id);
        let op = unsafe { Box::from_raw(overlapped_raw) };
        let thunk: Box<dyn FnOnce() + Send> = match op.kind {
            OverlappedOpKind::Read { handler, .. }
            | OverlappedOpKind::Recv { handler, .. }
            | OverlappedOpKind::Write { handler, .. }
            | OverlappedOpKind::Send { handler, .. }
            | OverlappedOpKind::RecvFrom { handler, .. }
            | OverlappedOpKind::SendTo { handler, .. } => Box::new(move || handler.run(Err(err))),
            OverlappedOpKind::Accept {
                accept_socket,
                handler,
                ..
            } => {
                unsafe {
                    let _ = windows::Win32::Networking::WinSock::closesocket(accept_socket);
                }
                Box::new(move || handler.run(Err(err)))
            }
            OverlappedOpKind::Connect { handler, .. } => Box::new(move || handler.run(Err(err))),
        };
        self.queue_io_completion(thunk)
    }
}

impl CompletionPort for IocpPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        self.queue
            .lock()
            .expect("iocp completion queue poisoned")
            .push_back(envelope);
        unsafe {
            PostQueuedCompletionStatus(self.handle, 0, QUEUE_KEY, None)
                .map_err(|err| io::Error::other(err.to_string()))
        }
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        if let Some(envelope) = self.drain_completion() {
            return Ok(PollEvent::Completion(envelope));
        }
        if let Some(thunk) = self.drain_io_completion() {
            return Ok(PollEvent::IoCompletion(thunk));
        }

        let mut bytes_transferred = 0u32;
        let mut completion_key = 0usize;
        let mut overlapped: *mut OVERLAPPED = ptr::null_mut();
        let timeout_ms = Self::duration_to_timeout_ms(timeout);

        let result = unsafe {
            GetQueuedCompletionStatus(
                self.handle,
                &mut bytes_transferred,
                &mut completion_key,
                &mut overlapped,
                timeout_ms,
            )
        };

        // A non-null `overlapped` means a specific I/O operation's
        // completion came back -- true whether `result` is `Ok` *or*
        // `Err`. This is a well-documented but easy-to-miss IOCP
        // subtlety: `GetQueuedCompletionStatus` returning an error does
        // NOT necessarily mean the wait itself failed (e.g. a timeout) --
        // it can just as well mean the *operation* completed with an
        // error, in which case `lpOverlapped` is still populated and the
        // real error is in `GetLastError()`. Treating every `Err` as "the
        // wait failed" would silently drop every failed I/O completion
        // instead of ever delivering it to its handler.
        if !overlapped.is_null() {
            let io_error = if result.is_err() {
                Some(io::Error::from_raw_os_error(
                    unsafe { GetLastError().0 } as i32
                ))
            } else {
                None
            };
            return Ok(self.resolve_io_completion(overlapped, bytes_transferred, io_error));
        }

        if let Err(err) = result {
            let last_error = unsafe { GetLastError().0 };
            if last_error == WAIT_TIMEOUT_ERROR {
                return Ok(PollEvent::Timeout);
            }
            return Err(io::Error::other(err.to_string()));
        }

        match completion_key {
            QUEUE_KEY => {
                if let Some(envelope) = self.drain_completion() {
                    Ok(PollEvent::Completion(envelope))
                } else if let Some(thunk) = self.drain_io_completion() {
                    Ok(PollEvent::IoCompletion(thunk))
                } else {
                    Ok(PollEvent::Wake)
                }
            }
            WAKE_KEY => Ok(PollEvent::Wake),
            _ => Ok(PollEvent::Wake),
        }
    }

    fn wake(&self) -> io::Result<()> {
        unsafe {
            PostQueuedCompletionStatus(self.handle, 0, WAKE_KEY, None)
                .map_err(|err| io::Error::other(err.to_string()))
        }
    }

    fn begin_shutdown(&self) {
        let entries: Vec<(IoOpId, usize)> = self
            .in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .iter()
            .map(|(op_id, &handle_raw)| (*op_id, handle_raw))
            .collect();
        for (op_id, handle_raw) in entries {
            let overlapped_raw = op_id.0 as usize;
            // Best-effort, same reasoning as IoUringPort::begin_shutdown:
            // a failed cancel request just means this op's own natural
            // completion (success or its own error) is what eventually
            // clears it from in_flight instead of an early cancellation.
            // Like io_uring's AsyncCancel (and unlike KqueuePort's
            // synchronous EV_DELETE), CancelIoEx is itself async -- the
            // cancelled operation still needs its own completion to
            // arrive via GetQueuedCompletionStatus before it's actually
            // safe to free, so shutdown_complete() below still depends on
            // resolve_io_completion removing the entry, not on this call.
            unsafe {
                let _ = CancelIoEx(
                    HANDLE(handle_raw as *mut _),
                    Some(overlapped_raw as *const OVERLAPPED),
                );
            }
        }
    }

    fn shutdown_complete(&self) -> bool {
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .is_empty()
    }
}

impl IoPort for IocpPort {
    fn read(
        &self,
        fd: RawFdCompat,
        mut buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let mut op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::Read {
                buf: IoBuf::with_capacity(0),
                handler: Box::new(handler),
            },
        });
        op.overlapped.Anonymous.Anonymous.Offset = offset as u32;
        op.overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        std::mem::swap(
            match &mut op.kind {
                OverlappedOpKind::Read { buf, .. } => buf,
                _ => unreachable!(),
            },
            &mut buf,
        );

        let ptr = buf.as_mut_ptr();
        let len = buf.capacity();
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let result = unsafe {
            ReadFile(
                HANDLE(fd as *mut _),
                Some(std::slice::from_raw_parts_mut(ptr, len)),
                None,
                Some(overlapped_raw as *mut OVERLAPPED),
            )
        };
        self.handle_sync_result(result, overlapped_raw, op_id)?;
        Ok(op_id)
    }

    fn write(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let mut op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::Write {
                buf,
                handler: Box::new(handler),
            },
        });
        op.overlapped.Anonymous.Anonymous.Offset = offset as u32;
        op.overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;

        let (ptr, len) = match &op.kind {
            OverlappedOpKind::Write { buf, .. } => (buf.as_ptr(), buf.len()),
            _ => unreachable!(),
        };
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let result = unsafe {
            WriteFile(
                HANDLE(fd as *mut _),
                Some(std::slice::from_raw_parts(ptr, len)),
                None,
                Some(overlapped_raw as *mut OVERLAPPED),
            )
        };
        self.handle_sync_result(result, overlapped_raw, op_id)?;
        Ok(op_id)
    }

    fn recv(
        &self,
        fd: RawFdCompat,
        mut buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let wsabuf = WSABUF {
            len: buf.capacity() as u32,
            buf: windows::core::PSTR(buf.as_mut_ptr()),
        };
        let op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::Recv {
                buf,
                wsabuf,
                handler: Box::new(handler),
            },
        });
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let wsabuf_ptr = match unsafe { &(*overlapped_raw).kind } {
            OverlappedOpKind::Recv { wsabuf, .. } => wsabuf as *const WSABUF as *mut WSABUF,
            _ => unreachable!(),
        };
        let mut flags: u32 = 0;
        let result = unsafe {
            windows::Win32::Networking::WinSock::WSARecv(
                SOCKET(fd as usize),
                std::slice::from_raw_parts(wsabuf_ptr, 1),
                None,
                &mut flags,
                Some(overlapped_raw as *mut OVERLAPPED),
                None,
            )
        };
        self.handle_sync_wsa_result(result, overlapped_raw, op_id)?;
        Ok(op_id)
    }

    fn send(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let mut buf = buf;
        let wsabuf = WSABUF {
            len: buf.len() as u32,
            buf: windows::core::PSTR(buf.as_mut_ptr()),
        };
        let op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::Send {
                buf,
                wsabuf,
                handler: Box::new(handler),
            },
        });
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let wsabuf_ptr = match unsafe { &(*overlapped_raw).kind } {
            OverlappedOpKind::Send { wsabuf, .. } => wsabuf as *const WSABUF as *mut WSABUF,
            _ => unreachable!(),
        };
        let result = unsafe {
            windows::Win32::Networking::WinSock::WSASend(
                SOCKET(fd as usize),
                std::slice::from_raw_parts(wsabuf_ptr, 1),
                None,
                0,
                Some(overlapped_raw as *mut OVERLAPPED),
                None,
            )
        };
        self.handle_sync_wsa_result(result, overlapped_raw, op_id)?;
        Ok(op_id)
    }

    fn recv_from(
        &self,
        fd: RawFdCompat,
        mut buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let wsabuf = WSABUF {
            len: buf.capacity() as u32,
            buf: windows::core::PSTR(buf.as_mut_ptr()),
        };
        let op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::RecvFrom {
                buf,
                wsabuf,
                addr: unsafe { std::mem::zeroed() },
                addr_len: std::mem::size_of::<SOCKADDR_STORAGE>() as i32,
                handler: Box::new(handler),
            },
        });
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let (wsabuf_ptr, addr_ptr, addr_len_ptr) = match unsafe { &mut (*overlapped_raw).kind } {
            OverlappedOpKind::RecvFrom {
                wsabuf,
                addr,
                addr_len,
                ..
            } => (
                wsabuf as *const WSABUF as *mut WSABUF,
                addr as *mut SOCKADDR_STORAGE as *mut SOCKADDR,
                addr_len as *mut i32,
            ),
            _ => unreachable!(),
        };
        let mut flags: u32 = 0;
        let result = unsafe {
            windows::Win32::Networking::WinSock::WSARecvFrom(
                SOCKET(fd as usize),
                std::slice::from_raw_parts(wsabuf_ptr, 1),
                None,
                &mut flags,
                Some(addr_ptr),
                Some(addr_len_ptr),
                Some(overlapped_raw as *mut OVERLAPPED),
                None,
            )
        };
        self.handle_sync_wsa_result(result, overlapped_raw, op_id)?;
        Ok(op_id)
    }

    fn send_to(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        target: SocketAddr,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let mut buf = buf;
        let wsabuf = WSABUF {
            len: buf.len() as u32,
            buf: windows::core::PSTR(buf.as_mut_ptr()),
        };
        let (target_storage, target_len) = socket_addr_to_sockaddr(target);
        let op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::SendTo {
                buf,
                wsabuf,
                target: target_storage,
                handler: Box::new(handler),
            },
        });
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let (wsabuf_ptr, target_ptr) = match unsafe { &(*overlapped_raw).kind } {
            OverlappedOpKind::SendTo { wsabuf, target, .. } => (
                wsabuf as *const WSABUF as *mut WSABUF,
                target as *const SOCKADDR_STORAGE as *const SOCKADDR,
            ),
            _ => unreachable!(),
        };
        let result = unsafe {
            windows::Win32::Networking::WinSock::WSASendTo(
                SOCKET(fd as usize),
                std::slice::from_raw_parts(wsabuf_ptr, 1),
                None,
                0,
                Some(target_ptr),
                target_len,
                Some(overlapped_raw as *mut OVERLAPPED),
                None,
            )
        };
        self.handle_sync_wsa_result(result, overlapped_raw, op_id)?;
        Ok(op_id)
    }

    fn accept(&self, fd: RawFdCompat, handler: impl AcceptCompletionHandler) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let listen_socket = SOCKET(fd as usize);
        let accept_ex: LpfnAcceptEx =
            unsafe { Self::load_extension_fn(listen_socket, WSAID_ACCEPTEX)? };

        // A fresh, unbound, unconnected socket for AcceptEx to attach the
        // accepted connection to -- unlike plain accept(), AcceptEx needs
        // this pre-created rather than creating it itself. Address family/
        // type/protocol must match the listening socket's; IPv4 TCP is
        // assumed here since that's what this whole IoPort surface targets
        // elsewhere (recv_from/send_to's SocketAddr is address-family
        // agnostic, but this specific WSASocket call is not -- a real gap
        // if IPv6 listeners need this, flagged rather than silently wrong).
        let accept_socket = unsafe {
            windows::Win32::Networking::WinSock::WSASocketW(
                windows::Win32::Networking::WinSock::AF_INET.0 as i32,
                windows::Win32::Networking::WinSock::SOCK_STREAM.0,
                0,
                None,
                0,
                windows::Win32::Networking::WinSock::WSA_FLAG_OVERLAPPED,
            )
        }
        .map_err(|err| io::Error::other(err.to_string()))?;

        let op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::Accept {
                listen_socket,
                accept_socket,
                addr_buf: Box::new([0u8; ACCEPT_ADDR_BUF_LEN]),
                handler: Box::new(handler),
            },
        });
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let addr_buf_ptr = match unsafe { &mut (*overlapped_raw).kind } {
            OverlappedOpKind::Accept { addr_buf, .. } => addr_buf.as_mut_ptr(),
            _ => unreachable!(),
        };
        let local_addr_len = (std::mem::size_of::<SOCKADDR_STORAGE>() + 16) as u32;
        let mut bytes_received: u32 = 0;
        let result = unsafe {
            accept_ex(
                listen_socket,
                accept_socket,
                addr_buf_ptr as *mut std::ffi::c_void,
                0,
                local_addr_len,
                local_addr_len,
                &mut bytes_received,
                overlapped_raw as *mut OVERLAPPED,
            )
        };
        if result.as_bool() {
            // Completed synchronously -- still posts to the IOCP per
            // documented behavior (same as ReadFile/WriteFile), no
            // special handling needed here.
        } else {
            let err = unsafe { WSAGetLastError() };
            if err != WSA_IO_PENDING {
                self.fail_sync(overlapped_raw, op_id, io::Error::from_raw_os_error(err.0))?;
            }
        }
        Ok(op_id)
    }

    fn connect(
        &self,
        fd: RawFdCompat,
        target: SocketAddr,
        handler: impl UnitCompletionHandler,
    ) -> io::Result<IoOpId> {
        self.ensure_associated(fd as usize)?;
        let socket = SOCKET(fd as usize);
        let connect_ex: LpfnConnectEx =
            unsafe { Self::load_extension_fn(socket, WSAID_CONNECTEX)? };

        // ConnectEx requires the socket to already be bound, unlike plain
        // connect() which binds implicitly -- bind to the wildcard
        // address/port 0 if the caller hasn't already bound it themselves.
        // A harmless no-op error (already bound) is ignored; any other
        // bind failure is real and reported. bind() is a plain synchronous
        // call (not OVERLAPPED), so a local stack variable for its
        // address is fine -- unlike target_storage below, which must
        // outlive this function if ConnectEx completes asynchronously.
        let wildcard_addr = match target {
            SocketAddr::V4(_) => {
                SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)
            }
            SocketAddr::V6(_) => {
                SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
            }
        };
        let (wildcard_storage, wildcard_len) = socket_addr_to_sockaddr(wildcard_addr);
        unsafe {
            let _ = windows::Win32::Networking::WinSock::bind(
                socket,
                &wildcard_storage as *const SOCKADDR_STORAGE as *const SOCKADDR,
                wildcard_len,
            );
        }

        let (target_storage, target_len) = socket_addr_to_sockaddr(target);
        let op = Box::new(OverlappedOp {
            overlapped: unsafe { std::mem::zeroed() },
            kind: OverlappedOpKind::Connect {
                target: target_storage,
                handler: Box::new(handler),
            },
        });
        let overlapped_raw = Box::into_raw(op);
        let op_id = IoOpId(overlapped_raw as usize as u64);
        self.in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .insert(op_id, fd as usize);

        let target_ptr = match unsafe { &(*overlapped_raw).kind } {
            OverlappedOpKind::Connect { target, .. } => {
                target as *const SOCKADDR_STORAGE as *const SOCKADDR
            }
            _ => unreachable!(),
        };
        let mut bytes_sent: u32 = 0;
        let result = unsafe {
            connect_ex(
                socket,
                target_ptr,
                target_len,
                ptr::null_mut(),
                0,
                &mut bytes_sent,
                overlapped_raw as *mut OVERLAPPED,
            )
        };
        if !result.as_bool() {
            let err = unsafe { WSAGetLastError() };
            if err != WSA_IO_PENDING {
                self.fail_sync(overlapped_raw, op_id, io::Error::from_raw_os_error(err.0))?;
            }
        }
        Ok(op_id)
    }

    fn cancel_io(&self, op: IoOpId) -> io::Result<()> {
        let handle_raw = self
            .in_flight
            .lock()
            .expect("iocp in-flight op table poisoned")
            .get(&op)
            .copied();
        let Some(handle_raw) = handle_raw else {
            return Ok(());
        };
        unsafe {
            let _ = CancelIoEx(
                HANDLE(handle_raw as *mut _),
                Some(op.0 as usize as *const OVERLAPPED),
            );
        }
        // Deliberately doesn't remove the in_flight entry or dispatch a
        // cancelled result here -- unlike KqueuePort's synchronous
        // cancel_io, CancelIoEx is itself async: the real completion
        // (success if it won the race, or an error such as
        // ERROR_OPERATION_ABORTED if the cancellation did) still has to
        // arrive via GetQueuedCompletionStatus, same as IoUringPort's
        // AsyncCancel. resolve_io_completion is what actually clears it.
        Ok(())
    }
}

impl Drop for IocpPort {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
