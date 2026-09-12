//! Desktop audio playback on loadngo's own output seam, without rodio or cpal.
//!
//! Selected by the `native-desktop-audio` feature. Off by default: rodio still
//! works, and these are the platforms where audio currently works *best*, so
//! the replacement earns its place by measurement before it becomes the
//! default. Both paths satisfy the same `imp` contract, so they can be
//! compared on one machine with the Goertzel harness in
//! `audio-io/tests/hardware_smoke.rs`.
//!
//! The shape follows the iOS backend -- music streamed from a worker thread,
//! effects decoded once and kept -- because the asset sizes force it: the
//! games' music runs 5-10 minutes (~212 MB decoded to `f32`) while their
//! effects are twenty clips of at most 17 KB.
//!
//! Desktop needs three things iOS does not, all of which the games rely on:
//! several tracks decoding at once so one can fade out under another, a
//! cue/resume state machine so a one-off cue returns to the playlist, and
//! playback from `&'static [u8]` for callers that embed their audio.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lewton::inside_ogg::OggStreamReader;
use loadngo_audio_io::{open_output_stream, OutputStream};

use super::{finite_clamped, oldest_evictable_voice, SfxPlayRequest, SfxSettings, SfxVoiceId};

/// Matches the rodio backend's constants so the two sound alike. The shelf is
/// applied to the summed music bus rather than per source: rodio mixed a
/// low-passed clone into every track, and its own comment records that doing
/// so cost per-sample lock contention on the audio thread and was a plausible
/// cause of periodic static. One filter over the mix is cheaper and audibly
/// equivalent at these settings.
const MUSIC_BASS_CUTOFF_HZ: f32 = 180.0;
const MUSIC_BASS_POST_GAIN: f32 = 0.9;
/// Frames per chunk from a decoder thread. ~85 ms at 48 kHz, and eight queued
/// chunks give a decoder two thirds of a second of slack against a stall.
const MUSIC_CHUNK_FRAMES: usize = 4_096;
const MUSIC_CHUNK_QUEUE: usize = 8;
/// A track that reports "finished" the instant it starts must not spin the
/// playlist. Mirrors the Android backend's identical guard.
const PLAYLIST_ADVANCE_DEBOUNCE: Duration = Duration::from_secs(2);
/// How often the host is asked to wake while music simply plays. Matches the
/// rodio backend's constant of the same name: `frame_demand` paces the host,
/// so a smaller value here would wake it needlessly often for the entire
/// length of every track.
const MUSIC_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicCueMode {
    OneShot,
    Loop,
}

// ---------------------------------------------------------------- decoding

/// Interleaved stereo `f32` at the output device's rate.
type Chunk = Vec<f32>;

trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// Where a track's bytes come from. Desktop supports both, unlike the mobile
/// backends: those have a packaging story that always yields a real path.
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
}

/// Decodes to interleaved stereo at `output_rate`, sending chunks until the
/// track ends or the receiver goes away.
fn stream_track(
    source: &TrackSource,
    output_rate: f64,
    tx: &SyncSender<Chunk>,
) -> Result<(), String> {
    let mut reader = OggStreamReader::new(source.open()?)
        .map_err(|err| format!("Failed to read audio: {err}"))?;
    let source_rate = f64::from(reader.ident_hdr.audio_sample_rate);
    let channels = usize::from(reader.ident_hdr.audio_channels).max(1);
    let ratio = source_rate / output_rate;

    let mut pending: Vec<f32> = Vec::new();
    let mut position = 0.0f64;
    let mut chunk: Chunk = Vec::with_capacity(MUSIC_CHUNK_FRAMES * 2);

    loop {
        let packet = reader
            .read_dec_packet_itl()
            .map_err(|err| format!("Failed to decode audio: {err}"))?;
        let Some(packet) = packet else { break };
        for frame in packet.chunks(channels) {
            let left = f32::from(frame[0]) / 32_768.0;
            let right = if channels > 1 {
                f32::from(frame[1]) / 32_768.0
            } else {
                left
            };
            pending.push(left);
            pending.push(right);
        }

        let available = pending.len() / 2;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        while (position.floor() as usize) + 1 < available {
            let index = position.floor() as usize;
            let fraction = (position - position.floor()) as f32;
            for channel in 0..2 {
                let a = pending[index * 2 + channel];
                let b = pending[(index + 1) * 2 + channel];
                chunk.push(a + (b - a) * fraction);
            }
            position += ratio;
            if chunk.len() >= MUSIC_CHUNK_FRAMES * 2 {
                if send_chunk(tx, std::mem::take(&mut chunk)).is_err() {
                    return Ok(());
                }
                chunk = Vec::with_capacity(MUSIC_CHUNK_FRAMES * 2);
            }
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let consumed = position.floor() as usize;
        if consumed > 0 {
            pending.drain(..consumed * 2);
            #[allow(clippy::cast_precision_loss)]
            let consumed_f = consumed as f64;
            position -= consumed_f;
        }
    }

    if !chunk.is_empty() {
        let _ = send_chunk(tx, chunk);
    }
    Ok(())
}

fn send_chunk(tx: &SyncSender<Chunk>, chunk: Chunk) -> Result<(), ()> {
    let mut chunk = chunk;
    loop {
        match tx.try_send(chunk) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                chunk = returned;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TrySendError::Disconnected(_)) => return Err(()),
        }
    }
}

#[derive(Clone)]
struct CachedClip {
    samples: Arc<Vec<f32>>,
}

fn decode_clip(source: &TrackSource, output_rate: f64) -> Result<CachedClip, String> {
    let mut reader = OggStreamReader::new(source.open()?)
        .map_err(|err| format!("Failed to read clip: {err}"))?;
    let source_rate = f64::from(reader.ident_hdr.audio_sample_rate);
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
    let samples = if (source_rate - output_rate).abs() < f64::EPSILON {
        interleaved
    } else {
        resample(&interleaved, source_rate / output_rate)
    };
    Ok(CachedClip {
        samples: Arc::new(samples),
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn resample(input: &[f32], ratio: f64) -> Vec<f32> {
    let frames = input.len() / 2;
    if frames < 2 {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(input.len());
    let mut position = 0.0f64;
    while (position.floor() as usize) + 1 < frames {
        let index = position.floor() as usize;
        let fraction = (position - position.floor()) as f32;
        for channel in 0..2 {
            let a = input[index * 2 + channel];
            let b = input[(index + 1) * 2 + channel];
            out.push(a + (b - a) * fraction);
        }
        position += ratio;
    }
    out
}

// ------------------------------------------------------------------ mixing

/// One decoding track. Several exist at once so a cue can fade in while the
/// previous track fades out -- the capability the mobile backends lack.
struct MusicTrack {
    key: String,
    chunks: Receiver<Chunk>,
    pending: VecDeque<f32>,
    /// Ramped on the game thread by `update`, read by the audio thread.
    current_volume: f32,
    target_volume: f32,
    playing: bool,
    finished: bool,
}

struct ActiveVoice {
    samples: Arc<Vec<f32>>,
    cursor: usize,
    left: f32,
    right: f32,
    looped: bool,
}

#[derive(Default)]
struct MixerState {
    tracks: Vec<MusicTrack>,
    music_mix_volume: f32,
    bass_boost: f32,
    bass_state: [f32; 2],
    bass_coefficient: f32,
    voices: Vec<(u64, ActiveVoice)>,
}

impl MixerState {
    fn fill(&mut self, out: &mut [f32], channels: usize) {
        out.fill(0.0);
        self.mix_music(out, channels);
        self.mix_voices(out, channels);
        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }

    fn mix_music(&mut self, out: &mut [f32], channels: usize) {
        let mix = self.music_mix_volume;
        let boost = self.bass_boost;
        let coefficient = self.bass_coefficient;
        for track in &mut self.tracks {
            // Why the fill loop stopped is the whole question: a decoder that
            // has not refilled yet looks identical to one that has ended
            // unless `Empty` and `Disconnected` are told apart. Conflating
            // them made every track report "finished" the moment its queue ran
            // dry, which cycled the playlist every couple of seconds.
            //
            // Recording it here also avoids a second `try_recv` after the
            // loop: that probe discarded the chunk whenever it happened to
            // succeed, losing ~85 ms of audio each time it raced a refill.
            let mut disconnected = false;
            while track.pending.len() < out.len() {
                match track.chunks.try_recv() {
                    Ok(chunk) => track.pending.extend(chunk),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if track.pending.is_empty() {
                // The decoder signals the end by dropping its sender, so an
                // empty queue *and* a closed channel is a finished track --
                // not a decoder that merely fell behind.
                track.finished = disconnected;
                continue;
            }
            if !track.playing || track.current_volume <= 0.0 {
                continue;
            }
            let volume = track.current_volume * mix;
            for sample in out.iter_mut() {
                let Some(value) = track.pending.pop_front() else {
                    break;
                };
                *sample += value * volume;
            }
        }

        // One shelf over the summed bus, then the post-gain rodio applies
        // unconditionally to music.
        for (index, sample) in out.iter_mut().enumerate() {
            let channel = index % channels.max(1) % 2;
            let low = self.bass_state[channel] + coefficient * (*sample - self.bass_state[channel]);
            self.bass_state[channel] = low;
            *sample = (*sample + low * boost) * MUSIC_BASS_POST_GAIN;
        }
    }

    fn mix_voices(&mut self, out: &mut [f32], channels: usize) {
        let stride = channels.max(1);
        self.voices.retain_mut(|(_, voice)| {
            let mut index = 0;
            while index + 1 < out.len() {
                if voice.cursor + 1 >= voice.samples.len() {
                    if !voice.looped {
                        return false;
                    }
                    voice.cursor = 0;
                }
                out[index] += voice.samples[voice.cursor] * voice.left;
                out[index + 1] += voice.samples[voice.cursor + 1] * voice.right;
                voice.cursor += 2;
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

/// The live stream, kept only so that dropping it stops the device.
///
/// It sits behind a `Mutex` rather than in a plain `static` because the
/// CoreAudio backend's handle is `Send` but deliberately not `Sync`, and a
/// `static` demands `Sync`. `Mutex<T>` supplies that for any `T: Send`, so
/// this module never has to assert anything about the backend's internals --
/// which it is in no position to know.
static STREAM: Mutex<Option<OutputStream>> = Mutex::new(None);
static MIXER: OnceLock<Arc<Mutex<MixerState>>> = OnceLock::new();
/// The rate the device opened at, or `None` if it could not be opened. Held
/// apart from `STREAM` so the hot paths read a `Copy` value instead of taking
/// a lock to ask a question that never changes.
static OUTPUT: OnceLock<Option<u32>> = OnceLock::new();
static BACKEND_FAILURE: OnceLock<String> = OnceLock::new();

fn mixer() -> &'static Arc<Mutex<MixerState>> {
    MIXER.get_or_init(|| {
        Arc::new(Mutex::new(MixerState {
            music_mix_volume: 1.0,
            ..MixerState::default()
        }))
    })
}

/// Opens the device once. A failure is remembered rather than retried, and
/// `LOADNGO_DISABLE_AUDIO` short-circuits it entirely -- both behaviours the
/// rodio backend had, and both load-bearing: the env switch is how a headless
/// or shared machine runs these apps at all.
fn output() -> Option<u32> {
    *OUTPUT.get_or_init(|| {
        if matches!(
            std::env::var("LOADNGO_DISABLE_AUDIO").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) {
            let _ = BACKEND_FAILURE.set("Audio disabled by LOADNGO_DISABLE_AUDIO".to_string());
            return None;
        }
        let shared = Arc::clone(mixer());
        match open_output_stream(None, None, move |out, channels| {
            match shared.try_lock() {
                Ok(mut state) => state.fill(out, channels),
                // Never block the audio thread: one silent buffer costs far
                // less than a stall, and the only other holders are short
                // game-thread calls.
                Err(_) => out.fill(0.0),
            }
        }) {
            Ok(stream) => {
                let sample_rate_hz = stream.sample_rate_hz();
                if let Ok(mut state) = mixer().lock() {
                    state.bass_coefficient = one_pole_coefficient(sample_rate_hz);
                }
                if let Ok(mut held) = STREAM.lock() {
                    *held = Some(stream);
                }
                Some(sample_rate_hz)
            }
            Err(error) => {
                let detail = format!("Audio output unavailable: {error}");
                eprintln!("{detail}");
                let _ = BACKEND_FAILURE.set(detail);
                None
            }
        }
    })
}

/// One-pole coefficient for [`MUSIC_BASS_CUTOFF_HZ`] at `sample_rate_hz`.
fn one_pole_coefficient(sample_rate_hz: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let rate = sample_rate_hz.max(1) as f32;
    1.0 - (-std::f32::consts::TAU * MUSIC_BASS_CUTOFF_HZ / rate).exp()
}

fn output_rate() -> f64 {
    output().map_or(48_000.0, f64::from)
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
        if output().is_none() {
            return Err(BACKEND_FAILURE
                .get()
                .cloned()
                .unwrap_or_else(|| "Audio output unavailable".to_string()));
        }
        self.fade_duration = fade.max(0.05);

        let source = self
            .sources
            .get(&selected)
            .cloned()
            .unwrap_or_else(|| TrackSource::Path(selected.clone()));
        let rate = output_rate();
        let (tx, rx) = sync_channel::<Chunk>(MUSIC_CHUNK_QUEUE);
        let thread_source = source.clone();
        let thread_key = selected.clone();
        std::thread::Builder::new()
            .name("loadngo-desktop-music".to_string())
            .spawn(move || loop {
                match stream_track(&thread_source, rate, &tx) {
                    Ok(()) => {
                        if !looped {
                            break;
                        }
                    }
                    Err(error) => {
                        // Nothing awaits this thread, and `play_track_path`
                        // returned `Ok` before the file was even opened, so
                        // an unreported failure here is indistinguishable
                        // from silence.
                        eprintln!(
                            "[loadngo/desktop] music decode failed for {thread_key}: {error}"
                        );
                        break;
                    }
                }
            })
            .map_err(|err| format!("Failed to start music decoder: {err}"))?;

        if let Ok(mut state) = mixer().lock() {
            // Everything already playing fades out; the new track fades in.
            for track in &mut state.tracks {
                track.target_volume = 0.0;
            }
            state.tracks.retain(|track| track.current_volume > 0.001);
            state.tracks.push(MusicTrack {
                key: selected.clone(),
                chunks: rx,
                pending: VecDeque::new(),
                current_volume: 0.0,
                target_volume: 1.0,
                playing: true,
                finished: false,
            });
            state.music_mix_volume = if self.paused { 0.0 } else { self.mix_volume };
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

    pub fn update(&mut self, dt: f32) {
        if self.paused {
            return;
        }
        let fade = self.fade_duration.max(0.001);
        let finished = {
            let Ok(mut state) = mixer().lock() else {
                return;
            };
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
            // Retire tracks that faded out, and any that ran dry.
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
            state.music_mix_volume = 0.0;
        }
    }

    pub fn resume(&mut self) {
        self.paused = false;
        if let Ok(mut state) = mixer().lock() {
            state.music_mix_volume = self.mix_volume;
        }
    }

    pub fn set_mix_volume(&mut self, volume: f32) {
        self.mix_volume = finite_clamped(volume, 0.0, 2.0, 1.0);
        if self.paused {
            return;
        }
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

    #[must_use]
    pub fn frame_demand(&self) -> Option<Duration> {
        let fading = mixer().lock().is_ok_and(|state| {
            state
                .tracks
                .iter()
                .any(|track| (track.current_volume - track.target_volume).abs() > 0.001)
        });
        if fading {
            return Some(Duration::from_millis(16));
        }
        if self.active_track.is_some()
            && (self.playlist_mode_active || self.resume_playlist_after_cue)
        {
            return Some(MUSIC_IDLE_POLL_INTERVAL);
        }
        None
    }
}

pub struct VoiceController {
    enabled: bool,
    volume: f32,
    voice: Option<u64>,
    next_id: u64,
}

impl VoiceController {
    pub fn new(enabled: bool, volume: f32) -> Self {
        Self {
            enabled,
            volume: finite_clamped(volume, 0.0, 2.0, 1.0),
            voice: None,
            next_id: 1,
        }
    }

    pub fn play_path(&mut self, path: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if output().is_none() {
            return Ok(());
        }
        self.stop();
        let clip = decode_clip(&TrackSource::Path(path.to_string()), output_rate())
            .map_err(|error| format!("{path}: {error}"))?;
        let id = self.allocate();
        if let Ok(mut state) = mixer().lock() {
            state.voices.push((
                id,
                ActiveVoice {
                    samples: clip.samples,
                    cursor: 0,
                    left: self.volume,
                    right: self.volume,
                    looped: false,
                },
            ));
        }
        self.voice = Some(id);
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

    #[must_use]
    pub fn is_playing(&self) -> bool {
        let Some(id) = self.voice else { return false };
        mixer()
            .lock()
            .is_ok_and(|state| state.voices.iter().any(|(voice, _)| *voice == id))
    }

    pub fn stop(&mut self) {
        let Some(id) = self.voice.take() else { return };
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
        self.is_playing().then_some(Duration::from_millis(100))
    }

    fn allocate(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }
}

pub struct SfxController {
    settings: SfxSettings,
    clips: HashMap<String, CachedClip>,
    embedded: HashMap<String, &'static [u8]>,
    voices: HashMap<SfxVoiceId, u8>,
    order: VecDeque<SfxVoiceId>,
    next_voice_id: u64,
}

impl SfxController {
    pub fn new(settings: SfxSettings) -> Self {
        Self {
            settings: settings.normalized(),
            clips: HashMap::new(),
            embedded: HashMap::new(),
            voices: HashMap::new(),
            order: VecDeque::new(),
            next_voice_id: 1,
        }
    }

    /// Decodes and caches under `key`, so a later `play` with `path: key`
    /// hits the cache and never touches the filesystem -- matching the rodio
    /// backend, whose callers rely on exactly that.
    pub fn preload_embedded(&mut self, key: &str, ogg_bytes: &'static [u8]) -> Result<(), String> {
        if self.clips.contains_key(key) {
            return Ok(());
        }
        self.embedded.insert(key.to_string(), ogg_bytes);
        if output().is_none() {
            // Decode lazily once a device exists rather than failing here:
            // the rodio path also tolerated a missing backend.
            return Ok(());
        }
        let clip = decode_clip(&TrackSource::Embedded(ogg_bytes), output_rate())
            .map_err(|error| format!("Failed to decode embedded SFX {key}: {error}"))?;
        self.clips.insert(key.to_string(), clip);
        Ok(())
    }

    pub fn play(&mut self, request: SfxPlayRequest<'_>) -> Result<Option<SfxVoiceId>, String> {
        let request = request.normalized();
        if request.path.is_empty() {
            return Err("SFX path must not be empty".to_string());
        }
        if !self.settings.enabled || output().is_none() {
            return Ok(None);
        }
        self.update();
        if !self.make_voice_capacity(request.priority) {
            return Ok(None);
        }
        let clip = self.load_clip(request.path)?;
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
                    cursor: 0,
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
        let ids: Vec<u64> = self.voices.keys().map(|id| id.value()).collect();
        if let Ok(mut state) = mixer().lock() {
            state.voices.retain(|(id, _)| !ids.contains(id));
        }
        self.voices.clear();
        self.order.clear();
    }

    pub fn update(&mut self) {
        let Ok(state) = mixer().lock() else { return };
        let live: Vec<u64> = state.voices.iter().map(|(id, _)| *id).collect();
        drop(state);
        self.voices.retain(|id, _| live.contains(&id.value()));
        self.order.retain(|id| live.contains(&id.value()));
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

    #[must_use]
    pub fn is_playing(&self, voice: SfxVoiceId) -> bool {
        mixer()
            .lock()
            .is_ok_and(|state| state.voices.iter().any(|(id, _)| *id == voice.value()))
    }

    #[must_use]
    pub fn frame_demand(&self) -> Option<Duration> {
        (!self.voices.is_empty()).then_some(Duration::from_millis(100))
    }

    fn load_clip(&mut self, path: &str) -> Result<CachedClip, String> {
        if let Some(clip) = self.clips.get(path) {
            return Ok(clip.clone());
        }
        let source = self.embedded.get(path).map_or_else(
            || TrackSource::Path(path.to_string()),
            |bytes| TrackSource::Embedded(bytes),
        );
        let clip =
            decode_clip(&source, output_rate()).map_err(|error| format!("{path}: {error}"))?;
        self.clips.insert(path.to_string(), clip.clone());
        Ok(clip)
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
        self.next_voice_id = self.next_voice_id.wrapping_add(1).max(1);
        id
    }
}
