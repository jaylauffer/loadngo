//! iOS audio playback: loadngo's own backend, no `rodio` and no `cpal`.
//!
//! One RemoteIO `AudioUnit` runs a render callback that mixes a streaming
//! music track with short, resident sound effects.
//!
//! Music streams and effects don't, because the assets differ by three
//! orders of magnitude: the games' music runs 5-10 minutes (`flutterrung.ogg`
//! is 603 s, about 212 MB once decoded to `f32`), which no phone should hold
//! in memory, while their effects are 20 clips of at most 17 KB.
//!
//! The game thread never waits on audio (`docs/GAME_AUDIO_RUNTIME.md`):
//!
//! - The unit is created, started and stopped on loadngo's offload pool
//!   (`audio_output_idle.rs`), and stops after three silent seconds.
//! - Effects and voice lines are decoded on the pool, once, into a shared
//!   cache. An effect played before its clip is ready is dropped and
//!   counted, never decoded inline; a voice line starts when its decode
//!   finishes.
//! - Music is decoded a little ahead on the pool (`audio_stream.rs`), topped
//!   up from the music controller's `update`, so no track owns a thread.
//!
//! Everything arrives as a real file path. `ios.rs` sets `SNG_ASSETS_ROOT` to
//! the bundle's `assets/`, and the games use it, so -- exactly as on Android
//! -- `preload_embedded` has nothing to do here and says so.
//!
//! The unit runs at 48 kHz stereo. Every effect and one of the three music
//! tracks is already 48 kHz; the others are converted as they are decoded,
//! so the render callback never resamples.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use coreaudio_sys::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
    kAudioUnitManufacturer_Apple, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, kAudioUnitSubType_RemoteIO,
    kAudioUnitType_Output, AURenderCallbackStruct, AudioBufferList, AudioComponentDescription,
    AudioComponentFindNext, AudioComponentInstanceNew, AudioOutputUnitStart, AudioOutputUnitStop,
    AudioStreamBasicDescription, AudioTimeStamp, AudioUnit, AudioUnitInitialize,
    AudioUnitRenderActionFlags, AudioUnitSetProperty, OSStatus,
};

use super::mobile_mix::{
    decode_clip_file, stereo_volume, ActiveVoice, CachedClip, Voices, OUTPUT_SAMPLE_RATE,
};
use super::output_idle::{sooner, IdlePolicy, OutputSwitch};
use super::stream::{ChunkConsumer, DecodeAhead, Opener, Pop, ReadSeek, TICK};
use super::{SfxPlayRequest, SfxSettings, SfxVoiceId};

const OUTPUT_CHANNELS: u32 = 2;
/// The mixer id of the voice line; effect voices start at 1.
const VOICE_LINE_ID: u64 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicCueMode {
    OneShot,
    Loop,
}

// ------------------------------------------------------------------ clips

#[derive(Clone)]
enum ClipState {
    Loading,
    Ready(CachedClip),
    Failed(String),
}

/// Every effect clip, by path.
static CLIPS: OnceLock<Mutex<HashMap<String, ClipState>>> = OnceLock::new();

fn clips() -> &'static Mutex<HashMap<String, ClipState>> {
    CLIPS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Starts decoding `path` on the pool, unless it is already loaded or
/// loading. Returns at once.
fn request_clip(path: &str) {
    {
        let mut cache = clips().lock().unwrap_or_else(PoisonError::into_inner);
        if cache.contains_key(path) {
            return;
        }
        cache.insert(path.to_string(), ClipState::Loading);
    }
    let path = path.to_string();
    crate::offload::run_detached(move || {
        let state = match decode_clip_file(&path) {
            Ok(clip) => ClipState::Ready(clip),
            Err(error) => ClipState::Failed(error),
        };
        clips()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path, state);
    });
}

fn path_opener(path: &str) -> Opener {
    let path = path.to_string();
    Arc::new(move || {
        let file = File::open(&path).map_err(|err| format!("Missing music {path}: {err}"))?;
        Ok(Box::new(BufReader::new(file)) as Box<dyn ReadSeek>)
    })
}

// ------------------------------------------------------------------ mixing

struct MusicPlayback {
    chunks: ChunkConsumer,
    ahead: DecodeAhead,
    finished: bool,
}

#[derive(Default)]
struct MixerState {
    music: Option<MusicPlayback>,
    music_volume: f32,
    /// One-pole state for the bass shelf, per channel.
    bass_state: [f32; 2],
    bass_boost: f32,
    voices: Voices,
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
        if music.finished {
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
        // applied to the music bus whether or not any boost is mixed in, so
        // it is unconditional here too -- without it this backend ran the
        // music bus about a decibel hot relative to effects.
        const BASS_ONE_POLE: f32 = 0.0234;
        const MUSIC_POST_GAIN: f32 = 0.9;

        let volume = self.music_volume;
        let boost = self.bass_boost;
        for (index, sample) in out.iter_mut().enumerate() {
            let value = match music.chunks.pop() {
                Pop::Sample(value) => value,
                // Not decoded yet: silence for the rest of this buffer.
                Pop::Empty => break,
                Pop::Ended => {
                    music.finished = true;
                    break;
                }
            };
            let channel = index % 2;
            let low = self.bass_state[channel] + BASS_ONE_POLE * (value - self.bass_state[channel]);
            self.bass_state[channel] = low;
            *sample += (value + low * boost) * volume * MUSIC_POST_GAIN;
        }
    }

    fn mix_voices(&mut self, out: &mut [f32]) {
        self.voices.mix(out);
    }

    /// Nothing left to play: no effect, and no music or only a finished
    /// track. Paused music still plays (at zero volume), so it keeps the
    /// output running.
    fn is_quiet(&self) -> bool {
        self.voices.list.is_empty() && self.music.as_ref().is_none_or(|music| music.finished)
    }
}

// ------------------------------------------------------------------ output

struct Output {
    unit: AudioUnit,
}

// SAFETY: the unit is created, started and stopped only by `apply_output`,
// which the output switch runs one call at a time on the offload pool. The
// render callback reaches the mixer through an `Arc`, not through this
// handle.
unsafe impl Send for Output {}
unsafe impl Sync for Output {}

static MIXER: OnceLock<Arc<Mutex<MixerState>>> = OnceLock::new();
/// The unit, once created; `None` inside if RemoteIO would not start, which
/// is remembered rather than retried.
static OUTPUT: OnceLock<Option<Output>> = OnceLock::new();
/// When the unit runs and stops (see `audio_output_idle.rs`). It starts
/// stopped and starts on the first sound.
static IDLE: Mutex<IdlePolicy> = Mutex::new(IdlePolicy::new(false));
static SWITCH: OnceLock<OutputSwitch> = OnceLock::new();
/// RemoteIO could not be created: every controller is silent from here on.
static UNAVAILABLE: AtomicBool = AtomicBool::new(false);

fn mixer() -> &'static Arc<Mutex<MixerState>> {
    MIXER.get_or_init(|| {
        Arc::new(Mutex::new(MixerState {
            music_volume: 1.0,
            ..MixerState::default()
        }))
    })
}

fn switch() -> &'static OutputSwitch {
    SWITCH.get_or_init(|| {
        let mut started = false;
        OutputSwitch::new(false, move |run| apply_output(run, &mut started))
    })
}

/// Runs on the offload pool: creates the unit the first time, then starts
/// or stops it. `started` is the switch's own record of what it last did,
/// so a repeated request is a no-op rather than a second start.
fn apply_output(run: bool, started: &mut bool) {
    let created = OUTPUT.get().is_some();
    let output = OUTPUT.get_or_init(|| {
        configure_audio_session();
        // SAFETY: every pointer below is a live local, and the callback
        // outlives the unit because the mixer is a `'static` `OnceLock`.
        unsafe { start_unit() }.ok()
    });
    let Some(output) = output else {
        if !UNAVAILABLE.swap(true, Ordering::AcqRel) {
            eprintln!("[loadngo/ios] RemoteIO would not start; audio is silent");
        }
        silence_queued();
        return;
    };
    if !created {
        // `start_unit` starts it.
        *started = true;
    }
    if run == *started {
        return;
    }
    // SAFETY: a unit `start_unit` created and initialized; only this function
    // starts or stops it, one call at a time.
    if run {
        if unsafe { AudioOutputUnitStart(output.unit) } == 0 {
            *started = true;
        } else {
            eprintln!(
                "[loadngo/ios] audio output failed to restart; will try again on the next sound"
            );
            IDLE.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .open_failed();
            silence_queued();
        }
    } else {
        let _ = unsafe { AudioOutputUnitStop(output.unit) };
        *started = false;
    }
}

/// Nothing will play what was queued, and a voice that never finishes would
/// keep its controller asking for frames.
fn silence_queued() {
    if let Ok(mut state) = mixer().lock() {
        state.voices.list.clear();
        state.music = None;
    }
}

/// Something is about to play: start the unit if it is stopped. Returns at
/// once; the start happens on the pool. False if RemoteIO is unavailable.
fn wake_output() -> bool {
    if UNAVAILABLE.load(Ordering::Acquire) {
        return false;
    }
    let starting = IDLE.lock().unwrap_or_else(PoisonError::into_inner).wake();
    if starting {
        switch().request(true);
    }
    true
}

/// Stops the unit once the mixer has been silent for `IDLE_STOP`. Called
/// from the controllers' `update`, on the game thread; cheap.
fn maintain_output() {
    let quiet = mixer().lock().is_ok_and(|state| state.is_quiet());
    let stopping = IDLE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .observe(quiet, Instant::now());
    if stopping {
        switch().request(false);
    }
}

/// When the game must call `update` again for the unit to stop, folded into
/// every controller's `frame_demand`.
fn output_demand() -> Option<Duration> {
    let quiet = mixer().lock().is_ok_and(|state| state.is_quiet());
    IDLE.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .demand(quiet, Instant::now())
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
    Ok(Output { unit })
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
        if !wake_output() {
            return Err("iOS audio output unavailable".to_string());
        }
        let (ahead, chunks) = DecodeAhead::start(
            path_opener(&selected),
            looped,
            OUTPUT_SAMPLE_RATE,
            selected.clone(),
        );
        if let Ok(mut state) = mixer().lock() {
            state.music = Some(MusicPlayback {
                chunks,
                ahead,
                finished: false,
            });
            // A new track plays: any pause is over.
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

    /// Tops up the decode-ahead, advances the playlist when a track runs
    /// out, and lets the output stop once everything is silent. Must be
    /// called every frame while music plays: the decode-ahead depends on it,
    /// and [`Self::frame_demand`] asks for frames often enough.
    pub fn update(&mut self, dt: f32) {
        self.advance_playlist(dt);
        maintain_output();
    }

    fn advance_playlist(&mut self, _dt: f32) {
        let finished = {
            let Ok(state) = mixer().lock() else {
                return;
            };
            if let Some(music) = &state.music {
                music.ahead.tick();
            }
            state.music.as_ref().is_none_or(|music| music.finished)
        };
        if self.paused || !self.playlist_active || self.playlist_tracks.is_empty() || !finished {
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

    /// Every [`TICK`] while a track plays, so its queue is topped up and its
    /// end noticed; then once more when the output can stop.
    pub fn frame_demand(&self) -> Option<Duration> {
        let playing = mixer()
            .lock()
            .is_ok_and(|state| state.music.as_ref().is_some_and(|music| !music.finished));
        sooner(playing.then_some(TICK), output_demand())
    }
}

pub struct VoiceController {
    enabled: bool,
    volume: f32,
    /// The generation of a voice line still decoding on the pool, 0 if none.
    /// The decode job starts the line only if it is still the one wanted.
    pending: Arc<AtomicU64>,
    generation: u64,
}

impl VoiceController {
    pub fn new(enabled: bool, volume: f32) -> Self {
        Self {
            enabled,
            volume,
            pending: Arc::new(AtomicU64::new(0)),
            generation: 0,
        }
    }

    /// Decodes `path` on the pool and plays it when ready, replacing any
    /// line playing now. Single-voice playback shares the effect mixer:
    /// these are short spoken cues, not streams. Returns at once.
    pub fn play_path(&mut self, path: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if !wake_output() {
            return Err("iOS audio output unavailable".to_string());
        }
        self.stop();
        self.generation = self.generation.wrapping_add(1).max(1);
        let generation = self.generation;
        self.pending.store(generation, Ordering::Release);
        let pending = Arc::clone(&self.pending);
        let volume = self.volume;
        let path = path.to_string();
        crate::offload::run_detached(move || {
            let decoded = decode_clip_file(&path);
            if pending
                .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }
            match decoded {
                Ok(clip) => {
                    if let Ok(mut state) = mixer().lock() {
                        state.voices.list.push((
                            VOICE_LINE_ID,
                            ActiveVoice {
                                samples: clip.samples,
                                cursor: 0,
                                left: volume,
                                right: volume,
                                looped: false,
                            },
                        ));
                    }
                }
                Err(error) => eprintln!("[loadngo/ios] voice {path}: {error}"),
            }
        });
        Ok(())
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
    }

    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        self.enabled = enabled;
        if !enabled {
            self.stop();
        }
        self.enabled
    }

    /// True while the line decodes or plays.
    pub fn is_playing(&self) -> bool {
        self.pending.load(Ordering::Acquire) != 0
            || mixer()
                .lock()
                .ok()
                .is_some_and(|state| state.voices.contains(VOICE_LINE_ID))
    }

    pub fn stop(&mut self) {
        self.pending.store(0, Ordering::Release);
        if let Ok(mut state) = mixer().lock() {
            state.voices.remove(VOICE_LINE_ID);
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn frame_demand(&self) -> Option<Duration> {
        sooner(
            (self.enabled && self.is_playing()).then_some(Duration::from_millis(100)),
            output_demand(),
        )
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

    /// Starts decoding `path` on the pool so its first `play` finds it
    /// ready. Returns at once.
    pub fn preload_path(&mut self, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Err("SFX path must not be empty".to_string());
        }
        request_clip(path);
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
        if !self.settings.enabled {
            return Ok(None);
        }
        if (request.playback_rate - 1.0).abs() > 0.001 {
            return Err("iOS SFX playback-rate control is not implemented".to_string());
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
                request_clip(request.path);
                return Ok(None);
            }
        };
        if !wake_output() {
            return Ok(None);
        }

        self.update();
        if !self.make_voice_capacity(request.priority) {
            return Ok(None);
        }
        let (left, right) = stereo_volume(request.volume * self.settings.mix_volume, request.pan);
        let id = self.allocate_voice_id();
        let Ok(mut state) = mixer().lock() else {
            return Ok(None);
        };
        state.voices.list.push((
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
            state.voices.remove(voice.value());
        }
        self.voices.remove(&voice);
        self.order.retain(|candidate| *candidate != voice);
    }

    pub fn stop_all(&mut self) {
        if let Ok(mut state) = mixer().lock() {
            for id in self.voices.keys() {
                state.voices.remove(id.value());
            }
        }
        self.voices.clear();
        self.order.clear();
    }

    /// Reclaims voices the mixer has finished with, and lets the output stop
    /// once everything has been silent for a while. The mixer drops a voice
    /// when its clip runs out, so "still in the mixer" is the liveness test.
    pub fn update(&mut self) {
        if let Ok(state) = mixer().lock() {
            self.voices
                .retain(|id, _| state.voices.contains(id.value()));
            self.order.retain(|id| state.voices.contains(id.value()));
        }
        maintain_output();
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

    /// Plays dropped so far because their clip was still decoding.
    pub fn not_ready_count(&self) -> u64 {
        self.not_ready
    }

    pub fn is_playing(&self, voice: SfxVoiceId) -> bool {
        mixer()
            .lock()
            .ok()
            .is_some_and(|state| state.voices.contains(voice.value()))
    }

    pub fn frame_demand(&self) -> Option<Duration> {
        sooner(
            (!self.voices.is_empty()).then_some(Duration::from_millis(100)),
            output_demand(),
        )
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
                state.voices.remove(oldest.value());
            }
        }
        true
    }

    fn allocate_voice_id(&mut self) -> SfxVoiceId {
        let id = SfxVoiceId(self.next_voice_id);
        // Never 0: that is the voice line's id.
        self.next_voice_id = self.next_voice_id.wrapping_add(1).max(1);
        id
    }
}
