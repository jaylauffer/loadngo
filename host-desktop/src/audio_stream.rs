//! Music decode-ahead for the iOS and desktop backends, without a thread per
//! track.
//!
//! A music track is minutes long (`flutterrung.ogg` is 603 s, about 212 MB
//! once decoded), so it is decoded a little ahead of playback rather than
//! all at once. This used to be a dedicated decoder thread per track that
//! slept in 10 ms steps whenever its queue was full. Now:
//!
//! - [`TrackDecoder`] decodes into a bounded [`ChunkProducer`] until the queue
//!   is full or the track ends, then returns. It never waits.
//! - [`DecodeAhead`] runs it as a job on loadngo's offload pool whenever the
//!   queue has drained below its low-water mark. The music controller calls
//!   [`DecodeAhead::tick`] from `update`, and asks for frames often enough
//!   ([`TICK`]) that the queue never runs dry: a queue holds about two seconds
//!   of sound and is topped up from about one and a half.
//! - The audio callback reads through [`ChunkConsumer`], and hands each
//!   emptied chunk back through a second bounded channel for the decoder to
//!   refill, so steady playback allocates nothing on either side.
//!
//! The end of a track is the decoder going away: it drops its producer, and
//! the consumer reports [`Pop::Ended`] once the queue has drained. A queue
//! that is merely empty because a job has not run yet is [`Pop::Empty`],
//! which is silence, not the end -- the two were confused once on desktop
//! and cycled the playlist every couple of seconds.

use std::io::{Read, Seek};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lewton::inside_ogg::OggStreamReader;

/// Frames per chunk: about 85 ms at 48 kHz.
pub(crate) const CHUNK_FRAMES: usize = 4_096;
/// Chunks a queue holds: about two seconds at 48 kHz.
pub(crate) const QUEUE_CHUNKS: usize = 24;
/// Below this many queued chunks (about 1.4 s) a tick decodes more.
const LOW_WATER_CHUNKS: usize = 16;
/// How often a game playing music must call `update`: well inside the
/// margin between the low-water mark and an empty queue, and slow enough
/// that a game which otherwise idles wakes only four times a second.
pub(crate) const TICK: Duration = Duration::from_millis(250);

/// Interleaved stereo `f32` at the rate the decoder was given.
pub(crate) type Chunk = Vec<f32>;

/// Anything a track's bytes can be read from.
pub(crate) trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// Opens a track from the start; called again each time a looped track
/// wraps.
pub(crate) type Opener = Arc<dyn Fn() -> Result<Box<dyn ReadSeek>, String> + Send + Sync>;

/// The decoder's side of a chunk queue.
pub(crate) struct ChunkProducer {
    full: SyncSender<Chunk>,
    empty: Receiver<Chunk>,
    queued: Arc<AtomicUsize>,
}

/// The audio callback's side of a chunk queue.
pub(crate) struct ChunkConsumer {
    full: Receiver<Chunk>,
    empty: SyncSender<Chunk>,
    queued: Arc<AtomicUsize>,
    current: Chunk,
    read: usize,
}

/// One sample from [`ChunkConsumer::pop`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Pop {
    Sample(f32),
    /// Nothing queued yet; the decoder is still running.
    Empty,
    /// The decoder finished and everything it queued has been read.
    Ended,
}

/// A queue of `capacity` chunks, and the count of chunks waiting in it.
pub(crate) fn chunk_queue(capacity: usize) -> (ChunkProducer, ChunkConsumer, Arc<AtomicUsize>) {
    let (full_tx, full_rx) = sync_channel(capacity);
    // Room for every chunk in flight, so returning one never fails and the
    // callback never frees one.
    let (empty_tx, empty_rx) = sync_channel(capacity + 2);
    let queued = Arc::new(AtomicUsize::new(0));
    (
        ChunkProducer {
            full: full_tx,
            empty: empty_rx,
            queued: Arc::clone(&queued),
        },
        ChunkConsumer {
            full: full_rx,
            empty: empty_tx,
            queued: Arc::clone(&queued),
            current: Vec::new(),
            read: 0,
        },
        queued,
    )
}

impl ChunkProducer {
    /// An emptied chunk to refill, or a new one while the queue is filling
    /// for the first time.
    fn recycled(&self) -> Chunk {
        match self.empty.try_recv() {
            Ok(mut chunk) => {
                chunk.clear();
                chunk
            }
            Err(_) => Vec::with_capacity(CHUNK_FRAMES * 2),
        }
    }

    /// Queues `chunk`, or hands it back if the queue is full.
    pub(crate) fn offer(&self, chunk: Chunk) -> Result<(), Chunk> {
        match self.full.try_send(chunk) {
            Ok(()) => {
                self.queued.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            Err(TrySendError::Full(chunk) | TrySendError::Disconnected(chunk)) => Err(chunk),
        }
    }
}

impl ChunkConsumer {
    /// The next sample. Allocation-free and lock-free, so it is safe in an
    /// audio callback.
    pub(crate) fn pop(&mut self) -> Pop {
        if self.read >= self.current.len() {
            match self.full.try_recv() {
                Ok(chunk) => {
                    self.queued.fetch_sub(1, Ordering::AcqRel);
                    let emptied = std::mem::replace(&mut self.current, chunk);
                    if emptied.capacity() > 0 {
                        // Sized so this cannot be full; if it ever were, the
                        // chunk is simply dropped.
                        let _ = self.empty.try_send(emptied);
                    }
                    self.read = 0;
                    if self.current.is_empty() {
                        return Pop::Empty;
                    }
                }
                Err(TryRecvError::Empty) => return Pop::Empty,
                Err(TryRecvError::Disconnected) => return Pop::Ended,
            }
        }
        let sample = self.current[self.read];
        self.read += 1;
        Pop::Sample(sample)
    }
}

/// Decodes one track to interleaved stereo at a fixed output rate.
pub(crate) struct TrackDecoder {
    open: Opener,
    looped: bool,
    reader: OggStreamReader<Box<dyn ReadSeek>>,
    channels: usize,
    ratio: f64,
    /// Decoded frames the interpolator has not consumed yet.
    pending: Vec<f32>,
    position: f64,
    /// A chunk being filled, or one the full queue refused last time.
    chunk: Option<Chunk>,
    producer: ChunkProducer,
}

impl TrackDecoder {
    /// Opens the track. Cheap (headers only); call it on the pool.
    pub(crate) fn new(
        open: Opener,
        looped: bool,
        output_rate: f64,
        producer: ChunkProducer,
    ) -> Result<Self, String> {
        let reader =
            OggStreamReader::new(open()?).map_err(|err| format!("Failed to read audio: {err}"))?;
        let mut decoder = Self {
            open,
            looped,
            channels: 1,
            ratio: 1.0,
            reader,
            pending: Vec::new(),
            position: 0.0,
            chunk: None,
            producer,
        };
        decoder.adopt_stream(output_rate);
        Ok(decoder)
    }

    fn adopt_stream(&mut self, output_rate: f64) {
        self.channels = usize::from(self.reader.ident_hdr.audio_channels).max(1);
        self.ratio = f64::from(self.reader.ident_hdr.audio_sample_rate) / output_rate.max(1.0);
    }

    /// Decodes until the queue is full (`Ok(true)`: call again later) or the
    /// track has ended (`Ok(false)`: drop the decoder to signal the end).
    pub(crate) fn decode_ahead(&mut self, output_rate: f64) -> Result<bool, String> {
        loop {
            // Hand over a finished chunk first; stop once the queue is full.
            if let Some(chunk) = self.chunk.take() {
                if chunk.len() >= CHUNK_FRAMES * 2 {
                    if let Err(refused) = self.producer.offer(chunk) {
                        self.chunk = Some(refused);
                        return Ok(true);
                    }
                } else {
                    self.chunk = Some(chunk);
                }
            }
            let packet = self
                .reader
                .read_dec_packet_itl()
                .map_err(|err| format!("Failed to decode audio: {err}"))?;
            let Some(packet) = packet else {
                if self.looped {
                    self.reader = OggStreamReader::new((self.open)()?)
                        .map_err(|err| format!("Failed to reopen audio: {err}"))?;
                    self.adopt_stream(output_rate);
                    continue;
                }
                // The end: queue what is left. If the queue is full, keep it
                // for the next call rather than lose the tail.
                if let Some(chunk) = self.chunk.take() {
                    if !chunk.is_empty() {
                        if let Err(refused) = self.producer.offer(chunk) {
                            self.chunk = Some(refused);
                            return Ok(true);
                        }
                    }
                }
                return Ok(false);
            };
            self.push_packet(&packet);
        }
    }

    /// Converts one packet to stereo and interpolates it to the output rate
    /// into the current chunk. A chunk may run past `CHUNK_FRAMES`; it is
    /// handed over on the next pass.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    fn push_packet(&mut self, packet: &[i16]) {
        for frame in packet.chunks(self.channels) {
            let left = f32::from(frame[0]) / 32_768.0;
            let right = if self.channels > 1 {
                f32::from(frame[1]) / 32_768.0
            } else {
                left
            };
            self.pending.push(left);
            self.pending.push(right);
        }
        let mut chunk = match self.chunk.take() {
            Some(chunk) => chunk,
            None => self.producer.recycled(),
        };
        let frames = self.pending.len() / 2;
        while (self.position.floor() as usize) + 1 < frames {
            let index = self.position.floor() as usize;
            let fraction = (self.position - self.position.floor()) as f32;
            for channel in 0..2 {
                let a = self.pending[index * 2 + channel];
                let b = self.pending[(index + 1) * 2 + channel];
                chunk.push(a + (b - a) * fraction);
            }
            self.position += self.ratio;
        }
        self.chunk = Some(chunk);
        // Keep only the tail the interpolator still needs.
        let consumed = self.position.floor() as usize;
        if consumed > 0 {
            self.pending.drain(..consumed * 2);
            self.position -= consumed as f64;
        }
    }
}

/// Keeps one track's queue topped up from the offload pool.
pub(crate) struct DecodeAhead {
    decoder: Arc<Mutex<Option<TrackDecoder>>>,
    /// A job is queued or running for this track.
    busy: Arc<AtomicBool>,
    queued: Arc<AtomicUsize>,
    /// The rate this track decodes at, fixed for its life so its chunks
    /// agree; the mixer resamples if the device runs at another.
    rate: f64,
}

impl DecodeAhead {
    /// Opens `open` on the pool and starts filling the queue at once. Returns
    /// the consumer the mixer reads.
    pub(crate) fn start(
        open: Opener,
        looped: bool,
        rate: f64,
        label: String,
    ) -> (Self, ChunkConsumer) {
        let (producer, consumer, queued) = chunk_queue(QUEUE_CHUNKS);
        let decoder = Arc::new(Mutex::new(None));
        let busy = Arc::new(AtomicBool::new(true));
        let ahead = Self {
            decoder: Arc::clone(&decoder),
            busy: Arc::clone(&busy),
            queued,
            rate,
        };
        crate::offload::run_detached(move || {
            let mut slot = decoder.lock().unwrap_or_else(PoisonError::into_inner);
            match TrackDecoder::new(open, looped, rate, producer) {
                Ok(mut track) => {
                    if decode_step(&mut track, rate, &label) {
                        *slot = Some(track);
                    }
                }
                Err(error) => eprintln!("[loadngo/audio] music {label} failed to open: {error}"),
            }
            busy.store(false, Ordering::Release);
        });
        (ahead, consumer)
    }

    /// Decodes more on the pool if the queue is below its low-water mark and
    /// no job is already at it. Cheap; call it every `update`.
    pub(crate) fn tick(&self) {
        if self.queued.load(Ordering::Acquire) >= LOW_WATER_CHUNKS
            || self.busy.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let decoder = Arc::clone(&self.decoder);
        let busy = Arc::clone(&self.busy);
        let rate = self.rate;
        crate::offload::run_detached(move || {
            {
                let mut slot = decoder.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(track) = slot.as_mut() {
                    if !decode_step(track, rate, "track") {
                        // Dropping the decoder drops its producer: the end.
                        *slot = None;
                    }
                }
            }
            busy.store(false, Ordering::Release);
        });
    }

    /// Still decoding: the track has not reached its end.
    #[cfg_attr(target_os = "ios", allow(dead_code))]
    pub(crate) fn is_decoding(&self) -> bool {
        self.busy.load(Ordering::Acquire)
            || self
                .decoder
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
    }
}

/// One fill; false once the track is over (ended or failed).
fn decode_step(track: &mut TrackDecoder, rate: f64, label: &str) -> bool {
    match track.decode_ahead(rate) {
        Ok(more) => more,
        Err(error) => {
            eprintln!("[loadngo/audio] music {label} stopped: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_of(value: f32, frames: usize) -> Chunk {
        vec![value; frames * 2]
    }

    #[test]
    fn an_empty_queue_with_a_live_decoder_is_not_the_end() {
        let (producer, mut consumer, _) = chunk_queue(4);
        assert_eq!(consumer.pop(), Pop::Empty);
        drop(producer);
        assert_eq!(consumer.pop(), Pop::Ended);
    }

    #[test]
    fn samples_come_out_in_order_and_the_end_follows_the_last() {
        let (producer, mut consumer, queued) = chunk_queue(4);
        producer.offer(vec![0.1, 0.2]).expect("room");
        producer.offer(vec![0.3, 0.4]).expect("room");
        assert_eq!(queued.load(Ordering::SeqCst), 2);
        drop(producer);
        let heard: Vec<Pop> = (0..5).map(|_| consumer.pop()).collect();
        assert_eq!(
            heard,
            [
                Pop::Sample(0.1),
                Pop::Sample(0.2),
                Pop::Sample(0.3),
                Pop::Sample(0.4),
                Pop::Ended
            ]
        );
        assert_eq!(queued.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_full_queue_refuses_rather_than_blocks() {
        let (producer, _consumer, _) = chunk_queue(2);
        producer.offer(chunk_of(0.0, 1)).expect("room");
        producer.offer(chunk_of(0.0, 1)).expect("room");
        assert!(producer.offer(chunk_of(0.0, 1)).is_err());
    }

    #[test]
    fn emptied_chunks_are_recycled_not_reallocated() {
        let (producer, mut consumer, _) = chunk_queue(2);
        let chunk = producer.recycled();
        let address = chunk.as_ptr();
        let mut chunk = chunk;
        chunk.extend_from_slice(&[0.5, 0.5]);
        producer.offer(chunk).expect("room");
        producer.offer(vec![0.25, 0.25]).expect("room");
        // Reading past the first chunk hands it back.
        for _ in 0..3 {
            consumer.pop();
        }
        let reused = producer.recycled();
        assert_eq!(reused.as_ptr(), address, "the same buffer came back");
        assert!(reused.is_empty());
    }

    /// A real Ogg file through the decoder: every sample arrives, at the
    /// requested rate, and the decoder reports the end only after the last
    /// chunk is queued.
    #[test]
    fn a_track_decodes_completely_across_many_fills() {
        let bytes: &'static [u8] = include_bytes!("../tests/assets/short-a.ogg");
        let open: Opener =
            Arc::new(move || Ok(Box::new(std::io::Cursor::new(bytes)) as Box<dyn ReadSeek>));
        let (producer, mut consumer, _) = chunk_queue(3);
        let mut decoder = TrackDecoder::new(open, false, 48_000.0, producer).expect("open");
        let mut frames = 0usize;
        let mut fills = 0;
        loop {
            let more = decoder.decode_ahead(48_000.0).expect("decode");
            fills += 1;
            loop {
                match consumer.pop() {
                    Pop::Sample(_) => frames += 1,
                    Pop::Empty => break,
                    Pop::Ended => unreachable!("the decoder still holds its producer"),
                }
            }
            if !more {
                break;
            }
            assert!(fills < 10_000, "the decoder must reach the end");
        }
        drop(decoder);
        assert_eq!(consumer.pop(), Pop::Ended);
        assert!(
            fills > 1,
            "a 3 s track needs more than one fill of 3 chunks"
        );
        let seconds = frames as f64 / 2.0 / 48_000.0;
        assert!(
            (seconds - 3.0).abs() < 0.1,
            "decoded {seconds:.2} s of a 3 s track"
        );
    }
}
