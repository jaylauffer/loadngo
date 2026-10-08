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
    ProactorHandle, TimerWait,
};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, AsRawSocket, FromRawSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

mod support;

/// `FILE_FLAG_OVERLAPPED`. IOCP only queues completions for handles opened
/// with it; a plain `std::fs::File` would complete synchronously and never
/// post one.
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

/// `ERROR_OPERATION_ABORTED`, what a cancelled overlapped operation completes
/// with.
const ERROR_OPERATION_ABORTED: i32 = 995;

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

    let report = support::run_once(&proactor, "iocp_dispatches_enqueued_work");
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

    support::run_until_dispatched(&proactor, 3, "burst of enqueued work");
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

    let worker = support::spawn(move || {
        let started = Instant::now();
        let report = proactor.run_once().unwrap();
        (started.elapsed(), report)
    });

    thread::sleep(Duration::from_millis(25));
    handle.wake().unwrap();

    let (elapsed, report) = worker.join("iocp_wake_interrupts_blocking_poll");
    assert!(elapsed < Duration::from_secs(1));
    assert!(report.woke);
}

#[test]
fn iocp_stop_wakes_and_ends_loop() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let worker = support::spawn(move || {
        let started = Instant::now();
        proactor.run_until_stopped().unwrap();
        started.elapsed()
    });

    thread::sleep(Duration::from_millis(25));
    handle.stop().unwrap();

    let elapsed = worker.join("iocp_stop_wakes_and_ends_loop");
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
    support::run_until_dispatched(&proactor, 1, "write");
    let written = write_rx
        .recv_timeout(Duration::from_millis(100))
        .unwrap()
        .unwrap();
    assert_eq!(written as usize, b"hello iocp".len());

    // At and past the end of the file `ReadFile` fails with
    // `ERROR_HANDLE_EOF`; the Unix backends return 0 bytes there, and so
    // must this one (a reader at end of file otherwise sees an error).
    for (offset, expected) in [
        (0u64, &b"hello iocp"[..]),
        (6, &b"iocp"[..]),
        (10, &b""[..]),
        (64, &b""[..]),
    ] {
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
        support::run_until_dispatched(&proactor, 1, "read");
        let (n, buf) = read_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(n as usize, expected.len(), "read at offset {offset}");
        assert_eq!(&buf[..n as usize], expected, "read at offset {offset}");
    }

    drop(file);
    let _ = std::fs::remove_file(&path);
}

/// How many open/operate/close cycles the handle-reuse tests run. Windows
/// hands a closed handle's value to the next handle opened, usually on the
/// very next open; the tests fail if no value came back at all.
const REUSE_CYCLES: usize = 64;

/// Regression (2026-10-08): the port remembered which handles it had
/// associated by their raw value. A file opened after another was closed
/// usually gets the closed one's value, so the port skipped associating the
/// new handle, its read's completion went nowhere and `run_once` waited
/// forever. espeak-ng-rs's engine reads (open, read, close, on one
/// process-wide port) hung its Windows CI for six hours per job. Run with
/// handles both registered and not (associated on every operation).
fn reads_a_file_opened_after_another_was_closed(register: bool) {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();

    let path = std::env::temp_dir().join(format!(
        "loadngo-proactor-iocp-reuse-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let contents: Vec<u8> = (0..4096u32).map(|i| (i * 7) as u8).collect();
    std::fs::write(&path, &contents).unwrap();

    let mut seen = std::collections::HashSet::new();
    let mut reused = 0;
    for cycle in 0..REUSE_CYCLES {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .open(&path)
            .unwrap();
        let raw = file.as_raw_handle() as usize as u64;
        if !seen.insert(raw) {
            reused += 1;
        }
        let fd = operand(&handle, register, raw);

        let (tx, rx) = mpsc::channel();
        handle
            .read(
                fd,
                IoBuf::with_capacity(contents.len()),
                0,
                move |result: IoResult| {
                    let transfer = result.unwrap();
                    tx.send(
                        transfer.buf.into_vec()[..transfer.bytes_transferred as usize].to_vec(),
                    )
                    .unwrap();
                },
            )
            .unwrap();
        support::run_until_dispatched(
            &proactor,
            1,
            &format!("read on handle {raw:#x}, cycle {cycle}, {reused} reused values so far"),
        );
        assert_eq!(rx.try_recv().unwrap(), contents, "cycle {cycle}");
        release(&handle, register, fd);
        drop(file);
    }
    assert!(
        reused > 0,
        "no handle value was reused in {REUSE_CYCLES} cycles, so reuse went untested"
    );
    let _ = std::fs::remove_file(&path);
}

/// The same defect on the socket path: a socket created after another was
/// closed reuses its value.
fn sends_on_a_socket_created_after_another_was_closed(register: bool) {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let receiver_addr = receiver.local_addr().unwrap();

    let mut seen = std::collections::HashSet::new();
    let mut reused = 0;
    for cycle in 0..REUSE_CYCLES {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let raw = sender.as_raw_socket();
        if !seen.insert(raw) {
            reused += 1;
        }
        let fd = operand(&handle, register, raw);
        let (tx, rx) = mpsc::channel();
        handle
            .send_to(
                fd,
                IoBuf::from_vec(b"ping".to_vec()),
                receiver_addr,
                move |result: IoResult| {
                    tx.send(result.map(|t| t.bytes_transferred)).unwrap();
                },
            )
            .unwrap();
        support::run_until_dispatched(
            &proactor,
            1,
            &format!("send_to on socket {raw:#x}, cycle {cycle}, {reused} reused values so far"),
        );
        assert_eq!(rx.try_recv().unwrap().unwrap(), 4, "cycle {cycle}");
        release(&handle, register, fd);
        drop(sender);
    }
    assert!(
        reused > 0,
        "no socket value was reused in {REUSE_CYCLES} cycles, so reuse went untested"
    );
}

/// The value operations take for `raw`: itself, or the tagged value
/// registering it returns.
fn operand(handle: &ProactorHandle<IocpPort>, register: bool, raw: u64) -> u64 {
    if register {
        let tagged = handle.register(raw).unwrap();
        assert_ne!(tagged, raw);
        tagged
    } else {
        raw
    }
}

fn release(handle: &ProactorHandle<IocpPort>, register: bool, fd: u64) {
    if register {
        handle.release(fd);
    }
}

#[test]
fn iocp_reads_a_reopened_handle_value_unregistered() {
    reads_a_file_opened_after_another_was_closed(false);
}

#[test]
fn iocp_reads_a_reopened_handle_value_registered() {
    reads_a_file_opened_after_another_was_closed(true);
}

#[test]
fn iocp_sends_on_a_reopened_socket_value_unregistered() {
    sends_on_a_socket_created_after_another_was_closed(false);
}

#[test]
fn iocp_sends_on_a_reopened_socket_value_registered() {
    sends_on_a_socket_created_after_another_was_closed(true);
}

/// A tagged value is refused, not hung on, once released, and when it was
/// never registered at all.
#[test]
fn iocp_refuses_released_and_unregistered_tagged_values() {
    let proactor = Proactor::new(IocpPort::new().unwrap());
    let handle = proactor.handle();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = socket.local_addr().unwrap();
    let tagged = handle.register(socket.as_raw_socket()).unwrap();
    assert_ne!(tagged, socket.as_raw_socket());
    handle.release(tagged);
    for fd in [tagged, tagged + (1 << 32)] {
        let error = handle
            .send_to(fd, IoBuf::from_vec(b"x".to_vec()), target, |_| {})
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{fd:#x}");
    }
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

    support::run_until_dispatched(&proactor, 2, "send_to/recv_from");
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

    support::run_until_dispatched(&proactor, 1, "accept");
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

    support::run_until_dispatched(&proactor, 1, "connect");
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
    support::run_until_dispatched(&proactor, 1, "cancelled recv");

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

    let worker = support::spawn(move || {
        let started = Instant::now();
        proactor.run_until_stopped().unwrap();
        started.elapsed()
    });

    thread::sleep(Duration::from_millis(25));
    handle.stop().unwrap();

    let elapsed = worker.join("iocp_shutdown_drains_a_still_in_flight_op_instead_of_hanging");
    assert!(
        elapsed < Duration::from_secs(5),
        "run_until_stopped did not drain the in-flight recv and return"
    );
}

/// Median lateness of `count` successive 1 ms deferred deadlines.
fn median_timer_lateness(proactor: &Proactor<IocpPort>, count: usize) -> Duration {
    let handle = proactor.handle();
    let mut lateness = Vec::with_capacity(count);
    for _ in 0..count {
        let (tx, rx) = mpsc::channel();
        let due = Instant::now() + Duration::from_millis(1);
        handle
            .defer_until(due, CompletionKind::Timer, 0, move |_| {
                tx.send(Instant::now()).unwrap();
            })
            .unwrap();
        let deadline = Instant::now() + support::LIMIT;
        let fired = loop {
            if let Ok(fired) = rx.try_recv() {
                break fired;
            }
            assert!(Instant::now() < deadline, "1 ms timer never fired");
            proactor.run_once_until(deadline).unwrap();
        };
        lateness.push(fired.saturating_duration_since(due));
    }
    lateness.sort();
    lateness[count / 2]
}

/// The default millisecond wait ends on Windows' 15.6 ms tick; both
/// candidates must fire a 1 ms deadline within a few milliseconds. Generous
/// for a shared runner: the profile records the real figures.
#[test]
fn iocp_timer_resolution_fires_a_1ms_deadline_within_4ms() {
    let proactor = Proactor::new(IocpPort::with_timer_wait(TimerWait::TimerResolution).unwrap());
    let late = median_timer_lateness(&proactor, 21);
    assert!(late < Duration::from_millis(4), "median {late:?} late");
}

#[test]
fn iocp_wait_packet_fires_a_1ms_deadline_within_4ms() {
    let proactor = Proactor::new(IocpPort::with_timer_wait(TimerWait::WaitPacket).unwrap());
    let late = median_timer_lateness(&proactor, 21);
    assert!(late < Duration::from_millis(4), "median {late:?} late");
}

/// Work posted from another thread ends a wait the timer was armed for;
/// the timer's packet is withdrawn, and the next timed waits still end on
/// their own deadlines, not early on a stale packet nor late.
#[test]
fn iocp_wait_packet_survives_waits_ended_by_other_work() {
    let proactor = Proactor::new(IocpPort::with_timer_wait(TimerWait::WaitPacket).unwrap());
    let handle = proactor.handle();
    for round in 0..10 {
        let (timer_tx, timer_rx) = mpsc::channel();
        let due = Instant::now() + Duration::from_millis(30);
        handle
            .defer_until(due, CompletionKind::Timer, 0, move |_| {
                timer_tx.send(Instant::now()).unwrap();
            })
            .unwrap();
        // Takes the wake that scheduling posted.
        proactor.run_ready().unwrap();
        let poster = handle.clone();
        let (work_tx, work_rx) = mpsc::channel();
        let posting = support::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            poster
                .enqueue_work(move |_| work_tx.send(()).unwrap())
                .unwrap();
        });
        let report = support::run_once(&proactor, "posted work during a timed wait");
        posting.join("posting thread");
        assert_eq!(report.dispatched_completions, 1, "round {round}");
        work_rx.try_recv().unwrap();
        assert!(
            timer_rx.try_recv().is_err(),
            "round {round}: timer fired early"
        );

        let deadline = Instant::now() + support::LIMIT;
        let fired = loop {
            if let Ok(fired) = timer_rx.try_recv() {
                break fired;
            }
            assert!(
                Instant::now() < deadline,
                "round {round}: timer never fired"
            );
            proactor.run_once_until(deadline).unwrap();
        };
        assert!(fired >= due, "round {round}: fired before its deadline");
        let late = fired - due;
        assert!(
            late < Duration::from_millis(8),
            "round {round}: {late:?} late"
        );
    }
}

/// The timer fires while nothing waits on it: a non-blocking `run_ready`
/// takes its packet. Later timed waits must still end on their deadlines.
#[test]
fn iocp_wait_packet_survives_a_timer_packet_taken_by_run_ready() {
    let proactor = Proactor::new(IocpPort::with_timer_wait(TimerWait::WaitPacket).unwrap());
    let handle = proactor.handle();
    for round in 0..5 {
        // Arm the timer for a 10 ms deadline, then let posted work end the wait.
        handle
            .defer_for(Duration::from_millis(10), CompletionKind::Timer, 0, |_| {})
            .unwrap();
        proactor.run_ready().unwrap();
        let poster = handle.clone();
        let posting = support::spawn(move || {
            thread::sleep(Duration::from_millis(2));
            poster.enqueue_work(|_| {}).unwrap();
        });
        support::run_once(&proactor, "posted work during a timed wait");
        posting.join("posting thread");
        // The timer fires with nobody waiting; run_ready dispatches the
        // deadline and dequeues the packet.
        thread::sleep(Duration::from_millis(20));
        for _ in 0..3 {
            proactor.run_ready().unwrap();
        }

        let late = median_timer_lateness(&proactor, 5);
        assert!(
            late < Duration::from_millis(4),
            "round {round}: median {late:?} late"
        );
    }
}
