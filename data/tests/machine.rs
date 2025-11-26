use data::machine::Machine;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn machine_runs_enqueued_work() {
    let machine = Machine::new();
    let (tx, rx) = mpsc::channel();

    machine
        .enqueue_fn(7, 99, move |key, size| {
            tx.send((key, size)).unwrap();
        })
        .unwrap();

    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), (7, 99));
}
