//! Desktop audio playback (macOS, Linux, Windows) on loadngo's own output
//! seam, `loadngo-audio-io`: CoreAudio, ALSA, and cpal on Windows until a
//! WASAPI backend exists. The only desktop backend since 2026-10-13, when it
//! replaced rodio; on macOS it had been checked against rodio first, trace for
//! trace and acoustically (`docs/AUDIO_BACKENDS.md`).
//!
//! The game thread never waits on audio (`docs/GAME_AUDIO_RUNTIME.md`):
//!
//! - The device is opened and closed on loadngo's offload pool
//!   (`audio_output_idle.rs`), and closes after three silent seconds.
//! - Effects and voice lines are decoded on the pool, once, into a shared
//!   cache. An effect played before its clip is ready is dropped and
//!   counted, never decoded inline; a voice line starts when its decode
//!   finishes.
//! - Music is decoded a little ahead on the pool (`audio_stream.rs`), topped
//!   up from the music controller's `update`, so no track owns a thread.
//!
//! Clips keep the sample rate they were recorded at, and each track keeps the
//! rate it was decoded at; the mixer resamples whatever differs from the
//! device. That is what lets the device open late, close, and reopen on
//! another device at another rate without anything being decoded again.
//!
//! Desktop needs three things the mobile backends do not, all of which the
//! games rely on: several tracks playing at once so one can fade out under
//! another, a cue/resume state machine so a one-off cue returns to the
//! playlist, and playback from `&'static [u8]` for callers that embed their
//! audio.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufReader, Cursor};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use lewton::inside_ogg::OggStreamReader;
use loadngo_audio_io::{open_output_stream, OutputStream};

use super::output_idle::{sooner, IdlePolicy, OutputSwitch};
use super::stream::{ChunkConsumer, DecodeAhead, Opener, Pop, ReadSeek, TICK};
use super::{finite_clamped, oldest_evictable_voice, SfxPlayRequest, SfxSettings, SfxVoiceId};

/// The music bus's bass shelf and post-gain, kept from the rodio backend this
/// replaced so the games sound as they did. The shelf is applied to the
/// summed music bus rather than per source: rodio mixed a low-passed clone
/// into every track, and its own comment recorded that doing so cost
/// per-sample lock contention on the audio thread. One filter over the mix is
/// cheaper and audibly equivalent at these settings.
const MUSIC_BASS_CUTOFF_HZ: f32 = 180.0;
const MUSIC_BASS_POST_GAIN: f32 = 0.9;
/// A track that reports "finished" the instant it starts must not spin the
/// playlist. Mirrors the Android backend's identical guard.
const PLAYLIST_ADVANCE_DEBOUNCE: Duration = Duration::from_secs(2);
/// The rate music decodes at before the device has reported its own.
const FALLBACK_RATE: u32 = 48_000;
/// Voice-line ids live in the top half of the id space so they never collide
/// with effect voices in the shared mixer.
const VOICE_LINE_ID_BIT: u64 = 1 << 63;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicCueMode {
    OneShot,
    Loop,
}

// ---------------------------------------------------------------- sources

/// Where a track's or clip's bytes come from. Desktop supports both, unlike
/// the mobile backends: those have a packaging story that always yields a
/// real path.
#[derive(Clone)]
enum TrackSource {
    Path(String),
    Embedded(&'static [u8]),
}

impl TrackSource {
    fn open(&self) -> Result<Box<dyn ReadSeek>, String> {
        match self {
            Self::Path(path) => {
                let file =
                    File::open(path).map_err(|err| format!("Missing audio {path}: {err}"))?;
                Ok(Box::new(BufReader::new(file)))
            }
            Self::Embedded(bytes) => Ok(Box::new(Cursor::new(*bytes))),
        }
    }

    fn opener(&self) -> Opener {
        let source = self.clone();
        Arc::new(move || source.open())
    }
}

// ------------------------------------------------------------------ clips

/// A whole effect or voice line, decoded once and kept: interleaved stereo
/// at the rate it was recorded at.
#[derive(Clone)]
struct CachedClip {
    samples: Arc<Vec<f32>>,
    rate: f64,
}

/// Decodes a whole clip. Runs on the offload pool.
fn decode_clip(source: &TrackSource) -> Result<CachedClip, String> {
    let mut reader = OggStreamReader::new(source.open()?)
        .map_err(|err| format!("Failed to read clip: {err}"))?;
    let rate = f64::from(reader.ident_hdr.audio_sample_rate);
    let channels = usize::from(reader.ident_hdr.audio_channels).max(1);
    let mut interleaved: Vec<f32> = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|err| format!("Failed to decode clip: {err}"))?
    {
        for frame in packet.chunks(channels) {
            let left = f32::from(frame[0]) / 32_768.0;
            let right = if channels > 1 {
                f32::from(frame[1]) / 32_768.0
            } else {
                left
            };
            interleaved.push(left);
            interleaved.push(right);
        }
    }
    if interleaved.is_empty() {
        return Err("clip contains no audio samples".to_string());
    }
    Ok(CachedClip {
        samples: Arc::new(interleaved),
        rate,
    })
}

#[derive(Clone)]
enum ClipState {
    Loading,
    Ready(CachedClip),
    Failed(String),
}

/// Every effect clip, by the key `play` names it with.
static CLIPS: OnceLock<Mutex<HashMap<String, ClipState>>> = OnceLock::new();

fn clips() -> &'static Mutex<HashMap<String, ClipState>> {
    CLIPS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Starts decoding `source` on the pool under `key`, unless it is already
/// loaded or loading. Returns at once.
fn request_clip(key: &str, source: TrackSource) {
    {
        let mut cache = clips().lock().unwrap_or_else(PoisonError::into_inner);
        if cache.contains_key(key) {
            return;
        }
        cache.insert(key.to_string(), ClipState::Loading);
    }
    let key = key.to_string();
    crate::offload::run_detached(move || {
        let state = match decode_clip(&source) {
            Ok(clip) => ClipState::Ready(clip),
            Err(error) => ClipState::Failed(format!("{key}: {error}")),
        };
        clips()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, state);
    });
}

// ------------------------------------------------------------------ mixing

/// One playing track. Several exist at once so a cue can fade in while the
/// previous track fades out -- the capability the mobile backends lack.
struct MusicTrack {
    key: String,
    chunks: ChunkConsumer,
    /// Keeps the queue topped up; `None` only in tests, which fill the queue
    /// by hand.
    ahead: Option<DecodeAhead>,
    /// The rate the track decodes at.
    rate: f64,
    /// Linear interpolation state, used only when `rate` differs from the
    /// device's: the read position between the two frames either side.
    frac: f64,
    previous: [f32; 2],
    next: [f32; 2],
    /// Ramped on the game thread by `update`, read by the audio thread.
    current_volume: f32,
    target_volume: f32,
    playing: bool,
    finished: bool,
}

impl MusicTrack {
    fn new(key: String, chunks: ChunkConsumer, ahead: Option<DecodeAhead>, rate: f64) -> Self {
        Self {
            key,
            chunks,
            ahead,
            rate,
            frac: 1.0,
            previous: [0.0; 2],
            next: [0.0; 2],
            current_volume: 0.0,
            target_volume: 1.0,
            playing: true,
            finished: false,
        }
    }

    /// Not finished and either still decoding or still meant to be heard.
    fn is_sounding(&self) -> bool {
        !self.finished
            && (self.target_volume > 0.0
                || self.ahead.as_ref().is_some_and(DecodeAhead::is_decoding))
    }

    /// The next stereo frame; `None` if nothing is queued (silence) or the
    /// track has ended (and `finished` is now set).
    fn pop_frame(&mut self) -> Option<[f32; 2]> {
        let left = match self.chunks.pop() {
            Pop::Sample(sample) => sample,
            Pop::Empty => return None,
            Pop::Ended => {
                self.finished = true;
                return None;
            }
        };
        match self.chunks.pop() {
            Pop::Sample(right) => Some([left, right]),
            Pop::Empty | Pop::Ended => Some([left, left]),
        }
    }

    /// The next output frame at `device_rate`.
    fn frame_at(&mut self, device_rate: f64) -> Option<[f32; 2]> {
        let step = if device_rate > 0.0 {
            self.rate / device_rate
        } else {
            1.0
        };
        if (step - 1.0).abs() < 1e-9 {
            return self.pop_frame();
        }
        while self.frac >= 1.0 {
            let next = self.pop_frame()?;
            self.previous = self.next;
            self.next = next;
            self.frac -= 1.0;
        }
        #[allow(clippy::cast_possible_truncation)]
        let fraction = self.frac as f32;
        self.frac += step;
        Some([
            self.previous[0] + (self.next[0] - self.previous[0]) * fraction,
            self.previous[1] + (self.next[1] - self.previous[1]) * fraction,
        ])
    }
}

struct ActiveVoice {
    samples: Arc<Vec<f32>>,
    /// Fractional *frame* position rather than a sample index, so playback
    /// rate is honoured (rodio's `sink.set_speed` did; the games use it).
    position: f64,
    rate: f64,
    /// The rate `samples` were recorded at; see [`CachedClip::rate`].
    source_rate: f64,
    left: f32,
    right: f32,
    looped: bool,
}

#[derive(Default)]
struct MixerState {
    tracks: Vec<MusicTrack>,
    music_mix_volume: f32,
    /// Music-only pause, and deliberately not a zeroed volume: muting would
    /// keep the queue draining, so a resume would land further into the track
    /// and a long pause could advance the playlist entirely.
    paused: bool,
    bass_boost: f32,
    bass_state: [f32; 2],
    bass_coefficient: f32,
    voices: Vec<(u64, ActiveVoice)>,
    /// The rate the device is running at now; 0 before it first opens.
    device_rate: f64,
}

impl MixerState {
    /// Nothing left to play: no effect, and every track ended or faded
    /// out. A paused track that has not ended still counts as something to
    /// play, so pausing music never closes the output under it.
    fn is_quiet(&self) -> bool {
        self.voices.is_empty()
            && self.tracks.iter().all(|track| {
                track.finished || (track.target_volume <= 0.0 && track.current_volume <= 0.001)
            })
    }

    fn fill(&mut self, out: &mut [f32], channels: usize) {
        out.fill(0.0);
        self.mix_music(out, channels);
        self.mix_voices(out, channels);
        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }

    fn mix_music(&mut self, out: &mut [f32], channels: usize) {
        if self.paused {
            // Return *without* reading the queue: it fills, the decoder
            // stops topping it up, and the track holds its position. That is
            // what makes this a pause rather than a mute. Effects still play;
            // this pauses music only.
            return;
        }
        let stride = channels.max(1);
        let mix = self.music_mix_volume;
        let boost = self.bass_boost;
        let coefficient = self.bass_coefficient;
        let device_rate = self.device_rate;
        for track in &mut self.tracks {
            if track.finished || !track.playing || track.current_volume <= 0.0 {
                continue;
            }
            let volume = track.current_volume * mix;
            for frame in out.chunks_mut(stride) {
                let Some([left, right]) = track.frame_at(device_rate) else {
                    break;
                };
                frame[0] += left * volume;
                if let Some(second) = frame.get_mut(1) {
                    *second += right * volume;
                }
            }
        }

        // One shelf over the summed bus, then the post-gain the rodio
        // backend applied unconditionally to music.
        for (index, sample) in out.iter_mut().enumerate() {
            let channel = index % stride % 2;
            let low = self.bass_state[channel] + coefficient * (*sample - self.bass_state[channel]);
            self.bass_state[channel] = low;
            *sample = (*sample + low * boost) * MUSIC_BASS_POST_GAIN;
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn mix_voices(&mut self, out: &mut [f32], channels: usize) {
        let stride = channels.max(1);
        let device_rate = self.device_rate;
        self.voices.retain_mut(|(_, voice)| {
            let step = if device_rate > 0.0 && voice.source_rate > 0.0 {
                voice.rate * voice.source_rate / device_rate
            } else {
                voice.rate
            };
            let frames = voice.samples.len() / 2;
            if frames < 2 {
                return false;
            }
            let mut index = 0;
            while index + 1 < out.len() {
                let mut frame = voice.position.floor() as usize;
                if frame + 1 >= frames {
                    if !voice.looped {
                        return false;
                    }
                    voice.position = 0.0;
                    frame = 0;
                }
                let fraction = (voice.position - voice.position.floor()) as f32;
                let left = voice.samples[frame * 2];
                let right = voice.samples[frame * 2 + 1];
                let next_left = voice.samples[(frame + 1) * 2];
                let next_right = voice.samples[(frame + 1) * 2 + 1];
                out[index] += (left + (next_left - left) * fraction) * voice.left;
                out[index + 1] += (right + (next_right - right) * fraction) * voice.right;
                voice.position += step;
                index += stride;
            }
            true
        });
    }
}

/// Pan law shared with the Android and iOS backends: a linear left/right
/// split.
fn stereo_volume(volume: f32, pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    (volume * (1.0 - pan.max(0.0)), volume * (1.0 + pan.min(0.0)))
}

// ------------------------------------------------------------------ output

/// The live stream, kept only so that dropping it stops the device; `None`
/// while the output is closed.
///
/// It sits behind a `Mutex` rather than in a plain `static` because the
/// CoreAudio backend's handle is `Send` but deliberately not `Sync`, and a
/// `static` demands `Sync`. `Mutex<T>` supplies that for any `T: Send`, so
/// this module never has to assert anything about the backend's internals --
/// which it is in no position to know.
static STREAM: Mutex<Option<OutputStream>> = Mutex::new(None);
static MIXER: OnceLock<Arc<Mutex<MixerState>>> = OnceLock::new();
/// The device's rate, once it has opened; 0 before that.
static DEVICE_RATE: AtomicU32 = AtomicU32::new(0);
/// When the output opens and closes (see `audio_output_idle.rs`). It starts
/// closed and opens on the first sound.
static IDLE: Mutex<IdlePolicy> = Mutex::new(IdlePolicy::new(false));
static SWITCH: OnceLock<OutputSwitch> = OnceLock::new();
/// A failed open is reported once, not on every sound that retries it.
static OPEN_FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

fn mixer() -> &'static Arc<Mutex<MixerState>> {
    MIXER.get_or_init(|| {
        Arc::new(Mutex::new(MixerState {
            music_mix_volume: 1.0,
            ..MixerState::default()
        }))
    })
}

fn switch() -> &'static OutputSwitch {
    SWITCH.get_or_init(|| OutputSwitch::new(false, apply_output))
}

/// `LOADNGO_DISABLE_AUDIO` turns playback off entirely: it is how a headless
/// or shared machine runs these apps at all.
fn audio_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        matches!(
            std::env::var("LOADNGO_DISABLE_AUDIO").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        )
    })
}

/// Opens the default device with the mixer as its callback, and records the
/// rate it runs at. Runs on the offload pool.
fn open_stream() -> Result<OutputStream, String> {
    let shared = Arc::clone(mixer());
    let stream = open_output_stream(None, None, move |out, channels| {
        match shared.try_lock() {
            Ok(mut state) => state.fill(out, channels),
            // Never block the audio thread: one silent buffer costs far
            // less than a stall, and the only other holders are short
            // game-thread calls.
            Err(_) => out.fill(0.0),
        }
    })
    .map_err(|error| format!("Audio output unavailable: {error}"))?;
    let sample_rate_hz = stream.sample_rate_hz();
    DEVICE_RATE.store(sample_rate_hz, Ordering::Release);
    if let Ok(mut state) = mixer().lock() {
        state.bass_coefficient = one_pole_coefficient(sample_rate_hz);
        state.device_rate = f64::from(sample_rate_hz);
    }
    Ok(stream)
}

/// Runs on the offload pool: brings the device to `open`.
fn apply_output(open: bool) {
    let mut held = STREAM.lock().unwrap_or_else(PoisonError::into_inner);
    if !open {
        // Dropping the stream stops the device; about 40 ms on the Mac mini,
        // which is why this is not on the game thread.
        drop(held.take());
        return;
    }
    if held.is_some() {
        return;
    }
    match open_stream() {
        Ok(stream) => *held = Some(stream),
        Err(detail) => {
            drop(held);
            if !OPEN_FAILURE_REPORTED.swap(true, Ordering::AcqRel) {
                eprintln!("[loadngo/desktop] {detail}; will try again on the next sound");
            }
            IDLE.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .open_failed();
            // Nothing will play what was queued, and a voice that never
            // finishes would keep its controller asking for frames.
            if let Ok(mut state) = mixer().lock() {
                state.voices.clear();
                state.tracks.clear();
            }
        }
    }
}

/// Something is about to play: open the output if it is closed. Returns at
/// once; the open happens on the pool, and what was queued starts as soon as
/// the device does (8-11 ms on the Mac mini, about 80 ms the first time).
fn wake_output() {
    if audio_disabled() {
        return;
    }
    let opening = IDLE.lock().unwrap_or_else(PoisonError::into_inner).wake();
    if opening {
        switch().request(true);
    }
}

/// Closes the output once the mixer has been silent for `IDLE_STOP`. Called
/// from every controller's `update`, on the game thread; cheap.
fn maintain_output() {
    if audio_disabled() {
        return;
    }
    let quiet = mixer().lock().is_ok_and(|state| state.is_quiet());
    let closing = IDLE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .observe(quiet, Instant::now());
    if closing {
        switch().request(false);
    }
}

/// When the game must call `update` again for the output to close, folded
/// into every controller's `frame_demand`.
fn output_demand() -> Option<Duration> {
    if audio_disabled() {
        return None;
    }
    let quiet = mixer().lock().is_ok_and(|state| state.is_quiet());
    IDLE.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .demand(quiet, Instant::now())
}

/// One-pole coefficient for [`MUSIC_BASS_CUTOFF_HZ`] at `sample_rate_hz`.
fn one_pole_coefficient(sample_rate_hz: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let rate = sample_rate_hz.max(1) as f32;
    1.0 - (-std::f32::consts::TAU * MUSIC_BASS_CUTOFF_HZ / rate).exp()
}

/// The rate a new music track decodes at: the device's, once it is known.
fn music_rate() -> f64 {
    match DEVICE_RATE.load(Ordering::Acquire) {
        0 => f64::from(FALLBACK_RATE),
        rate => f64::from(rate),
    }
}

// ------------------------------------------------------------- controllers

pub struct MusicController {
    sources: HashMap<String, TrackSource>,
    fade_duration: f32,
    mix_volume: f32,
    bass_boost: f32,
    active_track: Option<String>,
    track_started_at: Option<Instant>,
    playlist_mode_active: bool,
    resume_playlist_after_cue: bool,
    resume_playlist_from_next_track: bool,
    cue_mode: MusicCueMode,
    playlist_tracks: Vec<String>,
    playlist_index: usize,
    boot_track_path: String,
    paused: bool,
}

impl MusicController {
    pub fn new(
        boot_track_path: String,
        playlist_tracks: Vec<String>,
        cue_mode: MusicCueMode,
        bass_boost: f32,
    ) -> Self {
        Self {
            sources: HashMap::new(),
            fade_duration: 1.0,
            mix_volume: 1.0,
            bass_boost: bass_boost.clamp(0.0, 1.0),
            active_track: None,
            track_started_at: None,
            playlist_mode_active: false,
            resume_playlist_after_cue: false,
            resume_playlist_from_next_track: false,
            cue_mode,
            playlist_tracks,
            playlist_index: 0,
            boot_track_path,
            paused: false,
        }
    }

    pub fn play_track_path(&mut self, path: &str, fade: f32, looped: bool) -> Result<(), String> {
        let selected = if path.trim().is_empty() {
            self.boot_track_path.clone()
        } else {
            path.trim().to_string()
        };
        if audio_disabled() {
            return Err("Audio disabled by LOADNGO_DISABLE_AUDIO".to_string());
        }
        wake_output();
        self.fade_duration = fade.max(0.05);

        let source = self
            .sources
            .get(&selected)
            .cloned()
            .unwrap_or_else(|| TrackSource::Path(selected.clone()));
        let rate = music_rate();
        let (ahead, chunks) = DecodeAhead::start(source.opener(), looped, rate, selected.clone());

        if let Ok(mut state) = mixer().lock() {
            // Everything already playing fades out; the new track fades in.
            for track in &mut state.tracks {
                track.target_volume = 0.0;
            }
            state.tracks.retain(|track| track.current_volume > 0.001);
            state
                .tracks
                .push(MusicTrack::new(selected.clone(), chunks, Some(ahead), rate));
            state.music_mix_volume = self.mix_volume;
            state.paused = false;
            state.bass_boost = self.bass_boost;
        }
        self.active_track = Some(selected);
        self.track_started_at = Some(Instant::now());
        self.paused = false;
        Ok(())
    }

    pub fn start_playlist(&mut self, fade: f32) -> Result<(), String> {
        self.playlist_index = 0;
        self.playlist_mode_active = true;
        self.resume_playlist_after_cue = false;
        self.resume_playlist_from_next_track = false;
        self.play_playlist_current(fade)
    }

    pub fn fade_to_path(&mut self, path: &str, fade: f32) -> Result<(), String> {
        self.resume_playlist_after_cue = !self.playlist_tracks.is_empty();
        self.resume_playlist_from_next_track = self.playlist_mode_active;
        self.playlist_mode_active = false;
        let looped = self.playlist_tracks.is_empty() && self.cue_mode == MusicCueMode::Loop;
        self.play_track_path(path, fade, looped)
    }

    /// Registers bytes to play under `key` instead of opening a file. Unlike
    /// the mobile backends, desktop callers genuinely use this: they have no
    /// packaging step that guarantees a real path.
    pub fn preload_embedded(&mut self, key: &str, ogg_bytes: &'static [u8]) {
        self.sources
            .insert(key.to_string(), TrackSource::Embedded(ogg_bytes));
    }

    /// Ramps fades, tops up each track's decode-ahead, advances the
    /// playlist, and lets the output close once everything is silent. Must be
    /// called every frame while music plays: the decode-ahead depends on it,
    /// and [`Self::frame_demand`] asks for frames often enough.
    pub fn update(&mut self, dt: f32) {
        self.update_tracks(dt);
        maintain_output();
    }

    fn update_tracks(&mut self, dt: f32) {
        let fade = self.fade_duration.max(0.001);
        let finished = {
            let Ok(mut state) = mixer().lock() else {
                return;
            };
            for track in &state.tracks {
                if let Some(ahead) = &track.ahead {
                    ahead.tick();
                }
            }
            if self.paused {
                return;
            }
            for track in &mut state.tracks {
                let difference = track.target_volume - track.current_volume;
                if difference.abs() <= 0.001 {
                    track.current_volume = track.target_volume;
                } else {
                    let step = (dt / fade).min(difference.abs());
                    track.current_volume += step * difference.signum();
                }
                track.current_volume = track.current_volume.clamp(0.0, 1.0);
            }
            // Retire tracks that faded out.
            state
                .tracks
                .retain(|track| !(track.target_volume <= 0.0 && track.current_volume <= 0.001));
            let active = self.active_track.clone();
            // `is_some_and`, not `is_none_or`: a track missing from the mixer
            // is not a track that ended. The `retain` above can remove one
            // that faded out, and treating that absence as "finished" would
            // advance the playlist for a reason unrelated to playback.
            state
                .tracks
                .iter()
                .find(|track| Some(&track.key) == active.as_ref())
                .is_some_and(|track| track.finished)
        };
        if !finished {
            return;
        }

        if !self.playlist_mode_active {
            if self.resume_playlist_after_cue {
                self.resume_playlist_after_cue = false;
                self.playlist_mode_active = true;
                let result = if self.resume_playlist_from_next_track {
                    self.play_next_playlist(0.1)
                } else {
                    self.play_playlist_current(0.1)
                };
                if let Err(error) = result {
                    eprintln!("[loadngo/desktop] playlist resume failed: {error}");
                }
                self.resume_playlist_from_next_track = false;
            }
            return;
        }
        if self
            .track_started_at
            .is_some_and(|started| started.elapsed() < PLAYLIST_ADVANCE_DEBOUNCE)
        {
            return;
        }
        if let Err(error) = self.play_next_playlist(0.1) {
            eprintln!("[loadngo/desktop] playlist advance failed: {error}");
        }
    }

    fn play_playlist_current(&mut self, fade: f32) -> Result<(), String> {
        let path = self
            .playlist_tracks
            .get(self.playlist_index)
            .cloned()
            .unwrap_or_else(|| self.boot_track_path.clone());
        self.play_track_path(&path, fade, false)
    }

    fn play_next_playlist(&mut self, fade: f32) -> Result<(), String> {
        if self.playlist_tracks.is_empty() {
            let boot = self.boot_track_path.clone();
            return self.play_track_path(&boot, fade, false);
        }
        self.playlist_index = (self.playlist_index + 1) % self.playlist_tracks.len();
        self.play_playlist_current(fade)
    }

    pub fn pause(&mut self) {
        self.paused = true;
        if let Ok(mut state) = mixer().lock() {
            state.paused = true;
        }
    }

    pub fn resume(&mut self) {
        self.paused = false;
        if let Ok(mut state) = mixer().lock() {
            state.paused = false;
            state.music_mix_volume = self.mix_volume;
        }
    }

    pub fn set_mix_volume(&mut self, volume: f32) {
        self.mix_volume = finite_clamped(volume, 0.0, 2.0, 1.0);
        if let Ok(mut state) = mixer().lock() {
            state.music_mix_volume = self.mix_volume;
        }
    }

    pub fn set_bass_boost(&mut self, bass_boost: f32) {
        self.bass_boost = bass_boost.clamp(0.0, 1.0);
        if let Ok(mut state) = mixer().lock() {
            state.bass_boost = self.bass_boost;
        }
    }

    #[must_use]
    pub fn active_track(&self) -> Option<&str> {
        self.active_track.as_deref()
    }

    /// Every 16 ms while a fade ramps; every [`TICK`] while any track is
    /// still decoding or meant to be heard, so its queue is topped up and its
    /// end noticed; then once more when the output can close.
    #[must_use]
    pub fn frame_demand(&self) -> Option<Duration> {
        let (fading, sounding) = mixer().lock().map_or((false, false), |state| {
            let fading = state
                .tracks
                .iter()
                .any(|track| (track.current_volume - track.target_volume).abs() > 0.001);
            let sounding = state.tracks.iter().any(MusicTrack::is_sounding);
            (fading, sounding)
        });
        if fading {
            return Some(Duration::from_millis(16));
        }
        if sounding {
            return Some(TICK);
        }
        output_demand()
    }
}

pub struct VoiceController {
    enabled: bool,
    volume: f32,
    voice: Option<u64>,
    /// The id of a voice line still decoding on the pool, 0 if none. The
    /// decode job starts the line only if it is still the one wanted.
    pending: Arc<AtomicU64>,
    next_id: u64,
}

impl VoiceController {
    pub fn new(enabled: bool, volume: f32) -> Self {
        Self {
            enabled,
            volume: finite_clamped(volume, 0.0, 2.0, 1.0),
            voice: None,
            pending: Arc::new(AtomicU64::new(0)),
            next_id: 1,
        }
    }

    /// Decodes `path` on the pool and plays it when ready, replacing any
    /// line playing now. Returns at once.
    pub fn play_path(&mut self, path: &str) -> Result<(), String> {
        if !self.enabled || audio_disabled() {
            return Ok(());
        }
        self.stop();
        wake_output();
        let id = self.allocate();
        self.voice = Some(id);
        self.pending.store(id, Ordering::Release);
        let pending = Arc::clone(&self.pending);
        let volume = self.volume;
        let path = path.to_string();
        crate::offload::run_detached(move || {
            let decoded = decode_clip(&TrackSource::Path(path.clone()));
            // Still wanted? A stop or a newer line clears or replaces `pending`.
            if pending
                .compare_exchange(id, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }
            match decoded {
                Ok(clip) => {
                    if let Ok(mut state) = mixer().lock() {
                        state.voices.push((
                            id,
                            ActiveVoice {
                                samples: clip.samples,
                                position: 0.0,
                                rate: 1.0,
                                source_rate: clip.rate,
                                left: volume,
                                right: volume,
                                looped: false,
                            },
                        ));
                    }
                }
                Err(error) => eprintln!("[loadngo/desktop] voice {path}: {error}"),
            }
        });
        Ok(())
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = finite_clamped(volume, 0.0, 2.0, 1.0);
    }

    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        self.enabled = enabled;
        if !enabled {
            self.stop();
        }
        self.enabled
    }

    /// True while the line decodes or plays.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        let Some(id) = self.voice else { return false };
        self.pending.load(Ordering::Acquire) == id
            || mixer()
                .lock()
                .is_ok_and(|state| state.voices.iter().any(|(voice, _)| *voice == id))
    }

    pub fn stop(&mut self) {
        let Some(id) = self.voice.take() else { return };
        let _ = self
            .pending
            .compare_exchange(id, 0, Ordering::AcqRel, Ordering::Acquire);
        if let Ok(mut state) = mixer().lock() {
            state.voices.retain(|(voice, _)| *voice != id);
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn frame_demand(&self) -> Option<Duration> {
        sooner(
            self.is_playing().then_some(Duration::from_millis(100)),
            output_demand(),
        )
    }

    fn allocate(&mut self) -> u64 {
        let id = self.next_id | VOICE_LINE_ID_BIT;
        self.next_id = self.next_id.wrapping_add(1).max(1) & !VOICE_LINE_ID_BIT;
        id
    }
}

pub struct SfxController {
    settings: SfxSettings,
    voices: HashMap<SfxVoiceId, u8>,
    order: VecDeque<SfxVoiceId>,
    next_voice_id: u64,
    /// Plays dropped because their clip had not finished decoding.
    not_ready: u64,
}

impl SfxController {
    pub fn new(settings: SfxSettings) -> Self {
        Self {
            settings: settings.normalized(),
            voices: HashMap::new(),
            order: VecDeque::new(),
            next_voice_id: 1,
            not_ready: 0,
        }
    }

    /// Starts decoding `ogg_bytes` on the pool under `key`, so a later `play`
    /// with `path: key` finds it ready. Returns at once.
    pub fn preload_embedded(&mut self, key: &str, ogg_bytes: &'static [u8]) -> Result<(), String> {
        if !audio_disabled() {
            request_clip(key, TrackSource::Embedded(ogg_bytes));
        }
        Ok(())
    }

    /// Starts decoding the file at `path` on the pool. Returns at once.
    pub fn preload_path(&mut self, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Err("SFX path must not be empty".to_string());
        }
        if !audio_disabled() {
            request_clip(path, TrackSource::Path(path.to_string()));
        }
        Ok(())
    }

    /// Admits a voice and returns; never decodes. A clip that is not ready
    /// yet is started loading and this play is dropped (`Ok(None)`), as on
    /// Android: preload clips at startup so that never happens in play.
    pub fn play(&mut self, request: SfxPlayRequest<'_>) -> Result<Option<SfxVoiceId>, String> {
        let request = request.normalized();
        if request.path.is_empty() {
            return Err("SFX path must not be empty".to_string());
        }
        if !self.settings.enabled || audio_disabled() {
            return Ok(None);
        }
        let state = clips()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(request.path)
            .cloned();
        let clip = match state {
            Some(ClipState::Ready(clip)) => clip,
            Some(ClipState::Failed(error)) => return Err(error),
            Some(ClipState::Loading) => {
                self.not_ready += 1;
                return Ok(None);
            }
            None => {
                self.not_ready += 1;
                request_clip(request.path, TrackSource::Path(request.path.to_string()));
                return Ok(None);
            }
        };
        self.update();
        if !self.make_voice_capacity(request.priority) {
            return Ok(None);
        }
        wake_output();
        let (left, right) = stereo_volume(request.volume * self.settings.mix_volume, request.pan);
        let id = self.allocate_voice_id();
        {
            let Ok(mut state) = mixer().lock() else {
                return Ok(None);
            };
            state.voices.push((
                id.value(),
                ActiveVoice {
                    samples: clip.samples,
                    position: 0.0,
                    rate: f64::from(request.playback_rate),
                    source_rate: clip.rate,
                    left,
                    right,
                    looped: request.looped,
                },
            ));
        }
        self.voices.insert(id, request.priority);
        self.order.push_back(id);
        Ok(Some(id))
    }

    pub fn stop(&mut self, voice: SfxVoiceId) {
        if let Ok(mut state) = mixer().lock() {
            state.voices.retain(|(id, _)| *id != voice.value());
        }
        self.voices.remove(&voice);
        self.order.retain(|candidate| *candidate != voice);
    }

    pub fn stop_all(&mut self) {
        if let Ok(mut state) = mixer().lock() {
            let ours = &self.voices;
            state
                .voices
                .retain(|(id, _)| !ours.contains_key(&SfxVoiceId(*id)));
        }
        self.voices.clear();
        self.order.clear();
    }

    /// Forgets voices the mixer has finished, and lets the output close
    /// once everything has been silent for a while. No allocation.
    pub fn update(&mut self) {
        if let Ok(state) = mixer().lock() {
            let live = |id: &SfxVoiceId| state.voices.iter().any(|(voice, _)| *voice == id.value());
            self.voices.retain(|id, _| live(id));
            self.order.retain(|id| live(id));
        }
        maintain_output();
    }

    pub fn set_mix_volume(&mut self, volume: f32) {
        self.settings.mix_volume = finite_clamped(volume, 0.0, 2.0, 1.0);
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.settings.enabled = enabled;
        if !enabled {
            self.stop_all();
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    #[must_use]
    pub fn active_voice_count(&self) -> usize {
        self.voices.len()
    }

    /// Plays dropped so far because their clip was still decoding.
    #[must_use]
    pub fn not_ready_count(&self) -> u64 {
        self.not_ready
    }

    #[must_use]
    pub fn is_playing(&self, voice: SfxVoiceId) -> bool {
        mixer()
            .lock()
            .is_ok_and(|state| state.voices.iter().any(|(id, _)| *id == voice.value()))
    }

    #[must_use]
    pub fn frame_demand(&self) -> Option<Duration> {
        sooner(
            (!self.voices.is_empty()).then_some(Duration::from_millis(100)),
            output_demand(),
        )
    }

    fn make_voice_capacity(&mut self, incoming_priority: u8) -> bool {
        while self.voices.len() >= self.settings.maximum_voices {
            let Some(index) = oldest_evictable_voice(&self.order, incoming_priority, |id| {
                self.voices.get(&id).copied()
            }) else {
                return false;
            };
            let Some(oldest) = self.order.remove(index) else {
                return false;
            };
            self.voices.remove(&oldest);
            if let Ok(mut state) = mixer().lock() {
                state.voices.retain(|(id, _)| *id != oldest.value());
            }
        }
        true
    }

    fn allocate_voice_id(&mut self) -> SfxVoiceId {
        let id = SfxVoiceId(self.next_voice_id);
        self.next_voice_id = self.next_voice_id.wrapping_add(1).max(1) & !VOICE_LINE_ID_BIT;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::super::stream::{chunk_queue, ChunkProducer};
    use super::*;

    /// A track holding `samples`, with its producer either kept (a decoder
    /// still running) or dropped (a decoder that finished).
    fn track(samples: Vec<f32>, decoder_alive: bool) -> (MusicTrack, Option<ChunkProducer>) {
        let (producer, consumer, _) = chunk_queue(4);
        if !samples.is_empty() {
            assert!(producer.offer(samples).is_ok(), "room for one chunk");
        }
        let mut track = MusicTrack::new("test".to_string(), consumer, None, 48_000.0);
        track.current_volume = 1.0;
        (track, decoder_alive.then_some(producer))
    }

    fn mixer_with(tracks: Vec<MusicTrack>) -> MixerState {
        MixerState {
            tracks,
            music_mix_volume: 1.0,
            device_rate: 48_000.0,
            ..MixerState::default()
        }
    }

    fn voice(samples: Vec<f32>, rate: f64, source_rate: f64) -> ActiveVoice {
        ActiveVoice {
            samples: Arc::new(samples),
            position: 0.0,
            rate,
            source_rate,
            left: 1.0,
            right: 1.0,
            looped: false,
        }
    }

    /// A left channel that names its frame (0.0, 0.1, 0.2, ...), scaled so
    /// it survives `fill`'s clamp to +/-1.0.
    fn marked_frames(count: u8) -> Vec<f32> {
        (0..count)
            .flat_map(|frame| [f32::from(frame) * 0.1, 0.0])
            .collect()
    }

    fn left_channel(out: &[f32]) -> Vec<f32> {
        out.chunks(2).map(|frame| frame[0]).collect()
    }

    fn assert_close(got: &[f32], want: &[f32]) {
        assert_eq!(got.len(), want.len(), "got {got:?}, expected {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-5, "got {got:?}, expected {want:?}");
        }
    }

    /// The regression test for the bug that cycled the playlist every two
    /// seconds: an empty queue is a decoder that has not refilled yet, not a
    /// track that has ended.
    #[test]
    fn a_decoder_that_has_not_refilled_yet_is_not_finished() {
        let (track, producer) = track(Vec::new(), true);
        let mut state = mixer_with(vec![track]);
        state.fill(&mut [0.0; 8], 2);
        assert!(!state.tracks[0].finished);
        drop(producer);
    }

    #[test]
    fn a_finished_decoder_with_nothing_queued_is_finished() {
        let (track, _) = track(Vec::new(), false);
        let mut state = mixer_with(vec![track]);
        state.fill(&mut [0.0; 8], 2);
        assert!(state.tracks[0].finished);
    }

    #[test]
    fn music_is_scaled_by_volume_and_the_unconditional_post_gain() {
        let (track, producer) = track(vec![0.5; 8], true);
        let mut state = mixer_with(vec![track]);
        let mut out = vec![0.0; 8];
        state.fill(&mut out, 2);
        // With no bass boost the shelf contributes nothing, leaving the
        // post-gain.
        let expected = 0.5 * MUSIC_BASS_POST_GAIN;
        assert_close(&out, &[expected; 8]);
        drop(producer);
    }

    /// A track decoded at 48 kHz on a 24 kHz device plays every other frame:
    /// the same pitch in half the frames.
    #[test]
    fn music_is_resampled_to_the_device_rate() {
        let (track, producer) = track(marked_frames(12), true);
        let mut state = mixer_with(vec![track]);
        state.device_rate = 24_000.0;
        let mut out = [0.0; 8];
        state.fill(&mut out, 2);
        let heard: Vec<f32> = left_channel(&out)
            .iter()
            .map(|sample| sample / MUSIC_BASS_POST_GAIN)
            .collect();
        // The interpolator starts one frame behind: 0 (before the first
        // frame), then frames 1, 3 and 5 of the 48 kHz track.
        assert_close(&heard, &[0.0, 0.1, 0.3, 0.5]);
        drop(producer);
    }

    #[test]
    fn a_hard_left_pan_silences_the_right_channel() {
        let (left, right) = stereo_volume(1.0, -1.0);
        assert!((left - 1.0).abs() < 1e-6, "left was {left}");
        assert!(right.abs() < 1e-6, "right was {right}");
        let (left, right) = stereo_volume(1.0, 1.0);
        assert!(left.abs() < 1e-6, "left was {left}");
        assert!((right - 1.0).abs() < 1e-6, "right was {right}");
    }

    #[test]
    fn a_one_shot_voice_is_retired_once_it_runs_out() {
        let mut state = mixer_with(Vec::new());
        state
            .voices
            .push((1, voice(vec![1.0, 1.0, 1.0, 1.0], 1.0, 48_000.0)));
        let mut out = vec![0.0; 8];
        state.fill(&mut out, 2);
        assert!(
            state.voices.is_empty(),
            "a finished one-shot must not linger"
        );
        assert!(out[0] > 0.0 && out[1] > 0.0, "its samples were mixed");
    }

    /// Pause must hold the track, not silence it. Muting kept the queue
    /// draining, so a resume landed further into the music and a long enough
    /// pause could advance the playlist -- and sng-roguelite pauses on focus
    /// loss, so this is a path players actually take.
    #[test]
    fn pausing_holds_the_track_rather_than_muting_it() {
        let (track, producer) = track(vec![0.5; 8], true);
        let mut state = mixer_with(vec![track]);
        state.paused = true;

        let mut out = [0.0; 8];
        state.fill(&mut out, 2);
        assert!(
            out.iter().all(|sample| sample.abs() < 1e-6),
            "paused is silent"
        );

        state.paused = false;
        state.fill(&mut out, 2);
        let expected = 0.5 * MUSIC_BASS_POST_GAIN;
        assert_close(&out, &[expected; 8]);
        drop(producer);
    }

    /// Rate is honoured, and by selecting different samples rather than by
    /// merely not crashing: at 2x a voice reads every other frame.
    #[test]
    fn playback_rate_advances_the_voice_faster() {
        let mut state = mixer_with(Vec::new());
        state
            .voices
            .push((1, voice(marked_frames(8), 2.0, 48_000.0)));
        let mut out = [0.0; 8];
        state.fill(&mut out, 2);
        assert_close(&left_channel(&out), &[0.0, 0.2, 0.4, 0.6]);
    }

    /// A clip recorded at 48 kHz keeps its pitch on a 24 kHz device: the
    /// mixer steps through it twice as fast.
    #[test]
    fn a_clip_keeps_its_pitch_at_any_device_rate() {
        let mut state = mixer_with(Vec::new());
        state.device_rate = 24_000.0;
        state
            .voices
            .push((1, voice(marked_frames(8), 1.0, 48_000.0)));
        let mut out = [0.0; 8];
        state.fill(&mut out, 2);
        assert_close(&left_channel(&out), &[0.0, 0.2, 0.4, 0.6]);
    }

    #[test]
    fn the_mixer_is_quiet_only_with_nothing_left_to_play() {
        let mut state = mixer_with(Vec::new());
        assert!(state.is_quiet(), "nothing at all");

        let (playing, producer) = track(vec![0.1; 8], true);
        state.tracks.push(playing);
        assert!(!state.is_quiet(), "a track still playing");
        state.paused = true;
        assert!(!state.is_quiet(), "a paused track resumes later");
        state.paused = false;
        state.tracks[0].finished = true;
        assert!(state.is_quiet(), "a finished track");
        state.tracks[0].finished = false;
        state.tracks[0].target_volume = 0.0;
        state.tracks[0].current_volume = 0.0;
        assert!(state.is_quiet(), "a track faded out");
        drop(producer);

        state.voices.push((1, voice(vec![0.5; 4], 1.0, 48_000.0)));
        assert!(!state.is_quiet(), "an effect playing");
    }

    #[test]
    fn a_clip_decodes_at_its_own_rate() {
        let bytes: &'static [u8] = include_bytes!("../tests/assets/short-a.ogg");
        let clip = decode_clip(&TrackSource::Embedded(bytes)).expect("decode");
        assert!((clip.rate - 44_100.0).abs() < f64::EPSILON);
        #[allow(clippy::cast_precision_loss)]
        let seconds = clip.samples.len() as f64 / 2.0 / clip.rate;
        assert!((seconds - 3.0).abs() < 0.05, "{seconds:.3} s");
    }

    #[test]
    fn voice_lines_and_effects_never_share_an_id() {
        let mut voice_lines = VoiceController::new(true, 1.0);
        let mut effects = SfxController::new(SfxSettings::default());
        for _ in 0..4 {
            assert_ne!(voice_lines.allocate() & VOICE_LINE_ID_BIT, 0);
            assert_eq!(effects.allocate_voice_id().value() & VOICE_LINE_ID_BIT, 0);
        }
    }
}
