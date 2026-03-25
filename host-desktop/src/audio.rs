#[cfg(target_os = "android")]
mod imp {
    use crate::android;
    use jni::{
        objects::{GlobalRef, JObject, JValue},
        JNIEnv, JavaVM,
    };
    use ndk_context::android_context;
    use std::time::{Duration, Instant};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MusicCueMode {
        OneShot,
        Loop,
    }

    fn with_env<T>(f: impl FnOnce(&mut JNIEnv) -> Result<T, String>) -> Result<T, String> {
        let ctx = android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|err| format!("Android JavaVM unavailable: {err}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|err| format!("Failed to attach Android audio thread: {err}"))?;
        f(&mut env)
    }

    fn take_java_exception(env: &mut JNIEnv) -> Option<String> {
        match env.exception_check() {
            Ok(true) => {
                let _ = env.exception_describe();
                let message = match env.exception_occurred() {
                    Ok(exception) => {
                        let _ = env.exception_clear();
                        match env.call_method(&exception, "toString", "()Ljava/lang/String;", &[]) {
                            Ok(value) => match value.l() {
                                Ok(obj) => {
                                    let string = jni::objects::JString::from(obj);
                                    env.get_string(&string)
                                        .map(|value| value.to_string_lossy().into_owned())
                                        .unwrap_or_else(|_| {
                                            "Java exception (failed to decode message)".to_string()
                                        })
                                }
                                Err(_) => {
                                    "Java exception (failed to read message object)".to_string()
                                }
                            },
                            Err(_) => "Java exception (failed to stringify throwable)".to_string(),
                        }
                    }
                    Err(_) => {
                        let _ = env.exception_clear();
                        "Java exception (failed to fetch throwable)".to_string()
                    }
                };
                Some(message)
            }
            Ok(false) => None,
            Err(err) => Some(format!("Failed to inspect Java exception state: {err}")),
        }
    }

    fn call_void(
        env: &mut JNIEnv,
        obj: &GlobalRef,
        name: &str,
        sig: &str,
        args: &[JValue],
    ) -> Result<(), String> {
        if let Err(err) = env.call_method(obj.as_obj(), name, sig, args) {
            let detail = take_java_exception(env)
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            return Err(format!("Android MediaPlayer::{name} failed: {err}{detail}"));
        }
        if let Some(detail) = take_java_exception(env) {
            return Err(format!(
                "Android MediaPlayer::{name} raised Java exception: {detail}"
            ));
        }
        Ok(())
    }

    fn call_bool(
        env: &mut JNIEnv,
        obj: &GlobalRef,
        name: &str,
        sig: &str,
        args: &[JValue],
    ) -> Result<bool, String> {
        let value = match env.call_method(obj.as_obj(), name, sig, args) {
            Ok(value) => value,
            Err(err) => {
                let detail = take_java_exception(env)
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default();
                return Err(format!("Android MediaPlayer::{name} failed: {err}{detail}"));
            }
        };
        if let Some(detail) = take_java_exception(env) {
            return Err(format!(
                "Android MediaPlayer::{name} raised Java exception: {detail}"
            ));
        }
        value
            .z()
            .map_err(|err| format!("Android MediaPlayer::{name} return decode failed: {err}"))
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
                    &global,
                    "setAudioStreamType",
                    "(I)V",
                    &[JValue::Int(3)],
                )?;
                call_void(
                    env,
                    &global,
                    "setDataSource",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&path_obj)],
                )?;
                call_void(
                    env,
                    &global,
                    "setLooping",
                    "(Z)V",
                    &[JValue::Bool(u8::from(looped))],
                )?;
                call_void(env, &global, "prepare", "()V", &[])?;
                Ok(Self { player: global })
            })
        }

        fn set_volume(&mut self, volume: f32) -> Result<(), String> {
            let volume = volume.clamp(0.0, 2.0);
            with_env(|env| {
                call_void(
                    env,
                    &self.player,
                    "setVolume",
                    "(FF)V",
                    &[JValue::Float(volume), JValue::Float(volume)],
                )
            })
        }

        fn play(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, &self.player, "start", "()V", &[]))
        }

        fn stop(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, &self.player, "stop", "()V", &[]))
        }

        fn is_playing(&self) -> bool {
            with_env(|env| call_bool(env, &self.player, "isPlaying", "()Z", &[])).unwrap_or(false)
        }

        fn release(&mut self) -> Result<(), String> {
            with_env(|env| call_void(env, &self.player, "release", "()V", &[]))
        }
    }

    impl Drop for MediaPlayerHandle {
        fn drop(&mut self) {
            let _ = self.release();
        }
    }

    pub struct MusicController {
        player: Option<MediaPlayerHandle>,
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
            }
        }

        fn align_playlist_to_track(&mut self, path: &str) {}

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
            self.bass_boost = bass_boost.clamp(0.0, 1.0);
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
                if let Some(player) = self.player.as_mut() {
                    let _ = player.stop();
                    let _ = player.release();
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
            if let Some(player) = self.player.as_mut() {
                if let Err(err) = player.stop() {
                    eprintln!("Android voice stop failed: {err}");
                }
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
}

#[cfg(not(target_os = "android"))]
mod imp {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::BufReader;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use rodio::cpal::traits::HostTrait;
    use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};

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
                    }
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
            if self.tracks.is_empty() || self.stream.is_none() {
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
                if let Err(err) = state.restart(stream_handle, self.bass_boost) {
                    eprintln!("Music bass refresh failed for {}: {err}", state.path);
                    continue;
                }
                if state.target_volume > 0.0 {
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
}

pub use imp::*;
