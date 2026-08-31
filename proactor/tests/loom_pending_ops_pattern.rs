//! Exhaustively model-checks the synchronization *pattern* `src/uring.rs`
//! uses to avoid the real deadlock fixed in `IoUringPort::register_readable`/
//! `deregister` (see that file's `pending_ops` doc comment, and
//! `loadngo/network/tests/sneakernet.rs`'s
//! `registered_proactor_pump_handles_dual_stack_node_sockets`, which is what
//! first caught it empirically).
//!
//! This does **not** drive `IoUringPort` itself. `loom::model` re-runs the
//! same closure many times exploring every legal thread interleaving, which
//! is fundamentally incompatible with code that makes real blocking
//! syscalls (`io_uring_enter`, `eventfd` reads/writes) -- those have real
//! kernel-side effects and real blocking semantics loom doesn't control, so
//! there's no meaningful way to "explore interleavings" of a real syscall.
//! Instead, this reimplements the *shape* of the fix with loom's own
//! `Mutex` standing in for the real `Mutex<IoUring>`:
//!
//! - `ring_lock` <-> `IoUringPort::ring` (`Mutex<IoUring>`)
//! - `pending`   <-> `IoUringPort::pending_ops` (`Mutex<VecDeque<PendingRingOp>>`)
//! - `poll_tick` <-> one call to `IoUringPort::poll()`'s ring-lock section
//!   (acquire `ring`, call `apply_pending_ops`, release) -- the real
//!   blocking `submit_and_wait` in between is omitted, since it only
//!   touches the eventfd `wake_fd`, never `ring_lock` or `pending`, so it
//!   structurally cannot participate in the deadlock this model checks for.
//!   What it *does* model is the outer `run_until_stopped` loop's repeated,
//!   unconditional re-entry into `poll()` -- that's the real mechanism that
//!   guarantees a queued op eventually gets applied, not any single call
//!   completing synchronously.
//!
//! If `uring.rs`'s actual algorithm shape changes, update this model to
//! match -- it's a faithful reimplementation of the pattern, not a
//! generated one, so nothing enforces they stay in sync automatically.
//!
//! Run with:
//!   RUSTFLAGS="--cfg loom" cargo test --release -p loadngo-proactor \
//!     --test loom_pending_ops_pattern
//!
//! Without `--cfg loom` this file compiles to zero tests, so it's inert in
//! normal `cargo test`/CI runs -- loom models are far too slow for a fast
//! lint/test gate.

#![cfg(loom)]

use loom::sync::Mutex;
use std::collections::VecDeque;

/// Stands in for `IoUringPort::ring`'s submission queue.
struct Ring {
    submitted: Vec<u64>,
}

struct Model {
    ring_lock: Mutex<Ring>,
    pending: Mutex<VecDeque<u64>>,
}

impl Model {
    fn new() -> Self {
        Self {
            ring_lock: Mutex::new(Ring {
                submitted: Vec::new(),
            }),
            pending: Mutex::new(VecDeque::new()),
        }
    }

    /// Mirrors `IoUringPort::apply_pending_ops`.
    fn apply_pending(&self, ring: &mut Ring) {
        let mut pending = self.pending.lock().unwrap();
        while let Some(op) = pending.pop_front() {
            ring.submitted.push(op);
        }
    }

    /// Mirrors one `poll()` call's ring-lock section: acquire `ring_lock`,
    /// drain whatever `pending` holds right now, release. Each test below
    /// calls this once concurrently with the registrar(s), then once more
    /// after joining -- see those tests' doc comments for why the second,
    /// sequential call is what makes the model airtight.
    fn poll_tick(&self) {
        let mut ring = self.ring_lock.lock().unwrap();
        self.apply_pending(&mut ring);
    }

    /// Mirrors the *fixed* `register_readable`/`deregister`: try the fast
    /// path (`ring_lock.try_lock()`), and on contention queue instead of
    /// ever blocking on `ring_lock` -- the actual property that makes the
    /// fix deadlock-free. (The real `wake()` call on the queued path is
    /// omitted: it only touches `wake_fd`, never `ring_lock` or `pending`,
    /// so -- like the blocking wait in `poll_tick`'s doc comment above --
    /// it can't affect this lock-ordering property either way.)
    fn register_fixed(&self, op: u64) {
        match self.ring_lock.try_lock() {
            Ok(mut ring) => ring.submitted.push(op),
            Err(_) => {
                self.pending.lock().unwrap().push_back(op);
            }
        }
    }

}

/// The property that matters: every legal interleaving of a poller thread
/// and a registrar thread completes (loom itself fails the test if any
/// explored interleaving leaves a thread unable to finish -- that's what
/// "deadlock-free" means here), and the registrar's operation is never
/// lost. The poller runs one tick concurrently with the registrar, then --
/// after both threads have joined, i.e. strictly sequentially, racing
/// nothing -- one final tick. That final tick is what makes this airtight
/// rather than relying on picking "enough" concurrent ticks (no fixed
/// count ever fully closes a race against "queued in the last instant of
/// the last tick"; the real guarantee comes from the outer loop running
/// forever, not from any bound). By the time it runs, the registrar has
/// unconditionally finished, so nothing else can still be racing to add to
/// `pending` -- a subsequent `apply_pending` is then guaranteed to see it.
#[test]
fn register_never_deadlocks_against_a_blocked_poller() {
    loom::model(|| {
        let model = std::sync::Arc::new(Model::new());

        let poller = {
            let model = model.clone();
            loom::thread::spawn(move || model.poll_tick())
        };

        let registrar = {
            let model = model.clone();
            loom::thread::spawn(move || model.register_fixed(42))
        };

        registrar.join().unwrap();
        poller.join().unwrap();
        model.poll_tick(); // sequential "one more outer-loop iteration"

        let ring = model.ring_lock.lock().unwrap();
        assert!(
            ring.submitted.contains(&42),
            "registrar's op was lost instead of applied directly or via pending_ops"
        );
    });
}

/// Same property under two concurrent registrars, since the real
/// `pending_ops` queue is shared across every caller, not just one --
/// closer to the real multi-socket scenario in `sneakernet.rs`'s
/// dual-stack test.
#[test]
fn two_concurrent_registrars_never_deadlock_and_neither_op_is_lost() {
    loom::model(|| {
        let model = std::sync::Arc::new(Model::new());

        let poller = {
            let model = model.clone();
            loom::thread::spawn(move || model.poll_tick())
        };

        let reg_a = {
            let model = model.clone();
            loom::thread::spawn(move || model.register_fixed(1))
        };
        let reg_b = {
            let model = model.clone();
            loom::thread::spawn(move || model.register_fixed(2))
        };

        reg_a.join().unwrap();
        reg_b.join().unwrap();
        poller.join().unwrap();
        model.poll_tick(); // sequential "one more outer-loop iteration"

        let ring = model.ring_lock.lock().unwrap();
        assert!(ring.submitted.contains(&1), "registrar A's op was lost");
        assert!(ring.submitted.contains(&2), "registrar B's op was lost");
    });
}
