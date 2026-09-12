//! iOS audio playback: loadngo's own backend, no `rodio` and no `cpal`.
//!
//! One RemoteIO `AudioUnit` runs a render callback that mixes a streaming
//! music track with short, resident sound effects.
//!
//! Music streams and effects don't, because the assets differ by three
//! orders of magnitude: the games' music runs 5-10 minutes (`flutterrung.ogg`
//! is 603 s, about 212 MB once decoded to `f32`), which no phone should hold
//! in memory, while their effects are 20 clips of at most 17 KB. So music is
//! decoded incrementally on a worker thread and effects are decoded once and
//! kept.
//!
//! Everything arrives as a real file path. `ios.rs` sets `SNG_ASSETS_ROOT` to
//! the bundle's `assets/`, and both games use it, so -- exactly as on Android
//! -- `preload_embedded` has nothing to do here and says so.
//!
//! The unit runs at 48 kHz stereo. Every effect and one of the three music
//! tracks is already 48 kHz; the two 44.1 kHz tracks are converted on the
//! decoder thread, so the render callback never resamples.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::fs::File;
use std::io::BufReader;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use coreaudio_sys::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
    kAudioUnitManufacturer_Apple, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, kAudioUnitSubType_RemoteIO,
    kAudioUnitType_Output, AURenderCallbackStruct, AudioBufferList, AudioComponentDescription,
    AudioComponentFindNext, AudioComponentInstanceNew, AudioOutputUnitStart, AudioOutputUnitStop,
    AudioStreamBasicDescription, AudioTimeStamp, AudioUnit, AudioUnitInitialize,
    AudioUnitRenderActionFlags, AudioUnitSetProperty, OSStatus,
};
use lewton::inside_ogg::OggStreamReader;

use super::{SfxPlayRequest, SfxSettings, SfxVoiceId};

/// The rate the unit runs at. Chosen because 21 of the 23 shipped assets are
/// already 48 kHz, so only the two 44.1 kHz music tracks need converting.
const OUTPUT_SAMPLE_RATE: f64 = 48_000.0;
const OUTPUT_CHANNELS: u32 = 2;
/// Frames per chunk handed from the decoder thread to the mixer. At 48 kHz
/// this is ~85 ms, so eight queued chunks give the decoder about two thirds
/// of a second of slack against a scheduling hiccup.
const MUSIC_CHUNK_FRAMES: usize = 4_096;
const MUSIC_CHUNK_QUEUE: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicCueMode {
    OneShot,
    Loop,
}

// ---------------------------------------------------------------- decoding

/// Interleaved stereo `f32` at [`OUTPUT_SAMPLE_RATE`].
type Chunk = Vec<f32>;

/// Decodes `path`, converting to stereo and to the output rate, and sends it
/// on in chunks. Returns when the track ends or the receiver goes away.
fn stream_track(path: &str, tx: &SyncSender<Chunk>) -> Result<(), String> {
    let file = File::open(path).map_err(|err| format!("Missing music {path}: {err}"))?;
    let mut reader = OggStreamReader::new(BufReader::new(file))
        .map_err(|err| format!("Failed to read music {path}: {err}"))?;
    let source_rate = f64::from(reader.ident_hdr.audio_sample_rate);
    let source_channels = usize::from(reader.ident_hdr.audio_channels).max(1);
    let ratio = source_rate / OUTPUT_SAMPLE_RATE;

    // Linear interpolation is enough here: this only ever runs at 44100/48000
    // (a ratio of 0.919), on music, off the audio thread. It is deliberately
    // not `audio-io`'s `DriftResampler`, which exists to track two live clocks
    // against each other and would be the wrong tool for a fixed ratio.
    let mut pending: Vec<f32> = Vec::new();
    let mut position = 0.0f64;
    let mut chunk: Chunk = Vec::with_capacity(MUSIC_CHUNK_FRAMES * 2);

    loop {
        let packet = reader
            .read_dec_packet_itl()
            .map_err(|err| format!("Failed to decode music {path}: {err}"))?;
        let Some(packet) = packet else {
            break;
        };
        // Interleaved i16 -> interleaved stereo f32, mono duplicated.
        for frame in packet.chunks(source_channels) {
            let left = f32::from(frame[0]) / 32_768.0;
            let right = if source_channels > 1 {
                f32::from(frame[1]) / 32_768.0
            } else {
                left
            };
            pending.push(left);
            pending.push(right);
        }

        let frames_available = pending.len() / 2;
        while (position.floor() as usize) + 1 < frames_available {
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

        // Keep only the tail the interpolator still needs.
        let consumed = position.floor() as usize;
        if consumed > 0 {
            pending.drain(..consumed * 2);
            position -= consumed as f64;
        }
    }

    if !chunk.is_empty() {
        let _ = send_chunk(tx, chunk);
    }
    Ok(())
}

/// Blocking-ish send that gives up if the mixer stopped listening. The queue
/// is bounded so a decoder can never outrun playback into unbounded memory.
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

/// A whole effect, decoded once and kept. Interleaved stereo at the output
/// rate, so the render callback only has to add and scale.
#[derive(Clone)]
struct CachedClip {
    samples: Arc<Vec<f32>>,
}

fn decode_clip(path: &str) -> Result<CachedClip, String> {
    let file = File::open(path).map_err(|err| format!("Missing SFX {path}: {err}"))?;
    let mut reader = OggStreamReader::new(BufReader::new(file))
        .map_err(|err| format!("Failed to read SFX {path}: {err}"))?;
    let source_rate = f64::from(reader.ident_hdr.audio_sample_rate);
    let source_channels = usize::from(reader.ident_hdr.audio_channels).max(1);
    let mut interleaved: Vec<f32> = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .map_err(|err| format!("Failed to decode SFX {path}: {err}"))?
    {
        for frame in packet.chunks(source_channels) {
            let left = f32::from(frame[0]) / 32_768.0;
            let right = if source_channels > 1 {
                f32::from(frame[1]) / 32_768.0
            } else {
                left
            };
            interleaved.push(left);
            interleaved.push(right);
        }
    }
    if interleaved.is_empty() {
        return Err(format!("SFX {path} contains no audio samples"));
    }
    // Effects ship at 48 kHz already; resample only if that ever changes.
    let samples = if (source_rate - OUTPUT_SAMPLE_RATE).abs() < f64::EPSILON {
        interleaved
    } else {
        resample_interleaved(&interleaved, source_rate / OUTPUT_SAMPLE_RATE)
    };
    Ok(CachedClip {
        samples: Arc::new(samples),
    })
}

fn resample_interleaved(input: &[f32], ratio: f64) -> Vec<f32> {
    let frames = input.len() / 2;
    if frames < 2 {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(((frames as f64 / ratio) as usize + 1) * 2);
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

struct ActiveVoice {
    samples: Arc<Vec<f32>>,
    cursor: usize,
    left: f32,
    right: f32,
    looped: bool,
}

struct MusicPlayback {
    chunks: Receiver<Chunk>,
    pending: VecDeque<f32>,
    finished: bool,
}

#[derive(Default)]
struct MixerState {
    music: Option<MusicPlayback>,
    music_volume: f32,
    /// One-pole state for the bass shelf, per channel.
    bass_state: [f32; 2],
    bass_boost: f32,
    voices: Vec<(u64, ActiveVoice)>,
}

impl MixerState {
    fn fill(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        self.mix_music(out);
        self.mix_voices(out);
        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }

    fn mix_music(&mut self, out: &mut [f32]) {
        let Some(music) = self.music.as_mut() else {
            return;
        };
        while music.pending.len() < out.len() {
            match music.chunks.try_recv() {
                Ok(chunk) => music.pending.extend(chunk),
                Err(_) => break,
            }
        }
        if music.pending.is_empty() {
            // The decoder thread ends by dropping its sender, so an empty
            // queue plus a dead channel is a finished track, not a stall.
            music.finished = music.chunks.try_recv().is_err() && music.pending.is_empty();
            return;
        }
        // First-order low shelf: add a lowpassed copy back in. Android gets
        // this from the platform's own bass-boost effect; there is no
        // equivalent to borrow here, so it is done by hand -- but it should
        // still *sound* like the other backends.
        //
        // Both values mirror the desktop backend deliberately. Its
        // `MUSIC_BASS_CUTOFF_HZ` is 180, and the one-pole coefficient for
        // that at 48 kHz is `1 - exp(-2*PI*180/48000)`; the 0.15 used here
        // first was about six times too wide, lifting everything up past a
        // kilohertz instead of bass. Its `MUSIC_BASS_POST_GAIN` of 0.9 is
        // applied to the music bus whether or not any boost is mixed in
        // (both branches of `append_music_source` end in `.amplify`), so it
        // is unconditional here too -- without it this backend ran the music
        // bus about a decibel hot relative to effects.
        const BASS_ONE_POLE: f32 = 0.0234;
        const MUSIC_POST_GAIN: f32 = 0.9;

        let volume = self.music_volume;
        let boost = self.bass_boost;
        for (index, sample) in out.iter_mut().enumerate() {
            let Some(value) = music.pending.pop_front() else {
                break;
            };
            let channel = index % 2;
            let low = self.bass_state[channel] + BASS_ONE_POLE * (value - self.bass_state[channel]);
            self.bass_state[channel] = low;
            *sample += (value + low * boost) * volume * MUSIC_POST_GAIN;
        }
    }

    fn mix_voices(&mut self, out: &mut [f32]) {
        self.voices.retain_mut(|(_, voice)| {
            let mut index = 0;
            while index < out.len() {
                if voice.cursor + 1 >= voice.samples.len() {
                    if !voice.looped {
                        return false;
                    }
                    voice.cursor = 0;
                }
                out[index] += voice.samples[voice.cursor] * voice.left;
                out[index + 1] += voice.samples[voice.cursor + 1] * voice.right;
                voice.cursor += 2;
                index += 2;
            }
            true
        });
    }
}

/// Pan law shared with the Android backend: a linear left/right split.
fn stereo_volume(volume: f32, pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    (volume * (1.0 - pan.max(0.0)), volume * (1.0 + pan.min(0.0)))
}

// ------------------------------------------------------------------ output

struct Output {
    _unit: AudioUnit,
}

// SAFETY: the unit is created once, started once, and never touched again
// from Rust; the render callback reaches the mixer through an `Arc`, not
// through this handle.
unsafe impl Send for Output {}
unsafe impl Sync for Output {}

static MIXER: OnceLock<Arc<Mutex<MixerState>>> = OnceLock::new();
static OUTPUT: OnceLock<Option<Output>> = OnceLock::new();

fn mixer() -> &'static Arc<Mutex<MixerState>> {
    MIXER.get_or_init(|| {
        Arc::new(Mutex::new(MixerState {
            music_volume: 1.0,
            ..MixerState::default()
        }))
    })
}

/// Starts the audio unit the first time anything asks for audio. A failure
/// is remembered, not retried: if RemoteIO won't start, it won't start later
/// either, and every controller degrades to silence rather than erroring.
fn ensure_output() -> bool {
    OUTPUT
        .get_or_init(|| {
            configure_audio_session();
            // SAFETY: every pointer below is a live local, and the callback
            // outlives the unit because the mixer is a `'static` `OnceLock`.
            unsafe { start_unit() }.ok()
        })
        .is_some()
}

unsafe fn start_unit() -> Result<Output, ()> {
    let description = AudioComponentDescription {
        componentType: kAudioUnitType_Output,
        componentSubType: kAudioUnitSubType_RemoteIO,
        componentManufacturer: kAudioUnitManufacturer_Apple,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    let component = AudioComponentFindNext(std::ptr::null_mut(), &description);
    if component.is_null() {
        return Err(());
    }
    let mut unit: AudioUnit = std::ptr::null_mut();
    if AudioComponentInstanceNew(component, &mut unit) != 0 || unit.is_null() {
        return Err(());
    }

    let bytes_per_frame = 4 * OUTPUT_CHANNELS;
    let format = AudioStreamBasicDescription {
        mSampleRate: OUTPUT_SAMPLE_RATE,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: OUTPUT_CHANNELS,
        mBitsPerChannel: 32,
        mReserved: 0,
    };
    if AudioUnitSetProperty(
        unit,
        kAudioUnitProperty_StreamFormat,
        kAudioUnitScope_Input,
        0,
        (&raw const format).cast(),
        u32::try_from(std::mem::size_of::<AudioStreamBasicDescription>()).unwrap_or(0),
    ) != 0
    {
        return Err(());
    }

    let callback = AURenderCallbackStruct {
        inputProc: Some(render),
        inputProcRefCon: std::ptr::null_mut(),
    };
    if AudioUnitSetProperty(
        unit,
        kAudioUnitProperty_SetRenderCallback,
        kAudioUnitScope_Input,
        0,
        (&raw const callback).cast(),
        u32::try_from(std::mem::size_of::<AURenderCallbackStruct>()).unwrap_or(0),
    ) != 0
    {
        return Err(());
    }

    if AudioUnitInitialize(unit) != 0 || AudioOutputUnitStart(unit) != 0 {
        let _ = AudioOutputUnitStop(unit);
        return Err(());
    }
    Ok(Output { _unit: unit })
}

/// iOS plays nothing until the process has an active audio session in a
/// category that permits playback, so this is not optional setup.
fn configure_audio_session() {
    #[link(name = "AVFAudio", kind = "framework")]
    extern "C" {}

    // SAFETY: standard singleton messaging; both selectors exist on
    // AVAudioSession and return BOOL. Playback continues either way -- a
    // refused session leaves the app silent rather than broken -- but the
    // refusal is reported, because a silent session and a working one look
    // identical from everywhere else in this module.
    unsafe {
        use objc2::runtime::AnyObject;
        let class = objc2::class!(AVAudioSession);
        let session: *mut AnyObject = objc2::msg_send![class, sharedInstance];
        if session.is_null() {
            eprintln!("[loadngo/ios] AVAudioSession unavailable; audio will be silent");
            return;
        }
        let category = objc2_foundation::NSString::from_str("AVAudioSessionCategoryPlayback");
        let categorised: bool = objc2::msg_send![
            session,
            setCategory: &*category,
            error: std::ptr::null_mut::<*mut AnyObject>(),
        ];
        if !categorised {
            eprintln!("[loadngo/ios] AVAudioSession refused the playback category");
        }
        let activated: bool = objc2::msg_send![
            session,
            setActive: true,
            error: std::ptr::null_mut::<*mut AnyObject>(),
        ];
        if !activated {
            eprintln!("[loadngo/ios] AVAudioSession refused to activate; audio will be silent");
        }
    }
}

/// The render callback. Runs on CoreAudio's real-time thread.
///
/// It takes the mixer lock with `try_lock` and outputs silence if the lock is
/// held. Blocking here would glitch far worse than one silent buffer, and the
/// only other holders are short game-thread calls (`play`, `stop`, `update`).
unsafe extern "C" fn render(
    _ref_con: *mut c_void,
    _flags: *mut AudioUnitRenderActionFlags,
    _timestamp: *const AudioTimeStamp,
    _bus: u32,
    _frames: u32,
    data: *mut AudioBufferList,
) -> OSStatus {
    let Some(list) = data.as_mut() else {
        return 0;
    };
    let buffers = std::slice::from_raw_parts_mut(
        list.mBuffers.as_mut_ptr(),
        list.mNumberBuffers.max(1) as usize,
    );
    for buffer in buffers {
        let samples = buffer.mData.cast::<f32>();
        if samples.is_null() {
            continue;
        }
        let count = buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
        let out = std::slice::from_raw_parts_mut(samples, count);
        match mixer().try_lock() {
            Ok(mut state) => state.fill(out),
            Err(_) => out.fill(0.0),
        }
    }
    0
}

// ------------------------------------------------------------- controllers

pub struct MusicController {
    active_track: Option<String>,
    playlist_tracks: Vec<String>,
    playlist_index: usize,
    boot_track_path: String,
    cue_mode: MusicCueMode,
    mix_volume: f32,
    bass_boost: f32,
    playlist_active: bool,
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
            active_track: None,
            playlist_tracks,
            playlist_index: 0,
            boot_track_path,
            cue_mode,
            mix_volume: 1.0,
            bass_boost: bass_boost.clamp(0.0, 1.0),
            playlist_active: false,
            paused: false,
        }
    }

    /// `fade` is accepted and ignored, exactly as on Android: switching
    /// tracks is immediate. A crossfade would need two decoders and a ramp,
    /// which no mobile backend here has ever had.
    pub fn play_track_path(&mut self, path: &str, _fade: f32, looped: bool) -> Result<(), String> {
        let selected = if path.trim().is_empty() {
            self.boot_track_path.clone()
        } else {
            path.trim().to_string()
        };
        if !ensure_output() {
            return Err("iOS audio output unavailable".to_string());
        }
        let (tx, rx) = sync_channel::<Chunk>(MUSIC_CHUNK_QUEUE);
        let thread_path = selected.clone();
        let looping = looped;
        std::thread::Builder::new()
            .name("loadngo-ios-music".to_string())
            .spawn(move || loop {
                if let Err(error) = stream_track(&thread_path, &tx) {
                    // Nobody awaits this thread, so a failure it keeps to
                    // itself is indistinguishable from silence at the
                    // speaker -- `play_track_path` has already returned `Ok`
                    // by the time the file is even opened.
                    eprintln!("[loadngo/ios] music decode failed: {error}");
                    break;
                }
                if !looping {
                    break;
                }
            })
            .map_err(|err| format!("Failed to start music decoder: {err}"))?;

        if let Ok(mut state) = mixer().lock() {
            state.music = Some(MusicPlayback {
                chunks: rx,
                pending: VecDeque::new(),
                finished: false,
            });
            state.music_volume = self.mix_volume;
            state.bass_boost = self.bass_boost;
        }
        self.active_track = Some(selected);
        self.paused = false;
        Ok(())
    }

    pub fn start_playlist(&mut self, fade: f32) -> Result<(), String> {
        self.playlist_index = 0;
        self.playlist_active = !self.playlist_tracks.is_empty();
        let path = self
            .playlist_tracks
            .first()
            .cloned()
            .unwrap_or_else(|| self.boot_track_path.clone());
        self.play_track_path(&path, fade, false)
    }

    pub fn fade_to_path(&mut self, path: &str, fade: f32) -> Result<(), String> {
        let looped = self.playlist_tracks.is_empty() && self.cue_mode == MusicCueMode::Loop;
        self.playlist_active = false;
        self.play_track_path(path, fade, looped)
    }

    /// No-op: iOS reads music from the app bundle by path (`ios.rs` sets
    /// `SNG_ASSETS_ROOT`), so nothing is ever played from embedded bytes.
    /// Exists so desktop callers need no `cfg` at the call site.
    pub fn preload_embedded(&mut self, _key: &str, _ogg_bytes: &'static [u8]) {}

    /// Advances the playlist when a track runs out. This is the only place
    /// that can notice: the decoder thread signals the end by dropping its
    /// sender, which the mixer sees as a finished stream.
    pub fn update(&mut self, _dt: f32) {
        if self.paused || !self.playlist_active || self.playlist_tracks.is_empty() {
            return;
        }
        let finished = mixer()
            .lock()
            .ok()
            .is_some_and(|state| state.music.as_ref().is_none_or(|music| music.finished));
        if !finished {
            return;
        }
        self.playlist_index = (self.playlist_index + 1) % self.playlist_tracks.len();
        let next = self.playlist_tracks[self.playlist_index].clone();
        let _ = self.play_track_path(&next, 0.0, false);
    }

    pub fn pause(&mut self) {
        self.paused = true;
        if let Ok(mut state) = mixer().lock() {
            state.music_volume = 0.0;
        }
    }

    pub fn resume(&mut self) {
        self.paused = false;
        if let Ok(mut state) = mixer().lock() {
            state.music_volume = self.mix_volume;
        }
    }

    pub fn set_mix_volume(&mut self, volume: f32) {
        self.mix_volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
        if self.paused {
            return;
        }
        if let Ok(mut state) = mixer().lock() {
            state.music_volume = self.mix_volume;
        }
    }

    pub fn set_bass_boost(&mut self, bass_boost: f32) {
        self.bass_boost = bass_boost.clamp(0.0, 1.0);
        if let Ok(mut state) = mixer().lock() {
            state.bass_boost = self.bass_boost;
        }
    }

    pub fn active_track(&self) -> Option<&str> {
        self.active_track.as_deref()
    }

    pub fn frame_demand(&self) -> Option<Duration> {
        (self.active_track.is_some() && self.playlist_active).then_some(Duration::from_millis(100))
    }
}

pub struct VoiceController {
    enabled: bool,
    volume: f32,
}

impl VoiceController {
    pub fn new(enabled: bool, volume: f32) -> Self {
        Self { enabled, volume }
    }

    /// Single-voice playback shares the effect path: these are short spoken
    /// cues, not streams, so there is no reason for a second mechanism.
    pub fn play_path(&mut self, path: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if !ensure_output() {
            return Err("iOS audio output unavailable".to_string());
        }
        let clip = decode_clip(path)?;
        if let Ok(mut state) = mixer().lock() {
            state.voices.push((
                0,
                ActiveVoice {
                    samples: clip.samples,
                    cursor: 0,
                    left: self.volume,
                    right: self.volume,
                    looped: false,
                },
            ));
        }
        Ok(())
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
    }

    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        self.enabled = enabled;
        self.enabled
    }

    pub fn is_playing(&self) -> bool {
        mixer()
            .lock()
            .ok()
            .is_some_and(|state| state.voices.iter().any(|(id, _)| *id == 0))
    }

    pub fn stop(&mut self) {
        if let Ok(mut state) = mixer().lock() {
            state.voices.retain(|(id, _)| *id != 0);
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn frame_demand(&self) -> Option<Duration> {
        self.enabled
            .then(|| self.is_playing().then_some(Duration::from_millis(100)))
            .flatten()
    }
}

pub struct SfxController {
    settings: SfxSettings,
    clips: HashMap<String, CachedClip>,
    voices: HashMap<SfxVoiceId, u8>,
    order: VecDeque<SfxVoiceId>,
    next_voice_id: u64,
}

impl SfxController {
    pub fn new(settings: SfxSettings) -> Self {
        Self {
            settings: settings.normalized(),
            clips: HashMap::new(),
            voices: HashMap::new(),
            order: VecDeque::new(),
            next_voice_id: 1,
        }
    }

    /// No-op, for the same reason as Android's: iOS ships assets in the app
    /// bundle and the games hand over real paths, so nothing is ever played
    /// from embedded bytes here.
    pub fn preload_embedded(
        &mut self,
        _key: &str,
        _ogg_bytes: &'static [u8],
    ) -> Result<(), String> {
        Ok(())
    }

    pub fn play(&mut self, request: SfxPlayRequest<'_>) -> Result<Option<SfxVoiceId>, String> {
        let request = request.normalized();
        if request.path.is_empty() {
            return Err("SFX path must not be empty".to_string());
        }
        if !self.settings.enabled {
            return Ok(None);
        }
        if (request.playback_rate - 1.0).abs() > 0.001 {
            return Err("iOS SFX playback-rate control is not implemented".to_string());
        }
        if !ensure_output() {
            return Ok(None);
        }

        self.update();
        if !self.make_voice_capacity(request.priority) {
            return Ok(None);
        }
        let clip = self.load_clip(request.path)?;
        let (left, right) = stereo_volume(request.volume * self.settings.mix_volume, request.pan);
        let id = self.allocate_voice_id();
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
        drop(state);
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

    /// Reclaims voices the mixer has finished with. The mixer drops a voice
    /// when its clip runs out, so "still in the mixer" is the liveness test.
    pub fn update(&mut self) {
        let Ok(state) = mixer().lock() else {
            return;
        };
        let live: Vec<u64> = state.voices.iter().map(|(id, _)| *id).collect();
        drop(state);
        self.voices.retain(|id, _| live.contains(&id.value()));
        self.order.retain(|id| live.contains(&id.value()));
    }

    pub fn set_mix_volume(&mut self, volume: f32) {
        self.settings.mix_volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.settings.enabled = enabled;
        if !enabled {
            self.stop_all();
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    pub fn active_voice_count(&self) -> usize {
        self.voices.len()
    }

    pub fn is_playing(&self, voice: SfxVoiceId) -> bool {
        mixer()
            .lock()
            .ok()
            .is_some_and(|state| state.voices.iter().any(|(id, _)| *id == voice.value()))
    }

    pub fn frame_demand(&self) -> Option<Duration> {
        (!self.voices.is_empty()).then_some(Duration::from_millis(100))
    }

    fn load_clip(&mut self, path: &str) -> Result<CachedClip, String> {
        if let Some(clip) = self.clips.get(path) {
            return Ok(clip.clone());
        }
        let clip = decode_clip(path)?;
        self.clips.insert(path.to_string(), clip.clone());
        Ok(clip)
    }

    fn make_voice_capacity(&mut self, incoming_priority: u8) -> bool {
        while self.voices.len() >= self.settings.maximum_voices {
            let Some(index) = super::oldest_evictable_voice(&self.order, incoming_priority, |id| {
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
