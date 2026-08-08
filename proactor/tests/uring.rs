#![cfg(target_os = "linux")]

use loadngo_proactor::{Completion, CompletionKind, IoUringPort, Proactor, ReadinessEvent};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn uring_dispatches_enqueued_work() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
fn uring_dispatches_burst_enqueued_work_without_lost_wakeup() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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

    let start = Instant::now();
    let mut dispatched = 0;
    while dispatched < 3 {
        let report = proactor.run_ready().unwrap();
        dispatched += report.dispatched_completions;
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 1);
    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 2);
    assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), 3);
}

#[test]
fn uring_wake_interrupts_blocking_poll() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
fn uring_stop_wakes_and_ends_loop() {
    let proactor = Proactor::new(IoUringPort::new().unwrap());
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
