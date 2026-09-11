//! A lossless, full-width capture path out of [`crate::LiveMonitor`]'s input
//! stream, for writing recordings.
//!
//! The analysis tap (`LiveMonitor::drain_tap`) is the wrong source for a
//! recording on two counts: it is downmixed to mono, and it deliberately
//! drops old samples so a tuner always sees the latest audio. A recording
//! needs every channel and every sample, in order.
//!
//! So the input callback also feeds a third lock-free SPSC ring carrying
//! **interleaved frames at the device's full channel count**, as `f32`. The
//! callback only writes while a consumer has [armed](RecordingTap::arm) it,
//! so an idle monitor doesn't spend its life counting overruns, and it only
//! ever drops *whole frames* when the ring is full, so a consumer that falls
//! behind gets a gap rather than channels swapped for the rest of the take.
//! Every dropped sample is counted and reported, never silent.
//!
//! `f32` holds any integer sample of 24 bits or fewer exactly, which covers
//! the converters this was built for (16/24-bit USB interfaces). A 32-bit
//! integer converter loses its bottom 8 bits of dither on the way through
//! `f32`; recorders should store those as float rather than pretend
//! otherwise.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// State shared between the input callback and the [`RecordingTap`].
#[derive(Debug, Default)]
pub(crate) struct RecordingShared {
    armed: AtomicBool,
    dropped_samples: AtomicU64,
    /// Incremented at the end of every input callback, armed or not. Lets
    /// [`RecordingTap::disarm_and_settle`] tell when a block that was in
    /// flight at disarm time has definitely finished writing.
    callback_blocks: AtomicU64,
}

/// The input callback's end of the recording ring.
pub(crate) struct RecordingProducer {
    producer: rtrb::Producer<f32>,
    shared: Arc<RecordingShared>,
    channels: usize,
}

impl RecordingProducer {
    /// Writes one callback's worth of interleaved samples if armed. Never
    /// blocks or allocates; on a full ring it writes the whole frames that
    /// fit and counts the rest as dropped.
    pub(crate) fn push_interleaved(&mut self, interleaved: &[f32]) {
        if self.shared.armed.load(Ordering::Relaxed) {
            let wanted = interleaved.len() - interleaved.len() % self.channels.max(1);
            let fit = if self.producer.slots() >= wanted {
                wanted
            } else {
                self.producer.slots() - self.producer.slots() % self.channels.max(1)
            };
            if fit > 0 {
                if let Ok(chunk) = self.producer.write_chunk_uninit(fit) {
                    chunk.fill_from_iter(interleaved[..fit].iter().copied());
                }
            }
            let dropped = (interleaved.len() - fit) as u64;
            if dropped > 0 {
                self.shared
                    .dropped_samples
                    .fetch_add(dropped, Ordering::Relaxed);
            }
        }
        self.shared.callback_blocks.fetch_add(1, Ordering::Relaxed);
    }
}

/// The consumer end of a [`crate::LiveMonitor`]'s recording ring. `Send`, so
/// a recorder can move it onto its own storage thread; hand it back with
/// [`crate::LiveMonitor::return_recording_tap`] when done so the next
/// recording can take it again.
pub struct RecordingTap {
    consumer: rtrb::Consumer<f32>,
    shared: Arc<RecordingShared>,
    channels: u16,
    sample_rate_hz: u32,
}

impl RecordingTap {
    /// Starts capturing into the ring. Anything left over from a previous
    /// session is discarded first, so a recording never begins with stale
    /// audio.
    pub fn arm(&mut self) {
        self.discard_pending();
        self.shared.dropped_samples.store(0, Ordering::Relaxed);
        self.shared.armed.store(true, Ordering::Relaxed);
    }

    /// Stops capturing, then waits (up to `timeout`) for any callback block
    /// that was mid-write at disarm time to finish, so the ring holds the
    /// complete tail of the session before the caller's final drain. Returns
    /// `false` if the stream never produced another block in time -- e.g. a
    /// device that was unplugged -- in which case nothing more is coming
    /// anyway.
    pub fn disarm_and_settle(&mut self, timeout: Duration) -> bool {
        self.shared.armed.store(false, Ordering::Relaxed);
        let seen = self.shared.callback_blocks.load(Ordering::Relaxed);
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            // Two completed blocks after `seen` guarantees the one that may
            // have read `armed == true` just before we cleared it is done.
            if self.shared.callback_blocks.load(Ordering::Relaxed) >= seen + 2 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.shared.armed.load(Ordering::Relaxed)
    }

    /// Moves up to `max_samples` interleaved samples (rounded down to whole
    /// frames) into `out`. Returns how many samples were moved.
    pub fn pop_into(&mut self, out: &mut Vec<f32>, max_samples: usize) -> usize {
        let channels = usize::from(self.channels.max(1));
        let available = self.consumer.slots().min(max_samples);
        let whole = available - available % channels;
        if whole == 0 {
            return 0;
        }
        let Ok(chunk) = self.consumer.read_chunk(whole) else {
            return 0;
        };
        let (first, second) = chunk.as_slices();
        out.extend_from_slice(first);
        out.extend_from_slice(second);
        chunk.commit_all();
        whole
    }

    /// Interleaved samples currently waiting in the ring.
    #[must_use]
    pub fn pending_samples(&self) -> usize {
        self.consumer.slots()
    }

    /// Samples the input callback had to drop because the ring was full,
    /// since the last [`arm`](Self::arm).
    #[must_use]
    pub fn dropped_samples(&self) -> u64 {
        self.shared.dropped_samples.load(Ordering::Relaxed)
    }

    /// `true` once the monitor that fed this ring has stopped (device
    /// switched or closed). Drain what's left; nothing new will arrive.
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        self.consumer.is_abandoned()
    }

    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    #[must_use]
    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    fn discard_pending(&mut self) {
        let pending = self.consumer.slots();
        if let Ok(chunk) = self.consumer.read_chunk(pending) {
            chunk.commit_all();
        }
    }
}

/// Builds a recording ring holding `seconds` of interleaved audio.
pub(crate) fn recording_ring(
    channels: u16,
    sample_rate_hz: u32,
    seconds: f32,
) -> (RecordingProducer, RecordingTap) {
    let per_second = usize::from(channels.max(1)) * sample_rate_hz as usize;
    // Truncation is fine: this is a buffer size, and at least one second of
    // headroom is enforced below.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let capacity = ((per_second as f32) * seconds.max(1.0)) as usize;
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity);
    let shared = Arc::new(RecordingShared::default());
    (
        RecordingProducer {
            producer,
            shared: shared.clone(),
            channels: usize::from(channels.max(1)),
        },
        RecordingTap {
            consumer,
            shared,
            channels,
            sample_rate_hz,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_ring(channels: u16, capacity_samples: usize) -> (RecordingProducer, RecordingTap) {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity_samples);
        let shared = Arc::new(RecordingShared::default());
        (
            RecordingProducer {
                producer,
                shared: shared.clone(),
                channels: usize::from(channels),
            },
            RecordingTap {
                consumer,
                shared,
                channels,
                sample_rate_hz: 48_000,
            },
        )
    }

    #[test]
    fn nothing_is_captured_until_armed() {
        let (mut producer, mut tap) = small_ring(2, 64);
        producer.push_interleaved(&[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(tap.pending_samples(), 0);
        assert_eq!(tap.dropped_samples(), 0);

        tap.arm();
        producer.push_interleaved(&[0.1, 0.2, 0.3, 0.4]);
        let mut out = Vec::new();
        assert_eq!(tap.pop_into(&mut out, usize::MAX), 4);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn a_full_ring_drops_whole_frames_and_counts_them() {
        // Room for 5 samples of a stereo stream: only 2 whole frames fit.
        let (mut producer, mut tap) = small_ring(2, 5);
        tap.arm();
        producer.push_interleaved(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);

        let mut out = Vec::new();
        tap.pop_into(&mut out, usize::MAX);
        assert_eq!(out, vec![1.0, -1.0, 2.0, -2.0]);
        assert_eq!(tap.dropped_samples(), 2);

        // Channel alignment survives the drop: the next frame is still L, R.
        producer.push_interleaved(&[4.0, -4.0]);
        out.clear();
        tap.pop_into(&mut out, usize::MAX);
        assert_eq!(out, vec![4.0, -4.0]);
    }

    #[test]
    fn pop_into_never_splits_a_frame() {
        let (mut producer, mut tap) = small_ring(2, 64);
        tap.arm();
        producer.push_interleaved(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = Vec::new();
        assert_eq!(tap.pop_into(&mut out, 3), 2);
        assert_eq!(out, vec![1.0, 2.0]);
    }

    #[test]
    fn rearming_discards_audio_left_from_the_previous_session() {
        let (mut producer, mut tap) = small_ring(1, 64);
        tap.arm();
        producer.push_interleaved(&[9.0, 9.0]);
        tap.shared.armed.store(false, Ordering::Relaxed);

        tap.arm();
        producer.push_interleaved(&[1.0]);
        let mut out = Vec::new();
        tap.pop_into(&mut out, usize::MAX);
        assert_eq!(out, vec![1.0]);
    }

    #[test]
    fn settle_waits_for_in_flight_callback_blocks() {
        let (mut producer, mut tap) = small_ring(1, 64);
        tap.arm();
        let feeder = std::thread::spawn(move || {
            for _ in 0..50 {
                producer.push_interleaved(&[0.5]);
                std::thread::sleep(Duration::from_millis(1));
            }
            producer
        });
        std::thread::sleep(Duration::from_millis(5));
        assert!(tap.disarm_and_settle(Duration::from_secs(1)));
        let settled = tap.pending_samples();
        let _producer = feeder.join().expect("feeder thread panicked");
        assert_eq!(
            tap.pending_samples(),
            settled,
            "no samples may arrive after disarm settles"
        );
    }

    #[test]
    fn settle_gives_up_on_a_stalled_stream() {
        let (_producer, mut tap) = small_ring(1, 8);
        tap.arm();
        assert!(!tap.disarm_and_settle(Duration::from_millis(20)));
    }
}
