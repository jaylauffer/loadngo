//! The port 64-bit Linux uses: io_uring when the kernel lets this process
//! set one up, epoll otherwise.
//!
//! io_uring is missing in ordinary places: qemu-user has no `io_uring_setup`
//! (ENOSYS; CI runs aarch64 and riscv64 binaries that way), Docker's default
//! seccomp profile refuses it (EPERM), `kernel.io_uring_disabled` refuses it,
//! and before Linux 5.12 the rings count against `RLIMIT_MEMLOCK`, which is
//! 64 KiB by default (ENOMEM). Code that names [`PlatformPort`](crate::PlatformPort)
//! should still run there, so the choice is made once, when the port is
//! built, and every call goes to the backend chosen.

use crate::{
    AcceptCompletionHandler, CompletionEnvelope, CompletionPort, EpollPort, IoBuf,
    IoCompletionHandler, IoOpId, IoPort, IoUringPort, PollEvent, RawFdCompat, ReadinessPort,
    UnitCompletionHandler,
};
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::time::Duration;

// One per proactor, kept behind its Arc: the unused bytes of the smaller
// variant cost nothing, and boxing would add an indirection to every call.
#[allow(clippy::large_enum_variant)]
pub enum LinuxPort {
    Uring(IoUringPort),
    Epoll(EpollPort),
}

impl LinuxPort {
    /// io_uring if it can be set up, epoll if not. Fails only when both do.
    pub fn new() -> io::Result<Self> {
        Self::choose(IoUringPort::new(), EpollPort::new)
    }

    fn choose(
        uring: io::Result<IoUringPort>,
        epoll: impl FnOnce() -> io::Result<EpollPort>,
    ) -> io::Result<Self> {
        match uring {
            Ok(port) => Ok(Self::Uring(port)),
            Err(uring) => epoll().map(Self::Epoll).map_err(|epoll| {
                io::Error::new(epoll.kind(), format!("io_uring: {uring}; epoll: {epoll}"))
            }),
        }
    }
}

macro_rules! each {
    ($self:expr, $port:ident => $call:expr) => {
        match $self {
            LinuxPort::Uring($port) => $call,
            LinuxPort::Epoll($port) => $call,
        }
    };
}

impl CompletionPort for LinuxPort {
    fn post(&self, envelope: CompletionEnvelope) -> io::Result<()> {
        each!(self, port => port.post(envelope))
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<PollEvent> {
        each!(self, port => port.poll(timeout))
    }

    fn wake(&self) -> io::Result<()> {
        each!(self, port => port.wake())
    }

    fn begin_shutdown(&self) {
        each!(self, port => port.begin_shutdown())
    }

    fn shutdown_complete(&self) -> bool {
        each!(self, port => port.shutdown_complete())
    }
}

impl ReadinessPort for LinuxPort {
    fn register_readable(&self, fd: RawFd, token: u64) -> io::Result<()> {
        each!(self, port => port.register_readable(fd, token))
    }

    fn deregister(&self, fd: RawFd) -> io::Result<()> {
        each!(self, port => port.deregister(fd))
    }
}

impl IoPort for LinuxPort {
    fn register(&self, fd: RawFdCompat) -> io::Result<RawFdCompat> {
        each!(self, port => port.register(fd))
    }

    fn release(&self, fd: RawFdCompat) {
        each!(self, port => port.release(fd))
    }

    fn read(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        each!(self, port => port.read(fd, buf, offset, handler))
    }

    fn write(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        offset: u64,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        each!(self, port => port.write(fd, buf, offset, handler))
    }

    fn recv(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        each!(self, port => port.recv(fd, buf, handler))
    }

    fn recv_from(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        each!(self, port => port.recv_from(fd, buf, handler))
    }

    fn send(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        each!(self, port => port.send(fd, buf, handler))
    }

    fn send_to(
        &self,
        fd: RawFdCompat,
        buf: IoBuf,
        target: SocketAddr,
        handler: impl IoCompletionHandler,
    ) -> io::Result<IoOpId> {
        each!(self, port => port.send_to(fd, buf, target, handler))
    }

    fn accept(&self, fd: RawFdCompat, handler: impl AcceptCompletionHandler) -> io::Result<IoOpId> {
        each!(self, port => port.accept(fd, handler))
    }

    fn connect(
        &self,
        fd: RawFdCompat,
        target: SocketAddr,
        handler: impl UnitCompletionHandler,
    ) -> io::Result<IoOpId> {
        each!(self, port => port.connect(fd, target, handler))
    }

    fn cancel_io(&self, op: IoOpId) -> io::Result<()> {
        each!(self, port => port.cancel_io(op))
    }
}

#[cfg(test)]
mod tests {
    use super::LinuxPort;
    use crate::{Completion, CompletionKind, IoBuf, IoResult, Proactor};
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::fd::AsRawFd;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn unavailable() -> io::Result<crate::IoUringPort> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    #[test]
    fn a_kernel_without_io_uring_gets_a_working_epoll_port() {
        let port = LinuxPort::choose(unavailable(), crate::EpollPort::new).unwrap();
        assert!(matches!(port, LinuxPort::Epoll(_)));

        let proactor = Proactor::new(port);
        let (tx, rx) = mpsc::channel();
        proactor
            .handle()
            .enqueue_work(move |completion: Completion| tx.send(completion.kind).unwrap())
            .unwrap();
        assert_eq!(proactor.run_once().unwrap().dispatched_completions, 1);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            CompletionKind::Job
        );
    }

    #[test]
    fn the_epoll_fallback_reads_regular_files() {
        let path = std::env::temp_dir().join(format!(
            "loadngo-proactor-linux-port-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        File::create(&path).unwrap().write_all(b"fallback").unwrap();
        let file = File::open(&path).unwrap();

        let proactor =
            Proactor::new(LinuxPort::choose(unavailable(), crate::EpollPort::new).unwrap());
        let (tx, rx) = mpsc::channel();
        proactor
            .handle()
            .read(
                file.as_raw_fd(),
                IoBuf::with_capacity(16),
                0,
                move |result: IoResult| {
                    let transfer = result.unwrap();
                    tx.send((transfer.bytes_transferred as usize, transfer.buf.into_vec()))
                        .unwrap();
                },
            )
            .unwrap();
        let start = Instant::now();
        while proactor.run_ready().unwrap().dispatched_completions == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "read never completed"
            );
        }
        let (n, buf) = rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(&buf[..n], b"fallback");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn when_both_backends_fail_the_error_names_both() {
        let error = LinuxPort::choose(unavailable(), || {
            Err(io::Error::from_raw_os_error(libc::EMFILE))
        })
        .err()
        .unwrap();
        let text = error.to_string();
        assert!(text.starts_with("io_uring: "), "{text}");
        assert!(text.contains("; epoll: "), "{text}");
    }

    #[test]
    fn a_kernel_with_io_uring_or_without_builds_a_port() {
        LinuxPort::new().unwrap();
    }
}
