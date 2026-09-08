use loadngo_proactor::{ChannelPort, Completion, CompletionKind, Proactor};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn dispatches_enqueued_work() {
    let proactor = Proactor::new(ChannelPort::new());
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
        rx.recv_timeout(Duration::from_millis(50)).unwrap(),
        (CompletionKind::Job, 0)
    );
}

#[test]
fn dispatches_deferred_work_after_deadline() {
    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();

    handle
        .defer_for(
            Duration::from_millis(20),
            CompletionKind::Timer,
            7,
            move |completion: Completion| {
                tx.send((completion.kind, completion.bytes_transferred))
                    .unwrap();
            },
        )
        .unwrap();

    let start = Instant::now();
    loop {
        let report = proactor.run_once().unwrap();
        if report.dispatched_deferred > 0 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    assert_eq!(
        rx.recv_timeout(Duration::from_millis(50)).unwrap(),
        (CompletionKind::Timer, 7)
    );
}

#[test]
fn preserves_deferred_insertion_order_for_equal_deadlines() {
    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let (tx, rx) = mpsc::channel();
    let when = Instant::now() + Duration::from_millis(20);
    let tx_first = tx.clone();
    let tx_second = tx.clone();

    handle
        .defer_until(
            when,
            CompletionKind::Timer,
            1,
            move |_completion: Completion| {
                tx_first.send(1u8).unwrap();
            },
        )
        .unwrap();
    handle
        .defer_until(
            when,
            CompletionKind::Timer,
            2,
            move |_completion: Completion| {
                tx_second.send(2u8).unwrap();
            },
        )
        .unwrap();

    let start = Instant::now();
    let mut dispatched = 0;
    while dispatched < 2 {
        let report = proactor.run_once().unwrap();
        dispatched += report.dispatched_deferred;
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    assert_eq!(rx.recv_timeout(Duration::from_millis(50)).unwrap(), 1);
    assert_eq!(rx.recv_timeout(Duration::from_millis(50)).unwrap(), 2);
}

#[test]
fn wake_interrupts_blocking_poll_for_earlier_deadline() {
    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let worker = thread::spawn(move || {
        let started = Instant::now();
        let report = proactor.run_once().unwrap();
        (started.elapsed(), report)
    });

    thread::sleep(Duration::from_millis(25));
    handle
        .defer_for(Duration::from_millis(10), CompletionKind::Timer, 0, |_| {})
        .unwrap();

    let (elapsed, report) = worker.join().unwrap();
    assert!(elapsed < Duration::from_secs(1));
    assert!(report.woke || report.dispatched_deferred > 0);
}

#[test]
fn stop_wakes_and_ends_loop() {
    let proactor = Proactor::new(ChannelPort::new());
    let handle = proactor.handle();
    let worker = thread::spawn(move || proactor.run_until_stopped().unwrap());

    thread::sleep(Duration::from_millis(20));
    handle.stop().unwrap();

    worker.join().unwrap();
    assert!(!handle.is_running());
}

/// The public `IoPort` surface must stay free of raw OS address types.
///
/// `loadngo-proactor` is the platform seam for networking in this
/// workspace -- nothing above it names a `libc`/`windows` address type,
/// and `io_port.rs` is the file that defines what those crates *do* see.
/// Raw platform types belong in `sockaddr.rs` and the per-backend
/// modules, which are compiled per target and can be concrete about
/// widths and signedness.
///
/// This is a real constraint rather than a style preference: when the
/// decoding lived here, `sa_family_t` being `u16` on Linux and `u8` on
/// macOS meant no single cast form satisfied clippy on both, and the
/// file needed an `#[allow]` to compile clean everywhere. See
/// `src/sockaddr.rs`'s module docs.
#[test]
fn io_port_surface_names_no_raw_platform_address_types() {
    let src = include_str!("../src/io_port.rs");

    // Comments are allowed to *discuss* platform types -- that is where
    // the reasoning lives. Only real code is constrained.
    let code: String = src
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    // `std::os::windows::io::RawSocket` is std, not the `windows` crate,
    // and is the portable handle alias this surface is built on.
    for forbidden in ["libc::", "socket2::", "windows::Win32", "sockaddr_"] {
        assert!(
            !code.contains(forbidden),
            "io_port.rs code mentions `{forbidden}`; raw platform address types \
             belong in sockaddr.rs or a per-backend module, not in the shared surface"
        );
    }
}
