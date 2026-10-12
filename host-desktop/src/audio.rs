#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SfxSettings {
    pub enabled: bool,
    pub mix_volume: f32,
    pub maximum_voices: usize,
}

impl Default for SfxSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mix_volume: 1.0,
            maximum_voices: 24,
        }
    }
}

impl SfxSettings {
    fn normalized(self) -> Self {
        Self {
            enabled: self.enabled,
            mix_volume: finite_clamped(self.mix_volume, 0.0, 2.0, 1.0),
            maximum_voices: self.maximum_voices.max(1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SfxPlayRequest<'a> {
    pub path: &'a str,
    pub volume: f32,
    pub pan: f32,
    pub playback_rate: f32,
    pub looped: bool,
    pub priority: u8,
}

impl<'a> SfxPlayRequest<'a> {
    #[must_use]
    pub const fn one_shot(path: &'a str) -> Self {
        Self {
            path,
            volume: 1.0,
            pan: 0.0,
            playback_rate: 1.0,
            looped: false,
            priority: 128,
        }
    }

    fn normalized(self) -> Self {
        Self {
            path: self.path.trim(),
            volume: finite_clamped(self.volume, 0.0, 2.0, 1.0),
            pan: finite_clamped(self.pan, -1.0, 1.0, 0.0),
            playback_rate: finite_clamped(self.playback_rate, 0.5, 2.0, 1.0),
            looped: self.looped,
            priority: self.priority,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SfxVoiceId(u64);

impl SfxVoiceId {
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

pub(crate) fn finite_clamped(value: f32, minimum: f32, maximum: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        fallback
    }
}

fn oldest_evictable_voice(
    order: &std::collections::VecDeque<SfxVoiceId>,
    incoming_priority: u8,
    mut priority_for: impl FnMut(SfxVoiceId) -> Option<u8>,
) -> Option<usize> {
    order
        .iter()
        .position(|id| priority_for(*id).is_some_and(|priority| priority <= incoming_priority))
}

/// The effect-mixing core shared by the iOS and Android backends; see
/// `docs/GAME_AUDIO_RUNTIME.md`. Also compiled for desktop tests so its
/// mixing rules are checked on every host.
#[cfg(any(
    target_os = "ios",
    target_os = "android",
    all(test, not(target_os = "netbsd"))
))]
#[path = "audio_mobile_mix.rs"]
mod mobile_mix;

/// Closing the output after a silent stretch, for iOS and desktop (Android
/// has its own).
#[cfg(any(
    target_os = "ios",
    all(not(target_os = "android"), not(target_os = "netbsd")),
    all(test, not(target_os = "netbsd"))
))]
#[path = "audio_output_idle.rs"]
mod output_idle;

#[cfg(target_os = "android")]
mod imp {
    use super::{SfxPlayRequest, SfxSettings, SfxVoiceId};
    use crate::android;
    use crate::android_jni::{call_bool, call_int, call_void, with_env};
    use jni::objects::{GlobalRef, JObject, JValue};
    use std::{
        collections::{HashMap, VecDeque},
        time::{Duration, Instant},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MusicCueMode {
        OneShot,
        Loop,
    }

    fn bass_boost_strength(bass_boost: f32) -> u16 {
        (bass_boost.clamp(0.0, 1.0) * 1000.0).round() as u16
    }

    struct MediaPlayerHandle {
        player: GlobalRef,
    }

    impl MediaPlayerHandle {
        fn create(path: &str, looped: bool) -> Result<Self, String> {
            with_env(|env| {
                let player = env
                    .new_object("android/media/MediaPlayer", "()V", &[])
                    .map_err(|err| format!("Failed to allocate MediaPlayer: {err}"))?;
                let global = env
                    .new_global_ref(player)
                    .map_err(|err| format!("Failed to globalize MediaPlayer: {err}"))?;
                let path_string = env
                    .new_string(path)
                    .map_err(|err| format!("Failed to create MediaPlayer data source: {err}"))?;
                let path_obj = JObject::from(path_string);
                call_void(
                    env,
                    global.as_obj(),
                    "setAudioStreamType",
                    "(I)V",
                    &[JValue::Int(3)],
                )?;
                call_void(
                    env,
                    global.as_obj(),
                    "setDataSource",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&path_obj)],
                )?;
                call_void(
                    env,
                    global.as_obj(),
                    "setLooping",
                    "(Z)V",
                    &[JValue::Bool(u8::from(looped))],
                )?;
                call_void(env, global.as_obj(), "prepare", "()V", &[])?;
                Ok(Self { player: global })
            })
        }

        fn set_volume(&mut self, volume: f32) -> Result<(), String> {
            let volume = volume.clamp(0.0, 2.0);
            self.set_stereo_volume(volume, volume)
        }

        fn set_stereo_volume(&mut self, left: f32, right: f32) -> Result<(), String> {
            let left = left.clamp(0.0, 2.0);
            let right = right.clamp(0.0, 2.0);
            with_env(|env| {
                call_void(
                    env,
                    self.player.as_obj(),
                    "setVolume",
                    "(FF)V",
                    &[JValue::Float(left), JValue::Float(right)],
                )
            })
        }

        fn play(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, self.player.as_obj(), "start", "()V", &[]))
        }

        fn pause(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, self.player.as_obj(), "pause", "()V", &[]))
        }

        fn stop(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, self.player.as_obj(), "stop", "()V", &[]))
        }

        fn is_playing(&self) -> bool {
            with_env(|env| call_bool(env, self.player.as_obj(), "isPlaying", "()Z", &[]))
                .unwrap_or(false)
        }

        fn release(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, self.player.as_obj(), "release", "()V", &[]))
        }

        fn audio_session_id(&self) -> Result<i32, String> {
            with_env(|env| call_int(env, self.player.as_obj(), "getAudioSessionId", "()I", &[]))
        }
    }

    impl Drop for MediaPlayerHandle {
        fn drop(&mut self) {
            let _ = self.release();
        }
    }

    struct BassBoostHandle {
        effect: GlobalRef,
    }

    impl BassBoostHandle {
        fn create(audio_session_id: i32, strength: u16) -> Result<Self, String> {
            with_env(|env| {
                let effect = env
                    .new_object(
                        "android/media/audiofx/BassBoost",
                        "(II)V",
                        &[JValue::Int(0), JValue::Int(audio_session_id)],
                    )
                    .map_err(|err| format!("Failed to allocate Android BassBoost: {err}"))?;
                let global = env
                    .new_global_ref(effect)
                    .map_err(|err| format!("Failed to globalize Android BassBoost: {err}"))?;
                let mut handle = Self { effect: global };
                handle.set_strength(strength)?;
                Ok(handle)
            })
        }

        fn set_strength(&mut self, strength: u16) -> Result<(), String> {
            let enabled = strength > 0;
            with_env(|env| {
                call_void(
                    env,
                    self.effect.as_obj(),
                    "setStrength",
                    "(S)V",
                    &[JValue::Short(strength as i16)],
                )?;
                let status = call_int(
                    env,
                    self.effect.as_obj(),
                    "setEnabled",
                    "(Z)I",
                    &[JValue::Bool(u8::from(enabled))],
                )?;
                if status != 0 {
                    return Err(format!(
                        "Android BassBoost::setEnabled returned status {status}"
                    ));
                }
                Ok(())
            })
        }

        fn release(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, self.effect.as_obj(), "release", "()V", &[]))
        }
    }

    impl Drop for BassBoostHandle {
        fn drop(&mut self) {
            let _ = self.release();
        }
    }

    pub struct MusicController {
        player: Option<MediaPlayerHandle>,
        bass_boost_effect: Option<BassBoostHandle>,
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
        loop_current_track: bool,
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
                player: None,
                bass_boost_effect: None,
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
                loop_current_track: false,
                paused: false,
            }
        }

        /// Pauses playback in place (Android `MediaPlayer.pause`, resumable
        /// from the same position). Also suspends `update`'s finished-track
        /// detection: `MediaPlayer.isPlaying()` goes false while paused,
        /// which would otherwise look identical to the track having ended
        /// and trigger a from-the-top restart instead of a true resume.
        pub fn pause(&mut self) {
            self.paused = true;
            if let Some(player) = self.player.as_mut() {
                if let Err(err) = player.pause() {
                    android::android_log_error(&format!("Android music pause failed: {err}"));
                }
            }
        }

        pub fn resume(&mut self) {
            self.paused = false;
            if let Some(player) = self.player.as_mut() {
                if let Err(err) = player.play() {
                    android::android_log_error(&format!("Android music resume failed: {err}"));
                }
            }
        }

        fn clear_bass_boost_effect(&mut self) {
            if let Some(mut effect) = self.bass_boost_effect.take() {
                if let Err(err) = effect.release() {
                    android::android_log_error(&format!("Android BassBoost release failed: {err}"));
                }
            }
        }

        fn sync_bass_boost_effect(&mut self) {
            self.clear_bass_boost_effect();

            let strength = bass_boost_strength(self.bass_boost);
            if strength == 0 {
                return;
            }

            let Some(player) = self.player.as_ref() else {
                return;
            };
            let session_id = match player.audio_session_id() {
                Ok(session_id) => session_id,
                Err(err) => {
                    android::android_log_error(&format!(
                        "Android BassBoost session lookup failed: {err}"
                    ));
                    return;
                }
            };

            match BassBoostHandle::create(session_id, strength) {
                Ok(effect) => {
                    android::android_log_info(&format!(
                        "Android BassBoost attached session={} strength={}",
                        session_id, strength
                    ));
                    self.bass_boost_effect = Some(effect);
                }
                Err(err) => {
                    android::android_log_error(&format!(
                        "Android BassBoost attach failed for session {}: {}",
                        session_id, err
                    ));
                }
            }
        }

        fn next_playlist_track(&mut self) -> Option<String> {
            if self.playlist_tracks.is_empty() {
                return None;
            }
            self.playlist_index = (self.playlist_index + 1) % self.playlist_tracks.len();
            Some(self.playlist_tracks[self.playlist_index].clone())
        }

        pub fn play_track_path(
            &mut self,
            path: &str,
            _fade: f32,
            looped: bool,
        ) -> Result<(), String> {
            let selected_path = if path.trim().is_empty() {
                self.boot_track_path.clone()
            } else {
                path.trim().to_string()
            };
            let selected_path = android::ensure_materialized_asset_path(&selected_path)?;
            if self.active_track.as_deref() == Some(selected_path.as_str())
                && self
                    .player
                    .as_ref()
                    .is_some_and(|player| player.is_playing())
            {
                android::android_log_info(&format!(
                    "Android music ignoring duplicate active track {}",
                    selected_path
                ));
                return Ok(());
            }
            let volume = self.mix_volume;
            self.clear_bass_boost_effect();
            if let Some(mut player) = self.player.take() {
                let _ = player.stop();
                let _ = player.release();
            }
            let mut player = MediaPlayerHandle::create(&selected_path, looped)?;
            player.set_volume(volume)?;
            player.play()?;
            android::android_log_info(&format!(
                "Android music playing {} looped={}",
                selected_path, looped
            ));
            self.player = Some(player);
            self.sync_bass_boost_effect();
            self.active_track = Some(selected_path.clone());
            self.track_started_at = Some(Instant::now());
            self.loop_current_track = looped;
            Ok(())
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
                return self.play_track_path(&self.boot_track_path.clone(), fade, false);
            }
            let mut attempts = 0usize;
            let mut last_err = None;
            while attempts < self.playlist_tracks.len() {
                attempts += 1;
                let Some(track) = self.next_playlist_track() else {
                    break;
                };
                match self.play_track_path(&track, fade, false) {
                    Ok(()) => return Ok(()),
                    Err(err) => {
                        eprintln!("Skipping playlist track {track}: {err}");
                        last_err = Some(err);
                    }
                }
            }
            Err(last_err.unwrap_or_else(|| "No playable tracks in playlist".to_string()))
        }

        pub fn start_playlist(&mut self, fade: f32) -> Result<(), String> {
            self.playlist_index = 0;
            self.playlist_mode_active = true;
            self.resume_playlist_after_cue = false;
            self.resume_playlist_from_next_track = false;
            android::android_log_info("Android music playlist start");
            let result = self.play_playlist_current(fade);
            if let Err(err) = &result {
                android::android_log_error(&format!("Android music playlist start failed: {err}"));
            }
            result
        }

        pub fn fade_to_path(&mut self, path: &str, fade: f32) -> Result<(), String> {
            self.resume_playlist_after_cue = !self.playlist_tracks.is_empty();
            self.resume_playlist_from_next_track = self.playlist_mode_active;
            self.playlist_mode_active = false;
            let looped = self.playlist_tracks.is_empty() && self.cue_mode == MusicCueMode::Loop;
            android::android_log_info(&format!("Android music cue {} looped={}", path, looped));
            let result = self.play_track_path(path, fade, looped);
            if let Err(err) = &result {
                android::android_log_error(&format!(
                    "Android music cue playback failed for {path}: {err}"
                ));
            }
            result
        }

        /// No-op here: Android already has a real packaging story (assets
        /// bundled into the APK, extracted to a real on-device path that
        /// `fade_to_path`/`play_track_path` already use — see
        /// `android.rs`'s `configure_runtime_env`). Exists only so desktop
        /// callers can call `preload_embedded` unconditionally without
        /// `cfg`-gating every call site.
        pub fn preload_embedded(&mut self, _key: &str, _ogg_bytes: &'static [u8]) {}

        pub fn update(&mut self, _dt: f32) {
            if self.paused {
                return;
            }
            let finished = self
                .player
                .as_ref()
                .is_some_and(|player| !player.is_playing());
            if !finished {
                return;
            }

            if self.loop_current_track {
                if let Some(active) = self.active_track.clone() {
                    android::android_log_info(&format!("Android music loop restart {}", active));
                    if let Err(err) = self.play_track_path(&active, 0.0, true) {
                        eprintln!("Android loop restart failed for {active}: {err}");
                    }
                }
                return;
            }

            if !self.playlist_mode_active {
                if self.resume_playlist_after_cue {
                    self.resume_playlist_after_cue = false;
                    self.playlist_mode_active = true;
                    if self.resume_playlist_from_next_track {
                        android::android_log_info("Android music resuming playlist at next track");
                        if let Err(err) = self.play_next_playlist(0.1) {
                            eprintln!("Android playlist resume failed: {err}");
                        }
                    } else {
                        android::android_log_info(
                            "Android music resuming playlist at current track",
                        );
                        if let Err(err) = self.play_playlist_current(0.1) {
                            eprintln!("Android playlist resume failed: {err}");
                        }
                    }
                    self.resume_playlist_from_next_track = false;
                }
                return;
            }
            if self
                .track_started_at
                .is_some_and(|started| started.elapsed() < Duration::from_secs(2))
            {
                return;
            }
            if let Some(active) = self.active_track.clone() {
                android::android_log_info(&format!(
                    "Android music playlist advance after {}",
                    active
                ));
                if let Err(err) = self.play_next_playlist(0.1) {
                    eprintln!("Playlist advance failed after {active}: {err}");
                }
            }
        }

        pub fn set_mix_volume(&mut self, volume: f32) {
            let volume = volume.clamp(0.0, 2.0);
            if (self.mix_volume - volume).abs() <= 0.001 {
                return;
            }
            self.mix_volume = volume;
            if let Some(player) = self.player.as_mut() {
                if let Err(err) = player.set_volume(self.mix_volume) {
                    eprintln!("Android music volume update failed: {err}");
                }
            }
        }

        pub fn set_bass_boost(&mut self, bass_boost: f32) {
            let bass_boost = bass_boost.clamp(0.0, 1.0);
            if (self.bass_boost - bass_boost).abs() <= 0.001 {
                return;
            }
            self.bass_boost = bass_boost;
            self.sync_bass_boost_effect();
        }

        pub fn active_track(&self) -> Option<&str> {
            self.active_track.as_deref()
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            if self.player.is_some()
                && (self.playlist_mode_active
                    || self.resume_playlist_after_cue
                    || self.loop_current_track)
            {
                return Some(Duration::from_millis(100));
            }
            None
        }
    }

    pub struct VoiceController {
        enabled: bool,
        volume: f32,
        player: Option<MediaPlayerHandle>,
    }

    impl VoiceController {
        fn stop_player(player: &mut MediaPlayerHandle) {
            if let Err(err) = player.stop() {
                eprintln!("Android voice stop failed: {err}");
            }
            if let Err(err) = player.release() {
                eprintln!("Android voice release failed: {err}");
            }
        }

        pub fn new(enabled: bool, volume: f32) -> Self {
            let mut controller = Self {
                enabled: false,
                volume,
                player: None,
            };
            controller.set_enabled(enabled);
            controller
        }

        pub fn play_path(&mut self, path: &str) -> Result<(), String> {
            if !self.enabled {
                return Ok(());
            }
            let result = (|| {
                let path = android::ensure_materialized_asset_path(path)?;
                if let Some(mut player) = self.player.take() {
                    Self::stop_player(&mut player);
                }
                let mut player = MediaPlayerHandle::create(&path, false)?;
                let volume = self.volume;
                player.set_volume(volume)?;
                player.play()?;
                self.player = Some(player);
                android::android_log_info(&format!("Android voice playing {path}"));
                Ok(())
            })();
            if let Err(err) = &result {
                android::android_log_error(&format!(
                    "Android voice playback failed for {path}: {err}"
                ));
            }
            result
        }

        pub fn set_volume(&mut self, volume: f32) {
            let volume = volume.clamp(0.0, 2.0);
            if (self.volume - volume).abs() <= 0.001 {
                return;
            }
            self.volume = volume;
            if let Some(player) = self.player.as_mut() {
                if let Err(err) = player.set_volume(self.volume) {
                    eprintln!("Android voice volume update failed: {err}");
                }
            }
        }

        pub fn set_enabled(&mut self, enabled: bool) -> bool {
            self.enabled = enabled;
            if !enabled {
                self.stop();
            }
            self.enabled
        }

        pub fn is_playing(&mut self) -> bool {
            self.enabled
                && self
                    .player
                    .as_ref()
                    .is_some_and(|player| player.is_playing())
        }

        pub fn stop(&mut self) {
            if let Some(mut player) = self.player.take() {
                Self::stop_player(&mut player);
            }
        }

        pub fn is_enabled(&self) -> bool {
            self.enabled
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            if self.enabled
                && self
                    .player
                    .as_ref()
                    .is_some_and(|player| player.is_playing())
            {
                return Some(Duration::from_millis(100));
            }
            None
        }
    }

    // ------------------------------------------------------------ effects
    //
    // Effects mix in an AAudio data callback (see `docs/GAME_AUDIO_RUNTIME.md`).
    // A `MediaPlayer` per effect cost the main thread 40-53 ms each, measured
    // on a Xiaomi 22111317I. Now the game thread only admits voices: clips are
    // read through the host proactor and decoded in the completion, on the
    // proactor's own thread, and the output stream is opened, started and
    // stopped there too.

    use super::mobile_mix::{decode_clip_bytes, stereo_volume, ActiveVoice, CachedClip, Voices};
    use std::collections::HashSet;
    use std::ffi::c_void;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};

    /// Voices the callback can hold without growing its list.
    const MIXER_VOICE_CAPACITY: usize = 64;
    /// Stop the output after this long with nothing playing, so an idle game
    /// does not keep an audio callback waking the CPU.
    const IDLE_STOP: Duration = Duration::from_secs(3);

    static VOICES: OnceLock<Mutex<Voices>> = OnceLock::new();
    static CLIPS: OnceLock<Mutex<HashMap<String, CachedClip>>> = OnceLock::new();
    static OUTPUT: OnceLock<Mutex<OutputState>> = OnceLock::new();
    /// Set by the AAudio error callback (for example when headphones are
    /// unplugged); the stream is reopened on the proactor thread, never in
    /// the callback.
    static OUTPUT_LOST: AtomicBool = AtomicBool::new(false);

    fn voices() -> &'static Mutex<Voices> {
        VOICES.get_or_init(|| Mutex::new(Voices::with_capacity(MIXER_VOICE_CAPACITY)))
    }

    fn clips() -> &'static Mutex<HashMap<String, CachedClip>> {
        CLIPS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    struct OutputState {
        stream: *mut ndk_sys::AAudioStream,
        running: bool,
        failed: bool,
    }

    // SAFETY: the stream pointer is only opened, started, stopped and closed
    // on the proactor thread, under this mutex; the data callback reaches the
    // voices through `VOICES`, never through this state.
    unsafe impl Send for OutputState {}

    fn output() -> &'static Mutex<OutputState> {
        OUTPUT.get_or_init(|| {
            Mutex::new(OutputState {
                stream: std::ptr::null_mut(),
                running: false,
                failed: false,
            })
        })
    }

    /// Asks the proactor thread to bring the output to `running`.
    fn request_output(running: bool) {
        let submitted = android::proactor_handle()
            .enqueue_work(move |_completion: loadngo_proactor::Completion| apply_output(running));
        if let Err(err) = submitted {
            android::android_log_error(&format!("Android SFX output request failed: {err}"));
        }
    }

    /// Runs on the proactor thread.
    fn apply_output(running: bool) {
        let Ok(mut state) = output().lock() else {
            return;
        };
        if OUTPUT_LOST.swap(false, Ordering::AcqRel) && !state.stream.is_null() {
            // SAFETY: the stream was opened here and nothing else closes it.
            unsafe { ndk_sys::AAudioStream_close(state.stream) };
            state.stream = std::ptr::null_mut();
            state.running = false;
        }
        if running {
            if state.failed {
                return;
            }
            if state.stream.is_null() {
                match open_stream() {
                    Ok(stream) => state.stream = stream,
                    Err(err) => {
                        state.failed = true;
                        android::android_log_error(&format!(
                            "Android SFX output unavailable: {err}"
                        ));
                        return;
                    }
                }
            }
            if !state.running {
                // SAFETY: a live stream opened by `open_stream`.
                let result = unsafe { ndk_sys::AAudioStream_requestStart(state.stream) };
                state.running = result == ndk_sys::AAUDIO_OK;
            }
        } else if state.running && !state.stream.is_null() {
            // SAFETY: a live stream opened by `open_stream`.
            unsafe { ndk_sys::AAudioStream_requestStop(state.stream) };
            state.running = false;
        }
    }

    fn open_stream() -> Result<*mut ndk_sys::AAudioStream, String> {
        let mut builder: *mut ndk_sys::AAudioStreamBuilder = std::ptr::null_mut();
        // SAFETY: plain AAudio builder calls on pointers this function owns;
        // the builder is deleted on every path.
        unsafe {
            let result = ndk_sys::AAudio_createStreamBuilder(&mut builder);
            if result != ndk_sys::AAUDIO_OK || builder.is_null() {
                return Err(format!("AAudio_createStreamBuilder returned {result}"));
            }
            ndk_sys::AAudioStreamBuilder_setFormat(builder, ndk_sys::AAUDIO_FORMAT_PCM_FLOAT);
            ndk_sys::AAudioStreamBuilder_setChannelCount(builder, 2);
            ndk_sys::AAudioStreamBuilder_setSampleRate(
                builder,
                super::mobile_mix::OUTPUT_SAMPLE_RATE as i32,
            );
            ndk_sys::AAudioStreamBuilder_setPerformanceMode(
                builder,
                ndk_sys::AAUDIO_PERFORMANCE_MODE_LOW_LATENCY as i32,
            );
            ndk_sys::AAudioStreamBuilder_setSharingMode(
                builder,
                ndk_sys::AAUDIO_SHARING_MODE_SHARED as i32,
            );
            ndk_sys::AAudioStreamBuilder_setUsage(builder, ndk_sys::AAUDIO_USAGE_GAME as i32);
            ndk_sys::AAudioStreamBuilder_setDataCallback(
                builder,
                Some(render),
                std::ptr::null_mut(),
            );
            ndk_sys::AAudioStreamBuilder_setErrorCallback(
                builder,
                Some(on_error),
                std::ptr::null_mut(),
            );
            let mut stream: *mut ndk_sys::AAudioStream = std::ptr::null_mut();
            let result = ndk_sys::AAudioStreamBuilder_openStream(builder, &mut stream);
            ndk_sys::AAudioStreamBuilder_delete(builder);
            if result != ndk_sys::AAUDIO_OK || stream.is_null() {
                return Err(format!("AAudioStreamBuilder_openStream returned {result}"));
            }
            android::android_log_info(&format!(
                "Android SFX output open: {} Hz, {} channels, format {}, burst {} frames",
                ndk_sys::AAudioStream_getSampleRate(stream),
                ndk_sys::AAudioStream_getChannelCount(stream),
                ndk_sys::AAudioStream_getFormat(stream),
                ndk_sys::AAudioStream_getFramesPerBurst(stream),
            ));
            Ok(stream)
        }
    }

    /// The AAudio data callback: mixes the voices into `audio_data`. Takes
    /// the voice lock with `try_lock` and writes silence if the game thread
    /// holds it, because blocking here would glitch far worse than one
    /// silent burst. Never allocates.
    unsafe extern "C" fn render(
        _stream: *mut ndk_sys::AAudioStream,
        _user_data: *mut c_void,
        audio_data: *mut c_void,
        num_frames: i32,
    ) -> ndk_sys::aaudio_data_callback_result_t {
        let samples = usize::try_from(num_frames).unwrap_or(0) * 2;
        if !audio_data.is_null() && samples > 0 {
            // SAFETY: AAudio hands over `num_frames` frames of the float
            // stereo format this stream was opened with.
            let out = unsafe { std::slice::from_raw_parts_mut(audio_data.cast::<f32>(), samples) };
            out.fill(0.0);
            let mixed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Ok(mut voices) = voices().try_lock() {
                    voices.mix(out);
                }
            }));
            if mixed.is_err() {
                out.fill(0.0);
            }
            for sample in out.iter_mut() {
                *sample = sample.clamp(-1.0, 1.0);
            }
        }
        ndk_sys::AAUDIO_CALLBACK_RESULT_CONTINUE as ndk_sys::aaudio_data_callback_result_t
    }

    unsafe extern "C" fn on_error(
        _stream: *mut ndk_sys::AAudioStream,
        _user_data: *mut c_void,
        _error: ndk_sys::aaudio_result_t,
    ) {
        OUTPUT_LOST.store(true, Ordering::Release);
    }

    /// Reads `path` through the proactor and decodes it in the completion,
    /// on the proactor thread, into `CLIPS`.
    fn load_clip_async(path: String) {
        let handle = android::proactor_handle();
        let submitted =
            handle
                .clone()
                .enqueue_work(move |_completion: loadngo_proactor::Completion| {
                    let resolved = match android::ensure_materialized_asset_path(&path) {
                        Ok(resolved) => resolved,
                        Err(err) => {
                            android::android_log_error(&format!("SFX {path} unavailable: {err}"));
                            return;
                        }
                    };
                    let file = match std::fs::File::open(&resolved) {
                        Ok(file) => file,
                        Err(err) => {
                            android::android_log_error(&format!(
                                "SFX {resolved} unreadable: {err}"
                            ));
                            return;
                        }
                    };
                    let length = file
                        .metadata()
                        .map(|metadata| usize::try_from(metadata.len()).unwrap_or(0))
                        .unwrap_or(0);
                    let fd = file.as_raw_fd();
                    let read = handle.read(
                        fd,
                        loadngo_proactor::IoBuf::with_capacity(length),
                        0,
                        move |result: loadngo_proactor::IoResult| {
                            // The file stays open until the read completes.
                            let _file = file;
                            let decoded =
                                result.map_err(|err| err.to_string()).and_then(|transfer| {
                                    let read = transfer.bytes_transferred as usize;
                                    if read != length {
                                        return Err(format!("short read {read} of {length} bytes"));
                                    }
                                    decode_clip_bytes(&path, transfer.buf.into_vec())
                                });
                            match decoded {
                                Ok(clip) => {
                                    if let Ok(mut clips) = clips().lock() {
                                        clips.insert(path, clip);
                                    }
                                }
                                Err(err) => android::android_log_error(&format!(
                                    "SFX {path} failed to load: {err}"
                                )),
                            }
                        },
                    );
                    if let Err(err) = read {
                        android::android_log_error(&format!("SFX {resolved} read failed: {err}"));
                    }
                });
        if let Err(err) = submitted {
            android::android_log_error(&format!("SFX preload could not be queued: {err}"));
        }
    }

    pub struct SfxController {
        settings: SfxSettings,
        /// Priority, volume and pan of each live voice.
        voices: HashMap<SfxVoiceId, (u8, f32, f32)>,
        order: VecDeque<SfxVoiceId>,
        next_voice_id: u64,
        requested: HashSet<String>,
        idle_since: Option<Instant>,
        output_running: bool,
        timing: SfxTiming,
    }

    /// Opt-in (`LOADNGO_FRAME_METRICS`): wall time the calling thread spends
    /// inside SFX calls, reported every 5 s, with effects dropped because
    /// their clip had not finished loading.
    struct SfxTiming {
        enabled: bool,
        plays: u64,
        not_ready: u64,
        play_us: u64,
        play_max_us: u64,
        updates: u64,
        update_us: u64,
        update_max_us: u64,
        since: Instant,
    }

    impl SfxTiming {
        fn new() -> Self {
            let enabled = crate::debug_config_value("LOADNGO_FRAME_METRICS")
                .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"));
            Self {
                enabled,
                plays: 0,
                not_ready: 0,
                play_us: 0,
                play_max_us: 0,
                updates: 0,
                update_us: 0,
                update_max_us: 0,
                since: Instant::now(),
            }
        }

        fn record(&mut self, play: bool, started: Instant) {
            let us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            if play {
                self.plays += 1;
                self.play_us += us;
                self.play_max_us = self.play_max_us.max(us);
            } else {
                self.updates += 1;
                self.update_us += us;
                self.update_max_us = self.update_max_us.max(us);
            }
            let window = self.since.elapsed();
            if window >= Duration::from_secs(5) {
                crate::android::android_log_info(&format!(
                    "[loadngo-sfx] window={:.1}s plays={} not_ready={} play_total={}ms play_mean={:.3}ms play_max={:.2}ms updates={} update_total={}ms update_max={:.2}ms busy={:.2}%",
                    window.as_secs_f64(),
                    self.plays,
                    self.not_ready,
                    self.play_us / 1000,
                    self.play_us as f64 / 1000.0 / self.plays.max(1) as f64,
                    self.play_max_us as f64 / 1000.0,
                    self.updates,
                    self.update_us / 1000,
                    self.update_max_us as f64 / 1000.0,
                    (self.play_us + self.update_us) as f64 / 10_000.0 / window.as_secs_f64(),
                ));
                *self = Self::new();
            }
        }
    }

    impl SfxController {
        pub fn new(settings: SfxSettings) -> Self {
            Self {
                settings: settings.normalized(),
                voices: HashMap::new(),
                order: VecDeque::new(),
                next_voice_id: 1,
                requested: HashSet::new(),
                idle_since: None,
                output_running: false,
                timing: SfxTiming::new(),
            }
        }

        /// No-op here: Android plays effects by asset path, loaded through
        /// `preload_path`. Exists so desktop callers can call
        /// `preload_embedded` unconditionally without `cfg`-gating every call
        /// site.
        pub fn preload_embedded(
            &mut self,
            _key: &str,
            _ogg_bytes: &'static [u8],
        ) -> Result<(), String> {
            Ok(())
        }

        /// Starts loading `path` (read through the proactor, decoded off the
        /// game thread) so it is ready before its first `play`. Returns at
        /// once; a repeated path is ignored.
        pub fn preload_path(&mut self, path: &str) -> Result<(), String> {
            if path.is_empty() {
                return Err("SFX path must not be empty".to_string());
            }
            if self.requested.insert(path.to_string()) {
                load_clip_async(path.to_string());
            }
            Ok(())
        }

        pub fn play(&mut self, request: SfxPlayRequest<'_>) -> Result<Option<SfxVoiceId>, String> {
            let started = Instant::now();
            let result = self.play_untimed(request);
            if self.timing.enabled {
                self.timing.record(true, started);
            }
            result
        }

        fn play_untimed(
            &mut self,
            request: SfxPlayRequest<'_>,
        ) -> Result<Option<SfxVoiceId>, String> {
            let request = request.normalized();
            if request.path.is_empty() {
                return Err("SFX path must not be empty".to_string());
            }
            if !self.settings.enabled {
                return Ok(None);
            }
            if (request.playback_rate - 1.0).abs() > 0.001 {
                return Err("Android SFX playback-rate control is not implemented".to_string());
            }

            self.update_untimed();
            let clip = clips()
                .lock()
                .ok()
                .and_then(|clips| clips.get(request.path).cloned());
            let Some(clip) = clip else {
                // Never decode inline: start the load and drop this one.
                self.timing.not_ready += 1;
                self.preload_path(request.path)?;
                return Ok(None);
            };
            if !self.make_voice_capacity(request.priority) {
                return Ok(None);
            }
            let (left, right) =
                stereo_volume(request.volume * self.settings.mix_volume, request.pan);
            let id = self.allocate_voice_id();
            {
                let Ok(mut voices) = voices().lock() else {
                    return Ok(None);
                };
                if voices.list.len() >= MIXER_VOICE_CAPACITY {
                    return Ok(None);
                }
                voices.list.push((
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
            self.voices
                .insert(id, (request.priority, request.volume, request.pan));
            self.order.push_back(id);
            self.idle_since = None;
            if !self.output_running || OUTPUT_LOST.load(Ordering::Acquire) {
                self.output_running = true;
                request_output(true);
            }
            Ok(Some(id))
        }

        pub fn stop(&mut self, voice: SfxVoiceId) {
            if let Ok(mut voices) = voices().lock() {
                voices.remove(voice.value());
            }
            self.voices.remove(&voice);
            self.order.retain(|candidate| *candidate != voice);
        }

        pub fn stop_all(&mut self) {
            if let Ok(mut voices) = voices().lock() {
                for id in self.voices.keys() {
                    voices.remove(id.value());
                }
            }
            self.voices.clear();
            self.order.clear();
        }

        pub fn update(&mut self) {
            let started = Instant::now();
            self.update_untimed();
            if self.timing.enabled {
                self.timing.record(false, started);
            }
        }

        /// Forgets voices the mixer has finished, and stops the output once
        /// nothing has played for `IDLE_STOP`. No allocation.
        fn update_untimed(&mut self) {
            if let Ok(voices) = voices().lock() {
                self.voices.retain(|id, _| voices.contains(id.value()));
                self.order.retain(|id| voices.contains(id.value()));
            }
            if !self.voices.is_empty() {
                self.idle_since = None;
                return;
            }
            let idle_since = *self.idle_since.get_or_insert_with(Instant::now);
            if self.output_running && idle_since.elapsed() >= IDLE_STOP {
                self.output_running = false;
                request_output(false);
            }
        }

        pub fn set_mix_volume(&mut self, volume: f32) {
            self.settings.mix_volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
            if let Ok(mut voices) = voices().lock() {
                for (id, (_, volume, pan)) in &self.voices {
                    let (left, right) = stereo_volume(volume * self.settings.mix_volume, *pan);
                    voices.set_gains(id.value(), left, right);
                }
            }
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
            voices()
                .lock()
                .is_ok_and(|voices| voices.contains(voice.value()))
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            (!self.voices.is_empty() || self.output_running).then_some(Duration::from_millis(100))
        }

        fn make_voice_capacity(&mut self, incoming_priority: u8) -> bool {
            while self.voices.len() >= self.settings.maximum_voices {
                let Some(index) =
                    super::oldest_evictable_voice(&self.order, incoming_priority, |id| {
                        self.voices.get(&id).map(|voice| voice.0)
                    })
                else {
                    return false;
                };
                let Some(oldest) = self.order.remove(index) else {
                    return false;
                };
                self.voices.remove(&oldest);
                if let Ok(mut voices) = voices().lock() {
                    voices.remove(oldest.value());
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
}

/// iOS has no rodio (and so no cpal): playback is loadngo's own RemoteIO
/// backend. Kept in its own file rather than inlined like the others --
/// `audio.rs` is already 2000 lines, and this one owns a decoder thread and
/// a render callback.
#[cfg(target_os = "ios")]
#[path = "audio_ios.rs"]
mod imp;

/// Desktop playback (macOS, Linux, Windows): loadngo's own mixer on the
/// `loadngo-audio-io` output seam. It replaced rodio on 2026-10-13, so every
/// game on every desktop runs one backend.
#[cfg(all(
    not(target_os = "android"),
    not(target_os = "netbsd"),
    not(target_os = "ios")
))]
#[path = "audio_desktop.rs"]
mod imp;

#[cfg(target_os = "netbsd")]
mod imp {
    use super::{SfxPlayRequest, SfxSettings, SfxVoiceId};
    use std::time::Duration;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MusicCueMode {
        OneShot,
        Loop,
    }

    pub struct MusicController {
        active_track: Option<String>,
        playlist_tracks: Vec<String>,
        playlist_index: usize,
        boot_track_path: String,
        cue_mode: MusicCueMode,
        mix_volume: f32,
        bass_boost: f32,
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
            }
        }

        pub fn play_track_path(
            &mut self,
            path: &str,
            _fade: f32,
            _looped: bool,
        ) -> Result<(), String> {
            let selected_path = if path.trim().is_empty() {
                self.boot_track_path.clone()
            } else {
                path.trim().to_string()
            };
            self.active_track = Some(selected_path);
            Ok(())
        }

        pub fn start_playlist(&mut self, fade: f32) -> Result<(), String> {
            self.playlist_index = 0;
            let path = self
                .playlist_tracks
                .get(self.playlist_index)
                .cloned()
                .unwrap_or_else(|| self.boot_track_path.clone());
            self.play_track_path(&path, fade, false)
        }

        pub fn fade_to_path(&mut self, path: &str, fade: f32) -> Result<(), String> {
            let looped = self.playlist_tracks.is_empty() && self.cue_mode == MusicCueMode::Loop;
            self.play_track_path(path, fade, looped)
        }

        /// No-op: this platform has no audio device at all. Exists only for
        /// API parity with the desktop `imp` module.
        pub fn preload_embedded(&mut self, _key: &str, _ogg_bytes: &'static [u8]) {}

        pub fn update(&mut self, _dt: f32) {}

        pub fn pause(&mut self) {}

        pub fn resume(&mut self) {}

        pub fn set_mix_volume(&mut self, volume: f32) {
            self.mix_volume = volume.clamp(0.0, 2.0);
        }

        pub fn set_bass_boost(&mut self, bass_boost: f32) {
            self.bass_boost = bass_boost.clamp(0.0, 1.0);
        }

        pub fn active_track(&self) -> Option<&str> {
            self.active_track.as_deref()
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            None
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

        pub fn play_path(&mut self, _path: &str) -> Result<(), String> {
            Ok(())
        }

        pub fn set_volume(&mut self, volume: f32) {
            self.volume = volume.clamp(0.0, 2.0);
        }

        pub fn set_enabled(&mut self, enabled: bool) -> bool {
            self.enabled = enabled;
            self.enabled
        }

        pub fn is_playing(&self) -> bool {
            false
        }

        pub fn stop(&mut self) {}

        pub fn is_enabled(&self) -> bool {
            self.enabled
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            None
        }
    }

    pub struct SfxController {
        settings: SfxSettings,
    }

    impl SfxController {
        pub fn new(settings: SfxSettings) -> Self {
            Self {
                settings: settings.normalized(),
            }
        }

        /// No-op: this platform has no audio device at all. Exists only for
        /// API parity with the desktop `imp` module.
        pub fn preload_embedded(
            &mut self,
            _key: &str,
            _ogg_bytes: &'static [u8],
        ) -> Result<(), String> {
            Ok(())
        }

        /// Effects here decode on first play or come from `preload_embedded`, so
        /// there is nothing to start early.
        pub fn preload_path(&mut self, path: &str) -> Result<(), String> {
            if path.is_empty() {
                return Err("SFX path must not be empty".to_string());
            }
            Ok(())
        }

        pub fn play(&mut self, request: SfxPlayRequest<'_>) -> Result<Option<SfxVoiceId>, String> {
            if request.normalized().path.is_empty() {
                return Err("SFX path must not be empty".to_string());
            }
            Ok(None)
        }

        pub fn stop(&mut self, _voice: SfxVoiceId) {}

        pub fn stop_all(&mut self) {}

        pub fn update(&mut self) {}

        pub fn set_mix_volume(&mut self, volume: f32) {
            self.settings.mix_volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
        }

        pub fn set_enabled(&mut self, enabled: bool) {
            self.settings.enabled = enabled;
        }

        pub fn is_enabled(&self) -> bool {
            self.settings.enabled
        }

        pub fn active_voice_count(&self) -> usize {
            0
        }

        pub fn is_playing(&self, _voice: SfxVoiceId) -> bool {
            false
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            None
        }
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::{oldest_evictable_voice, SfxPlayRequest, SfxSettings, SfxVoiceId};
    use std::collections::{HashMap, VecDeque};

    #[test]
    fn sfx_settings_enforce_finite_volume_and_nonzero_polyphony() {
        let settings = SfxSettings {
            enabled: true,
            mix_volume: f32::NAN,
            maximum_voices: 0,
        }
        .normalized();

        assert_eq!(settings.mix_volume, 1.0);
        assert_eq!(settings.maximum_voices, 1);
    }

    #[test]
    fn sfx_requests_trim_paths_and_bound_mix_controls() {
        let request = SfxPlayRequest {
            path: "  effect.ogg  ",
            volume: 4.0,
            pan: -4.0,
            playback_rate: f32::INFINITY,
            looped: true,
            priority: 240,
        }
        .normalized();

        assert_eq!(request.path, "effect.ogg");
        assert_eq!(request.volume, 2.0);
        assert_eq!(request.pan, -1.0);
        assert_eq!(request.playback_rate, 1.0);
        assert!(request.looped);
        assert_eq!(request.priority, 240);
    }

    #[test]
    fn voice_pressure_sheds_the_oldest_voice_not_more_important_than_incoming() {
        let order = VecDeque::from([SfxVoiceId(1), SfxVoiceId(2), SfxVoiceId(3)]);
        let priorities = HashMap::from([
            (SfxVoiceId(1), 220),
            (SfxVoiceId(2), 40),
            (SfxVoiceId(3), 80),
        ]);

        assert_eq!(
            oldest_evictable_voice(&order, 100, |id| priorities.get(&id).copied()),
            Some(1)
        );
        assert_eq!(
            oldest_evictable_voice(&order, 20, |id| priorities.get(&id).copied()),
            None
        );
    }
}
