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

fn finite_clamped(value: f32, minimum: f32, maximum: f32, fallback: f32) -> f32 {
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

    struct SfxVoice {
        player: MediaPlayerHandle,
        volume: f32,
        pan: f32,
        priority: u8,
    }

    pub struct SfxController {
        settings: SfxSettings,
        voices: HashMap<SfxVoiceId, SfxVoice>,
        order: VecDeque<SfxVoiceId>,
        next_voice_id: u64,
    }

    impl SfxController {
        pub fn new(settings: SfxSettings) -> Self {
            Self {
                settings: settings.normalized(),
                voices: HashMap::new(),
                order: VecDeque::new(),
                next_voice_id: 1,
            }
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
                return Err("Android SFX playback-rate control is not implemented".to_string());
            }

            self.update();
            if !self.make_voice_capacity(request.priority) {
                return Ok(None);
            }
            let path = android::ensure_materialized_asset_path(request.path)?;
            let mut player = MediaPlayerHandle::create(&path, request.looped)?;
            let (left, right) =
                stereo_volume(request.volume * self.settings.mix_volume, request.pan);
            player.set_stereo_volume(left, right)?;
            player.play()?;

            let id = self.allocate_voice_id();
            self.voices.insert(
                id,
                SfxVoice {
                    player,
                    volume: request.volume,
                    pan: request.pan,
                    priority: request.priority,
                },
            );
            self.order.push_back(id);
            Ok(Some(id))
        }

        pub fn stop(&mut self, voice: SfxVoiceId) {
            if let Some(mut state) = self.voices.remove(&voice) {
                let _ = state.player.stop();
            }
            self.order.retain(|candidate| *candidate != voice);
        }

        pub fn stop_all(&mut self) {
            for state in self.voices.values_mut() {
                let _ = state.player.stop();
            }
            self.voices.clear();
            self.order.clear();
        }

        pub fn update(&mut self) {
            let finished = self
                .voices
                .iter()
                .filter_map(|(id, state)| (!state.player.is_playing()).then_some(*id))
                .collect::<Vec<_>>();
            for id in finished {
                self.voices.remove(&id);
                self.order.retain(|candidate| *candidate != id);
            }
        }

        pub fn set_mix_volume(&mut self, volume: f32) {
            self.settings.mix_volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
            for state in self.voices.values_mut() {
                let (left, right) =
                    stereo_volume(state.volume * self.settings.mix_volume, state.pan);
                let _ = state.player.set_stereo_volume(left, right);
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
            self.voices
                .get(&voice)
                .is_some_and(|state| state.player.is_playing())
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            (!self.voices.is_empty()).then_some(Duration::from_millis(100))
        }

        fn make_voice_capacity(&mut self, incoming_priority: u8) -> bool {
            while self.voices.len() >= self.settings.maximum_voices {
                let Some(index) =
                    super::oldest_evictable_voice(&self.order, incoming_priority, |id| {
                        self.voices.get(&id).map(|voice| voice.priority)
                    })
                else {
                    return false;
                };
                let Some(oldest) = self.order.remove(index) else {
                    return false;
                };
                if let Some(mut state) = self.voices.remove(&oldest) {
                    let _ = state.player.stop();
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

    fn stereo_volume(volume: f32, pan: f32) -> (f32, f32) {
        let pan = pan.clamp(-1.0, 1.0);
        (volume * (1.0 - pan.max(0.0)), volume * (1.0 + pan.min(0.0)))
    }
}

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

#[cfg(all(not(target_os = "android"), not(target_os = "netbsd")))]
mod imp {
    use super::{SfxPlayRequest, SfxSettings, SfxVoiceId};
    use std::collections::{HashMap, VecDeque};
    use std::fs::File;
    use std::io::BufReader;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use rodio::cpal::traits::HostTrait;
    use rodio::{
        buffer::SamplesBuffer, Decoder, OutputStream, OutputStreamHandle, Sink, Source, SpatialSink,
    };

    const MUSIC_BASE_VOLUME: f32 = 0.8;
    const MUSIC_BASS_CUTOFF_HZ: u32 = 180;
    const MUSIC_BASS_POST_GAIN: f32 = 0.9;
    const MUSIC_IDLE_POLL_INTERVAL: Duration = Duration::from_millis(1000);
    static AUDIO_BACKEND_FAILURE: OnceLock<String> = OnceLock::new();

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MusicCueMode {
        OneShot,
        Loop,
    }

    struct TrackState {
        sink: Sink,
        path: String,
        looped: bool,
        current_volume: f32,
        target_volume: f32,
        playing: bool,
    }

    fn append_music_source(
        sink: &Sink,
        file: File,
        looped: bool,
        bass_boost: f32,
    ) -> Result<(), String> {
        let source = Decoder::new(BufReader::new(file))
            .map_err(|err| format!("Failed to decode music track: {err}"))?
            .convert_samples::<f32>()
            .buffered();

        let enhanced = source
            .clone()
            .mix(
                source
                    .clone()
                    .low_pass(MUSIC_BASS_CUTOFF_HZ)
                    .amplify(bass_boost),
            )
            .amplify(MUSIC_BASS_POST_GAIN);

        if looped {
            sink.append(enhanced.repeat_infinite());
        } else {
            sink.append(enhanced);
        }

        Ok(())
    }

    fn open_output_stream() -> Result<(OutputStream, OutputStreamHandle), String> {
        if let Some(err) = AUDIO_BACKEND_FAILURE.get() {
            return Err(err.clone());
        }
        if matches!(
            std::env::var("LOADNGO_DISABLE_AUDIO").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        ) {
            let err = "Audio disabled by LOADNGO_DISABLE_AUDIO".to_string();
            let _ = AUDIO_BACKEND_FAILURE.set(err.clone());
            return Err(err);
        }

        let host = rodio::cpal::default_host();
        let Some(device) = host.default_output_device() else {
            let err = "No default output device".to_string();
            let _ = AUDIO_BACKEND_FAILURE.set(err.clone());
            return Err(err);
        };

        match OutputStream::try_from_device(&device) {
            Ok(stream) => Ok(stream),
            Err(err) => {
                let detail = format!("Default output device stream failed: {err}");
                let _ = AUDIO_BACKEND_FAILURE.set(detail.clone());
                Err(detail)
            }
        }
    }

    impl TrackState {
        fn new(
            handle: &OutputStreamHandle,
            path: &str,
            looped: bool,
            bass_boost: f32,
        ) -> Result<Self, String> {
            let file = File::open(path).map_err(|err| format!("Missing audio {path}: {err}"))?;
            let sink = Sink::try_new(handle)
                .map_err(|err| format!("Failed to create audio sink: {err}"))?;
            append_music_source(&sink, file, looped, bass_boost)
                .map_err(|err| format!("Failed to prepare {path}: {err}"))?;
            sink.set_volume(0.0);
            sink.pause();

            Ok(Self {
                sink,
                path: path.to_string(),
                looped,
                current_volume: 0.0,
                target_volume: 0.0,
                playing: false,
            })
        }

        fn ensure_playing(&mut self) {
            if !self.playing {
                self.sink.play();
                self.playing = true;
            }
        }

        fn is_finished(&self) -> bool {
            self.playing && self.sink.empty()
        }

        fn restart(&mut self, handle: &OutputStreamHandle, bass_boost: f32) -> Result<(), String> {
            let file = File::open(&self.path)
                .map_err(|err| format!("Missing audio {}: {err}", self.path))?;
            self.sink.stop();
            let sink = Sink::try_new(handle)
                .map_err(|err| format!("Failed to create audio sink: {err}"))?;
            append_music_source(&sink, file, self.looped, bass_boost)
                .map_err(|err| format!("Failed to prepare {}: {err}", self.path))?;
            sink.set_volume(0.0);
            self.sink = sink;
            self.current_volume = 0.0;
            self.target_volume = 0.0;
            self.playing = false;
            Ok(())
        }
    }

    pub struct MusicController {
        tracks: HashMap<String, TrackState>,
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
        stream: Option<OutputStream>,
        stream_handle: Option<OutputStreamHandle>,
        music_paused: bool,
    }

    impl MusicController {
        pub fn new(
            boot_track_path: String,
            playlist_tracks: Vec<String>,
            cue_mode: MusicCueMode,
            bass_boost: f32,
        ) -> Self {
            match open_output_stream() {
                Ok((stream, stream_handle)) => Self {
                    tracks: HashMap::new(),
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
                    stream: Some(stream),
                    stream_handle: Some(stream_handle),
                    music_paused: false,
                },
                Err(err) => {
                    eprintln!("Audio backend unavailable ({err}), running without music.");
                    Self {
                        tracks: HashMap::new(),
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
                        stream: None,
                        stream_handle: None,
                        music_paused: false,
                    }
                }
            }
        }

        /// Pauses every currently-playing track's sink in place (resumable
        /// from the same position) and suspends `update`'s fade/finished
        /// bookkeeping, matching the Android impl's `pause`/`resume`
        /// contract.
        pub fn pause(&mut self) {
            self.music_paused = true;
            for state in self.tracks.values_mut() {
                if state.playing {
                    state.sink.pause();
                }
            }
        }

        pub fn resume(&mut self) {
            self.music_paused = false;
            for state in self.tracks.values_mut() {
                if state.playing {
                    state.sink.play();
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
            fade: f32,
            looped: bool,
        ) -> Result<(), String> {
            if self.stream.is_none() {
                return Err("Audio backend unavailable".to_string());
            }

            let selected_path = if path.trim().is_empty() {
                self.boot_track_path.clone()
            } else {
                path.trim().to_string()
            };
            if self.active_track.as_deref() == Some(selected_path.as_str())
                && self
                    .tracks
                    .get(&selected_path)
                    .is_some_and(|state| !state.is_finished())
            {
                self.fade_duration = fade.max(0.05);
                return Ok(());
            }

            self.fade_duration = fade.max(0.05);
            let Some(stream_handle) = self.stream_handle.as_ref() else {
                return Err("Audio output handle unavailable".to_string());
            };

            if !self.tracks.contains_key(&selected_path) {
                let state =
                    TrackState::new(stream_handle, &selected_path, looped, self.bass_boost)?;
                self.tracks.insert(selected_path.clone(), state);
            } else if self
                .tracks
                .get(&selected_path)
                .is_some_and(|state| state.is_finished() || state.looped != looped)
            {
                if let Some(state) = self.tracks.get_mut(&selected_path) {
                    state.looped = looped;
                    state.restart(stream_handle, self.bass_boost)?;
                }
            }

            let Some(state) = self.tracks.get_mut(&selected_path) else {
                return Err(format!("Song {selected_path} could not be prepared"));
            };
            state.ensure_playing();
            state.target_volume = 1.0;

            for (name, other) in self.tracks.iter_mut() {
                if name != &selected_path {
                    other.target_volume = 0.0;
                }
            }

            self.active_track = Some(selected_path.clone());
            self.track_started_at = Some(Instant::now());
            Ok(())
        }

        fn play_playlist_current(&mut self, fade: f32) -> Result<(), String> {
            if self.playlist_tracks.is_empty() {
                return self.play_track_path(&self.boot_track_path.clone(), fade, false);
            }
            let track = self.playlist_tracks[self.playlist_index].clone();
            self.play_track_path(&track, fade, false)
        }

        fn play_next_playlist(&mut self, fade: f32) -> Result<(), String> {
            if self.playlist_tracks.is_empty() {
                return self.play_track_path(&self.boot_track_path.clone(), fade, false);
            }
            let mut last_err: Option<String> = None;
            let len = self.playlist_tracks.len();
            for _ in 0..len {
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
            self.play_playlist_current(fade)
        }

        pub fn fade_to_path(&mut self, path: &str, fade: f32) -> Result<(), String> {
            self.resume_playlist_after_cue = !self.playlist_tracks.is_empty();
            self.resume_playlist_from_next_track = self.playlist_mode_active;
            self.playlist_mode_active = false;
            let looped = self.playlist_tracks.is_empty() && self.cue_mode == MusicCueMode::Loop;
            self.play_track_path(path, fade, looped)
        }

        pub fn update(&mut self, dt: f32) {
            if self.music_paused || self.tracks.is_empty() || self.stream.is_none() {
                return;
            }
            let fade = self.fade_duration.max(0.001);
            for state in self.tracks.values_mut() {
                if !state.playing && state.target_volume <= 0.0 {
                    continue;
                }
                let diff = state.target_volume - state.current_volume;
                if diff.abs() <= 0.001 {
                    state.current_volume = state.target_volume;
                } else {
                    let step = (dt / fade).min(diff.abs());
                    state.current_volume += step * diff.signum();
                }
                state.current_volume = state.current_volume.clamp(0.0, 1.0);
                if state.playing {
                    state
                        .sink
                        .set_volume(state.current_volume * MUSIC_BASE_VOLUME * self.mix_volume);
                }
                if state.playing && state.current_volume == 0.0 && state.target_volume == 0.0 {
                    state.sink.pause();
                    state.playing = false;
                }
            }

            if let Some(active_name) = self.active_track.clone() {
                let finished = self
                    .tracks
                    .get(&active_name)
                    .is_some_and(TrackState::is_finished);
                if finished {
                    if !self.playlist_mode_active {
                        if self.resume_playlist_after_cue {
                            self.resume_playlist_after_cue = false;
                            self.playlist_mode_active = true;
                            let resume_result = if self.resume_playlist_from_next_track {
                                self.play_next_playlist(self.fade_duration.max(0.1))
                            } else {
                                self.play_playlist_current(self.fade_duration.max(0.1))
                            };
                            self.resume_playlist_from_next_track = false;
                            if let Err(err) = resume_result {
                                eprintln!(
                                    "Playlist resume failed after direct cue {active_name}: {err}"
                                );
                            }
                        }
                        return;
                    }
                    if self
                        .track_started_at
                        .is_some_and(|started| started.elapsed() < Duration::from_secs(2))
                    {
                        return;
                    }
                    if let Err(err) = self.play_next_playlist(self.fade_duration.max(0.1)) {
                        eprintln!("Playlist advance failed after {active_name}: {err}");
                    }
                }
            }
        }

        pub fn set_mix_volume(&mut self, volume: f32) {
            self.mix_volume = volume.clamp(0.0, 2.0);
        }

        pub fn set_bass_boost(&mut self, bass_boost: f32) {
            let bass_boost = bass_boost.clamp(0.0, 1.0);
            if (self.bass_boost - bass_boost).abs() <= 0.001 {
                return;
            }
            self.bass_boost = bass_boost;

            let Some(stream_handle) = self.stream_handle.as_ref() else {
                return;
            };

            for state in self.tracks.values_mut() {
                let resume_volume = state.current_volume;
                let resume_target = state.target_volume;
                let was_playing = state.playing;
                if let Err(err) = state.restart(stream_handle, self.bass_boost) {
                    eprintln!("Music bass refresh failed for {}: {err}", state.path);
                    continue;
                }
                state.current_volume = resume_volume;
                state.target_volume = resume_target;
                if was_playing || resume_volume > 0.0 || resume_target > 0.0 {
                    state.ensure_playing();
                }
                state
                    .sink
                    .set_volume(state.current_volume * MUSIC_BASE_VOLUME * self.mix_volume);
            }
        }

        pub fn active_track(&self) -> Option<&str> {
            self.active_track.as_deref()
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            let fading = self
                .tracks
                .values()
                .any(|state| (state.current_volume - state.target_volume).abs() > 0.001);
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
        stream: Option<OutputStream>,
        stream_handle: Option<OutputStreamHandle>,
        sink: Option<Sink>,
    }

    impl VoiceController {
        pub fn new(enabled: bool, volume: f32) -> Self {
            let mut controller = Self {
                enabled: false,
                volume,
                stream: None,
                stream_handle: None,
                sink: None,
            };
            controller.set_enabled(enabled);
            controller
        }

        pub fn play_path(&mut self, path: &str) -> Result<(), String> {
            if !self.enabled || self.stream.is_none() {
                return Ok(());
            }
            if let Some(sink) = self.sink.take() {
                sink.stop();
            }

            let file =
                File::open(path).map_err(|err| format!("Missing voice clip {path}: {err}"))?;
            let source = Decoder::new(BufReader::new(file))
                .map_err(|err| format!("Failed to decode voice clip {path}: {err}"))?;
            let Some(stream_handle) = self.stream_handle.as_ref() else {
                return Err("Voice output handle unavailable".to_string());
            };
            let sink = Sink::try_new(stream_handle)
                .map_err(|err| format!("Failed to create voice sink: {err}"))?;
            sink.set_volume(self.volume);
            sink.append(source);
            self.sink = Some(sink);
            Ok(())
        }

        pub fn set_volume(&mut self, volume: f32) {
            self.volume = volume.clamp(0.0, 2.0);
            if let Some(sink) = self.sink.as_ref() {
                sink.set_volume(self.volume);
            }
        }

        pub fn set_enabled(&mut self, enabled: bool) -> bool {
            if !enabled {
                if let Some(sink) = self.sink.take() {
                    sink.stop();
                }
                self.stream_handle = None;
                self.stream = None;
                self.enabled = false;
                return self.enabled;
            }

            if self.enabled && self.stream_handle.is_some() {
                return true;
            }

            match open_output_stream() {
                Ok((stream, stream_handle)) => {
                    self.stream = Some(stream);
                    self.stream_handle = Some(stream_handle);
                    self.enabled = true;
                    true
                }
                Err(err) => {
                    eprintln!("Voice backend unavailable ({err}), running without voiceover.");
                    self.enabled = false;
                    self.stream = None;
                    self.stream_handle = None;
                    false
                }
            }
        }

        pub fn is_playing(&self) -> bool {
            self.enabled && self.sink.as_ref().is_some_and(|sink| !sink.empty())
        }

        pub fn stop(&mut self) {
            if let Some(sink) = self.sink.take() {
                sink.stop();
            }
        }

        pub fn is_enabled(&self) -> bool {
            self.enabled
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            if self.enabled && self.sink.as_ref().is_some_and(|sink| !sink.empty()) {
                return Some(Duration::from_millis(100));
            }
            None
        }
    }

    #[derive(Clone)]
    struct CachedSfxClip {
        channels: u16,
        sample_rate: u32,
        samples: Vec<f32>,
    }

    struct SfxVoice {
        sink: SpatialSink,
        volume: f32,
        priority: u8,
    }

    pub struct SfxController {
        settings: SfxSettings,
        clips: HashMap<String, CachedSfxClip>,
        voices: HashMap<SfxVoiceId, SfxVoice>,
        order: VecDeque<SfxVoiceId>,
        next_voice_id: u64,
        _stream: Option<OutputStream>,
        stream_handle: Option<OutputStreamHandle>,
    }

    impl SfxController {
        pub fn new(settings: SfxSettings) -> Self {
            let settings = settings.normalized();
            match open_output_stream() {
                Ok((stream, stream_handle)) => Self {
                    settings,
                    clips: HashMap::new(),
                    voices: HashMap::new(),
                    order: VecDeque::new(),
                    next_voice_id: 1,
                    _stream: Some(stream),
                    stream_handle: Some(stream_handle),
                },
                Err(err) => {
                    eprintln!("Audio backend unavailable ({err}), running without sound effects.");
                    Self {
                        settings,
                        clips: HashMap::new(),
                        voices: HashMap::new(),
                        order: VecDeque::new(),
                        next_voice_id: 1,
                        _stream: None,
                        stream_handle: None,
                    }
                }
            }
        }

        pub fn play(&mut self, request: SfxPlayRequest<'_>) -> Result<Option<SfxVoiceId>, String> {
            let request = request.normalized();
            if request.path.is_empty() {
                return Err("SFX path must not be empty".to_string());
            }
            if !self.settings.enabled || self.stream_handle.is_none() {
                return Ok(None);
            }

            self.update();
            if !self.make_voice_capacity(request.priority) {
                return Ok(None);
            }
            let clip = self.load_clip(request.path)?.clone();
            let Some(stream_handle) = self.stream_handle.as_ref() else {
                return Ok(None);
            };
            let sink = SpatialSink::try_new(
                stream_handle,
                [request.pan, 0.0, 1.0],
                [-1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
            )
            .map_err(|err| format!("Failed to create SFX voice for {}: {err}", request.path))?;
            sink.set_volume(request.volume * self.settings.mix_volume);
            sink.set_speed(request.playback_rate);
            let source = SamplesBuffer::new(clip.channels, clip.sample_rate, clip.samples);
            if request.looped {
                sink.append(source.repeat_infinite());
            } else {
                sink.append(source);
            }

            let id = self.allocate_voice_id();
            self.voices.insert(
                id,
                SfxVoice {
                    sink,
                    volume: request.volume,
                    priority: request.priority,
                },
            );
            self.order.push_back(id);
            Ok(Some(id))
        }

        pub fn stop(&mut self, voice: SfxVoiceId) {
            if let Some(state) = self.voices.remove(&voice) {
                state.sink.stop();
            }
            self.order.retain(|candidate| *candidate != voice);
        }

        pub fn stop_all(&mut self) {
            for state in self.voices.values() {
                state.sink.stop();
            }
            self.voices.clear();
            self.order.clear();
        }

        pub fn update(&mut self) {
            let finished = self
                .voices
                .iter()
                .filter_map(|(id, state)| state.sink.empty().then_some(*id))
                .collect::<Vec<_>>();
            for id in finished {
                self.voices.remove(&id);
                self.order.retain(|candidate| *candidate != id);
            }
        }

        pub fn set_mix_volume(&mut self, volume: f32) {
            self.settings.mix_volume = super::finite_clamped(volume, 0.0, 2.0, 1.0);
            for state in self.voices.values() {
                state
                    .sink
                    .set_volume(state.volume * self.settings.mix_volume);
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
            self.voices
                .get(&voice)
                .is_some_and(|state| !state.sink.empty())
        }

        pub fn cached_clip_count(&self) -> usize {
            self.clips.len()
        }

        pub fn frame_demand(&self) -> Option<Duration> {
            (!self.voices.is_empty()).then_some(Duration::from_millis(100))
        }

        fn load_clip(&mut self, path: &str) -> Result<&CachedSfxClip, String> {
            if !self.clips.contains_key(path) {
                let file = File::open(path).map_err(|err| format!("Missing SFX {path}: {err}"))?;
                let decoder = Decoder::new(BufReader::new(file))
                    .map_err(|err| format!("Failed to decode SFX {path}: {err}"))?;
                let channels = decoder.channels();
                let sample_rate = decoder.sample_rate();
                let samples = decoder.convert_samples::<f32>().collect::<Vec<_>>();
                if samples.is_empty() {
                    return Err(format!("SFX {path} contains no audio samples"));
                }
                self.clips.insert(
                    path.to_string(),
                    CachedSfxClip {
                        channels,
                        sample_rate,
                        samples,
                    },
                );
            }
            self.clips
                .get(path)
                .ok_or_else(|| format!("SFX cache lost {path}"))
        }

        fn make_voice_capacity(&mut self, incoming_priority: u8) -> bool {
            while self.voices.len() >= self.settings.maximum_voices {
                let Some(index) =
                    super::oldest_evictable_voice(&self.order, incoming_priority, |id| {
                        self.voices.get(&id).map(|voice| voice.priority)
                    })
                else {
                    return false;
                };
                let Some(oldest) = self.order.remove(index) else {
                    return false;
                };
                if let Some(state) = self.voices.remove(&oldest) {
                    state.sink.stop();
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
