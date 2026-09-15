#![cfg(windows)]

//! `IocpPort` against real Windows handles and sockets.
//!
//! The Unix backends each carry a suite like this (`kqueue.rs`, `uring.rs`,
//! `epoll.rs`); until 2026-09-15 this file had only the two queue/wake tests,
//! so every `IoPort` operation below shipped without ever running. Several
//! tests here pin a specific defect found by reading the backend -- see each
//! test's comment.

use loadngo_proactor::{
    AcceptResult, Completion, CompletionKind, IoBuf, IoResult, IocpPort, PeerAddr, Proactor,
};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, AsRawSocket, FromRawSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// `FILE_FLAG_OVERLAPPED`. IOCP only queues completions for handles opened
/// with it; a plain `std::fs::File` would complete synchronously and never
/// post one.
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

/// `ERROR_OPERATION_ABORTED`, what a cancelled overlapped operation completes
/// with.
const ERROR_OPERATION_ABORTED: i32 = 995;

fn run_until_dispatched(proactor: &Proactor<IocpPort>, count: usize, what: &str) {
    let start = Instant::now();
    let mut dispatched = 0;
    while dispatched < count {
        dispatched += proactor.run_ready().unwrap().dispatched_completions;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{what} never completed"
        );
    }
}

#[test]
fn iocp_dispatches_enqueued_work() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();

    handle
        .enqueue_work(move |completion: Completion| {
            tx.send((completion.kind, completion.bytes_transferred))
                .unwrap();
        })
        .unwrap();

    let report = proactor.run_once().unwrap();
    assert_eq!(report.dispatched_completions, 1);
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(100)).unwrap(),
        (CompletionKind::Job, 0)
    );
}

#[test]
fn iocp_dispatches_burst_enqueued_work_in_order() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();

    for value in [1u8, 2, 3] {
        let tx = tx.clone();
        handle
            .enqueue_work(move |_completion: Completion| {
                tx.send(value).unwrap();
            })
            .unwrap();
    }
    drop(tx);

    run_until_dispatched(&proactor, 3, "burst of enqueued work");
    for expected in [1u8, 2, 3] {
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(100)).unwrap(),
            expected
        );
    }
}

#[test]
fn iocp_wake_interrupts_blocking_poll() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let worker = thread::spawn(move || {
        let started = Instant::now();
        let report = proactor.run_once().unwrap();
        (started.elapsed(), report)
    });

    thread::sleep(Duration::from_millis(25));
    handle.wake().unwrap();

    let (elapsed, report) = worker.join().unwrap();
    assert!(elapsed < Duration::from_secs(1));
    assert!(report.woke);
}

#[test]
fn iocp_stop_wakes_and_ends_loop() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let worker = thread::spawn(move || {
        let started = Instant::now();
        proactor.run_until_stopped().unwrap();
        started.elapsed()
    });

    thread::sleep(Duration::from_millis(25));
    handle.stop().unwrap();

    let elapsed = worker.join().unwrap();
    assert!(elapsed < Duration::from_secs(1));
    assert!(!handle.is_running());
}

#[test]
fn iocp_write_then_read_round_trip_an_overlapped_file() {
    // Regression: `read` handed `ReadFile` the zero-capacity placeholder it
    // had swapped out of its op instead of the caller's buffer, so every
    // read completed having read nothing. The offset read also checks
    // `OVERLAPPED.Offset` reaches the kernel.
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let path = std::env::temp_dir().join(format!(
        "loadngo-proactor-iocp-test-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(&path)
        .unwrap();
    let fd = file.as_raw_handle() as usize as u64;

    let (write_tx, write_rx) = mpsc::channel();
    handle
        .write(
            fd,
            IoBuf::from_vec(b"hello iocp".to_vec()),
            0,
            move |result: IoResult| {
                write_tx.send(result.map(|t| t.bytes_transferred)).unwrap();
            },
        )
        .unwrap();
    run_until_dispatched(&proactor, 1, "write");
    let written = write_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .unwrap();
    assert_eq!(written as usize, b"hello iocp".len());

    for (offset, expected) in [(0u64, &b"hello iocp"[..]), (6, &b"iocp"[..])] {
        let (read_tx, read_rx) = mpsc::channel();
        handle
            .read(
                fd,
                IoBuf::with_capacity(64),
                offset,
                move |result: IoResult| {
                    let transfer = result.unwrap();
                    read_tx
                        .send((transfer.bytes_transferred, transfer.buf.into_vec()))
                        .unwrap();
                },
            )
            .unwrap();
        run_until_dispatched(&proactor, 1, "read");
        let (n, buf) = read_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(n as usize, expected.len(), "read at offset {offset}");
        assert_eq!(&buf[..n as usize], expected, "read at offset {offset}");
    }

    drop(file);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn iocp_recv_from_reports_the_real_sender_and_send_to_reaches_it() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let receiver_addr = receiver.local_addr().unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sender_addr = sender.local_addr().unwrap();

    let (recv_tx, recv_rx) = mpsc::channel();
    handle
        .recv_from(
            receiver.as_raw_socket(),
            IoBuf::with_capacity(64),
            move |result: IoResult| {
                let transfer = result.unwrap();
                recv_tx
                    .send((
                        transfer.bytes_transferred,
                        transfer.peer,
                        transfer.buf.into_vec(),
                    ))
                    .unwrap();
            },
        )
        .unwrap();

    let (send_tx, send_rx) = mpsc::channel();
    handle
        .send_to(
            sender.as_raw_socket(),
            IoBuf::from_vec(b"ping".to_vec()),
            receiver_addr,
            move |result: IoResult| {
                send_tx.send(result.map(|t| t.bytes_transferred)).unwrap();
            },
        )
        .unwrap();

    run_until_dispatched(&proactor, 2, "send_to/recv_from");
    let sent = send_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .unwrap();
    assert_eq!(sent as usize, 4);
    let (n, peer, buf) = recv_rx.recv_timeout(Duration::from_millis(100)).unwrap();
    assert_eq!(n as usize, 4);
    assert_eq!(&buf[..4], b"ping");
    assert_eq!(peer, Some(sender_addr));
}

/// Accepts one connection on a listener bound to `bind`, then proves the
/// accepted socket is really usable -- `SO_UPDATE_ACCEPT_CONTEXT` applied,
/// data flowing -- not merely that a completion arrived.
fn accept_round_trip(bind: SocketAddr) {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let listener = TcpListener::bind(bind).unwrap();
    let listener_addr = listener.local_addr().unwrap();

    let (tx, rx) = mpsc::channel();
    handle
        .accept(listener.as_raw_socket(), move |result: AcceptResult| {
            tx.send(result.map(|transfer| (transfer.new_fd, transfer.peer)))
                .unwrap();
        })
        .unwrap();

    let mut client = TcpStream::connect(listener_addr).unwrap();
    let client_addr = client.local_addr().unwrap();

    run_until_dispatched(&proactor, 1, "accept");
    let (new_socket, peer) = rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .expect("accept failed");
    assert_eq!(peer, PeerAddr::Ip(client_addr));

    let mut accepted = unsafe { TcpStream::from_raw_socket(new_socket) };
    assert_eq!(accepted.peer_addr().unwrap(), client_addr);
    client.write_all(b"hi").unwrap();
    let mut got = [0u8; 2];
    accepted.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hi");
}

#[test]
fn iocp_accept_reports_the_real_connecting_peer() {
    accept_round_trip("127.0.0.1:0".parse().unwrap());
}

#[test]
fn iocp_accept_works_on_an_ipv6_listener() {
    // Regression: the accept socket `AcceptEx` needs up front was always
    // created as AF_INET, which an IPv6 listener rejects.
    let bind: SocketAddr = "[::1]:0".parse().unwrap();
    if TcpListener::bind(bind).is_err() {
        eprintln!("skipping: this machine has no IPv6 loopback");
        return;
    }
    accept_round_trip(bind);
}

/// A fresh overlapped TCP socket, bound to nothing and connected to nothing.
/// Call only after something in the test has already opened a std socket,
/// which is what initializes WinSock for this process.
fn unconnected_overlapped_tcp_socket_v4() -> u64 {
    use windows::Win32::Networking::WinSock::{
        WSASocketW, AF_INET, SOCK_STREAM, WSA_FLAG_OVERLAPPED,
    };
    let socket = unsafe {
        WSASocketW(
            AF_INET.0 as i32,
            SOCK_STREAM.0,
            0,
            None,
            0,
            WSA_FLAG_OVERLAPPED,
        )
    }
    .expect("WSASocketW");
    socket.0 as u64
}

#[test]
fn iocp_connect_reaches_a_real_listener_and_leaves_a_usable_socket() {
    // Regression: nothing applied SO_UPDATE_CONNECT_CONTEXT after ConnectEx,
    // so `getpeername` (std's `peer_addr`) failed on the connected socket.
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let listener_addr = listener.local_addr().unwrap();
    let socket = unconnected_overlapped_tcp_socket_v4();

    let (tx, rx) = mpsc::channel();
    handle
        .connect(socket, listener_addr, move |result: io::Result<()>| {
            tx.send(result).unwrap();
        })
        .unwrap();

    run_until_dispatched(&proactor, 1, "connect");
    rx.recv_timeout(Duration::from_millis(100))
        .unwrap()
        .expect("connect failed");

    let stream = unsafe { TcpStream::from_raw_socket(socket) };
    assert_eq!(stream.peer_addr().unwrap(), listener_addr);
}

#[test]
fn iocp_cancel_io_completes_the_op_with_operation_aborted() {
    // Also pins error reporting: the code must come from the failed call
    // itself, not from a later `GetLastError`, and reach the handler as a
    // raw OS error rather than as text.
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let op = handle
        .recv(
            socket.as_raw_socket(),
            IoBuf::with_capacity(64),
            move |result: IoResult| {
                tx.send(result.map(|t| t.bytes_transferred)).unwrap();
            },
        )
        .unwrap();

    handle.cancel_io(op).unwrap();
    run_until_dispatched(&proactor, 1, "cancelled recv");

    let err = rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .expect_err("a cancelled recv must report an error, not data");
    assert_eq!(
        err.raw_os_error(),
        Some(ERROR_OPERATION_ABORTED),
        "expected ERROR_OPERATION_ABORTED, got {err:?}"
    );
}

#[test]
fn iocp_shutdown_drains_a_still_in_flight_op_instead_of_hanging() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    // A recv on a socket nothing will ever write to, so it is certainly
    // still in flight when stop() lands: this exercises the
    // begin_shutdown/CancelIoEx/shutdown_complete drain rather than the
    // nothing-in-flight no-op.
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    handle
        .recv(
            socket.as_raw_socket(),
            IoBuf::with_capacity(64),
            |_result| {},
        )
        .unwrap();

    let worker = thread::spawn(move || {
        let started = Instant::now();
        proactor.run_until_stopped().unwrap();
        started.elapsed()
    });

    thread::sleep(Duration::from_millis(25));
    handle.stop().unwrap();

    let elapsed = worker.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(5),
        "run_until_stopped did not drain the in-flight recv and return"
    );
}
