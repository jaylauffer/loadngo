//! A platform-agnostic mixing/volume layer on top of `MusicController`,
//! `SfxController`, and `VoiceController`.
//!
//! Before this existed, every game hand-rolled its own `master * bus`
//! volume math at the call site (see `sng-rusty`'s Sound Settings menu)
//! and each of the three controllers independently opened its own audio
//! device. On a platform whose default output device can't be opened
//! concurrently (confirmed via `strace` on a Linux box with an ambiguous
//! ALSA default), that meant only whichever controller constructed first
//! actually got sound -- `sng-roguelite`'s SFX (constructed first) played
//! while its music (constructed second, `open_output_stream` fails) went
//! silent with a swallowed "no available device" error.
//!
//! `AudioMixer` fixes the device race (rodio-backed platforms share one
//! `OutputStream`/`OutputStreamHandle` via `MusicController::
//! new_with_handle`/`SfxController::new_with_handle`/
//! `VoiceController::enable_with_handle`) and gives games one place to
//! read/write `AudioBus` volumes plus a master volume, instead of each
//! game recomputing and pushing `set_mix_volume`/`set_volume` by hand.
//!
//! `AudioPreferences` is the persisted half: `save_audio_preferences`/
//! `load_audio_preferences` round-trip it through `loadngo_persistence`
//! (atomic write, checksummed) at whatever path the caller resolves via
//! `app_data_dir`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{MusicController, MusicCueMode, SfxController, SfxSettings, VoiceController};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioBus {
    Music,
    Sfx,
    Voice,
}

/// One-time construction parameters -- the game's own creative choices
/// (which track, how loud sfx sit relative to music by design) as opposed
/// to `AudioPreferences`, which is the user's saved preference layered on
/// top.
pub struct AudioMixerConfig {
    pub boot_music_track: String,
    pub music_playlist: Vec<String>,
    pub music_cue_mode: MusicCueMode,
    /// The music bus's baseline mix level relative to other buses, chosen
    /// by the game (e.g. `sng-roguelite` mixes its music at `0.35` so it
    /// sits behind sound effects). Multiplied into every effective volume
    /// alongside the user's master/bus preference -- not itself a user
    /// preference.
    pub music_creative_mix: f32,
    pub sfx_settings: SfxSettings,
}

/// The user-facing, persisted half of the mix: master volume, one volume
/// per `AudioBus`, mute state, and the one per-bus extra `MusicController`
/// already exposes (`bass_boost`). Round-trip this through
/// `save_audio_preferences`/`load_audio_preferences`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioPreferences {
    pub master_volume: f32,
    pub music_volume: f32,
    pub sfx_volume: f32,
    pub voice_volume: f32,
    /// Music has no controller-level enable/disable (unlike Sfx/Voice), so
    /// muting it is purely a volume-to-zero preference rather than a real
    /// teardown.
    pub music_muted: bool,
    pub sfx_enabled: bool,
    pub voice_enabled: bool,
    pub music_bass_boost: f32,
}

impl Default for AudioPreferences {
    fn default() -> Self {
        Self {
            master_volume: 1.0,
            music_volume: 1.0,
            sfx_volume: 1.0,
            voice_volume: 1.0,
            music_muted: false,
            sfx_enabled: true,
            voice_enabled: true,
            music_bass_boost: 0.0,
        }
    }
}

impl AudioPreferences {
    fn normalized(self) -> Self {
        Self {
            master_volume: crate::audio::finite_clamped(self.master_volume, 0.0, 2.0, 1.0),
            music_volume: crate::audio::finite_clamped(self.music_volume, 0.0, 2.0, 1.0),
            sfx_volume: crate::audio::finite_clamped(self.sfx_volume, 0.0, 2.0, 1.0),
            voice_volume: crate::audio::finite_clamped(self.voice_volume, 0.0, 2.0, 1.0),
            music_muted: self.music_muted,
            sfx_enabled: self.sfx_enabled,
            voice_enabled: self.voice_enabled,
            music_bass_boost: crate::audio::finite_clamped(self.music_bass_boost, 0.0, 1.0, 0.0),
        }
    }
}

const AUDIO_PREFERENCES_SCHEMA_VERSION: u32 = 1;

/// Saves `prefs` atomically (temp file + rename + fsync, via
/// `loadngo_persistence::write_atomic`) to `path`, creating parent
/// directories as needed.
pub fn save_audio_preferences(path: &Path, prefs: &AudioPreferences) -> Result<(), String> {
    let payload = ron::ser::to_string(prefs)
        .map_err(|err| format!("failed to encode audio preferences: {err}"))?;
    loadngo_persistence::write_atomic(path, AUDIO_PREFERENCES_SCHEMA_VERSION, payload.as_bytes())
        .map_err(|err| {
            format!(
                "failed to save audio preferences to {}: {err}",
                path.display()
            )
        })
}

/// Loads preferences saved via `save_audio_preferences`. Never fails to
/// the caller: a missing file, a schema-version mismatch, a checksum
/// failure, or unparseable payload all just log and fall back to
/// `AudioPreferences::default()`, matching how a fresh install should
/// behave anyway.
pub fn load_audio_preferences(path: &Path) -> AudioPreferences {
    let loaded = match loadngo_persistence::read_checked(path) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return AudioPreferences::default(),
        Err(err) => {
            eprintln!(
                "failed to read audio preferences from {} ({err}); using defaults",
                path.display()
            );
            return AudioPreferences::default();
        }
    };

    if loaded.schema_version != AUDIO_PREFERENCES_SCHEMA_VERSION {
        eprintln!(
            "audio preferences at {} have schema version {} (expected {}); using defaults",
            path.display(),
            loaded.schema_version,
            AUDIO_PREFERENCES_SCHEMA_VERSION
        );
        return AudioPreferences::default();
    }

    std::str::from_utf8(&loaded.payload)
        .ok()
        .and_then(|text| ron::de::from_str::<AudioPreferences>(text).ok())
        .map(AudioPreferences::normalized)
        .unwrap_or_else(|| {
            eprintln!(
                "audio preferences at {} could not be parsed; using defaults",
                path.display()
            );
            AudioPreferences::default()
        })
}

pub struct AudioMixer {
    music: MusicController,
    sfx: SfxController,
    voice: VoiceController,
    music_creative_mix: f32,
    prefs: AudioPreferences,
    #[cfg(all(not(target_os = "android"), not(target_os = "netbsd")))]
    _shared_stream: Option<rodio::OutputStream>,
    #[cfg(all(not(target_os = "android"), not(target_os = "netbsd")))]
    shared_handle: Option<rodio::OutputStreamHandle>,
}

#[cfg(all(not(target_os = "android"), not(target_os = "netbsd")))]
impl AudioMixer {
    pub fn new(config: AudioMixerConfig, prefs: AudioPreferences) -> Self {
        let prefs = prefs.normalized();
        let mut mixer = match crate::audio::open_output_stream() {
            Ok((stream, handle)) => {
                let music = MusicController::new_with_handle(
                    &handle,
                    config.boot_music_track,
                    config.music_playlist,
                    config.music_cue_mode,
                    prefs.music_bass_boost,
                );
                let sfx = SfxController::new_with_handle(&handle, config.sfx_settings);
                let mut voice = VoiceController::new(false, 1.0);
                if prefs.voice_enabled {
                    voice.enable_with_handle(&handle);
                }
                Self {
                    music,
                    sfx,
                    voice,
                    music_creative_mix: crate::audio::finite_clamped(
                        config.music_creative_mix,
                        0.0,
                        2.0,
                        1.0,
                    ),
                    prefs,
                    _shared_stream: Some(stream),
                    shared_handle: Some(handle),
                }
            }
            Err(_) => {
                // Each controller independently attempts its own open
                // (matches pre-AudioMixer behavior/log messages) -- this
                // only matters if the backend is unavailable everywhere,
                // in which case every attempt fails the same way anyway.
                let music = MusicController::new(
                    config.boot_music_track,
                    config.music_playlist,
                    config.music_cue_mode,
                    prefs.music_bass_boost,
                );
                let sfx = SfxController::new(config.sfx_settings);
                let voice = VoiceController::new(prefs.voice_enabled, 1.0);
                Self {
                    music,
                    sfx,
                    voice,
                    music_creative_mix: crate::audio::finite_clamped(
                        config.music_creative_mix,
                        0.0,
                        2.0,
                        1.0,
                    ),
                    prefs,
                    _shared_stream: None,
                    shared_handle: None,
                }
            }
        };
        mixer.apply_prefs();
        mixer
    }

    fn set_voice_enabled_internal(&mut self, enabled: bool) {
        if enabled {
            if let Some(handle) = self.shared_handle.clone() {
                self.voice.enable_with_handle(&handle);
                return;
            }
        }
        self.voice.set_enabled(enabled);
    }
}

#[cfg(any(target_os = "android", target_os = "netbsd"))]
impl AudioMixer {
    pub fn new(config: AudioMixerConfig, prefs: AudioPreferences) -> Self {
        let prefs = prefs.normalized();
        let music = MusicController::new(
            config.boot_music_track,
            config.music_playlist,
            config.music_cue_mode,
            prefs.music_bass_boost,
        );
        let sfx = SfxController::new(config.sfx_settings);
        let voice = VoiceController::new(prefs.voice_enabled, 1.0);
        let mut mixer = Self {
            music,
            sfx,
            voice,
            music_creative_mix: crate::audio::finite_clamped(
                config.music_creative_mix,
                0.0,
                2.0,
                1.0,
            ),
            prefs,
        };
        mixer.apply_prefs();
        mixer
    }

    fn set_voice_enabled_internal(&mut self, enabled: bool) {
        self.voice.set_enabled(enabled);
    }
}

impl AudioMixer {
    pub fn music(&mut self) -> &mut MusicController {
        &mut self.music
    }

    pub fn sfx(&mut self) -> &mut SfxController {
        &mut self.sfx
    }

    pub fn voice(&mut self) -> &mut VoiceController {
        &mut self.voice
    }

    pub fn preferences(&self) -> AudioPreferences {
        self.prefs
    }

    pub fn set_master_volume(&mut self, volume: f32) {
        self.prefs.master_volume = crate::audio::finite_clamped(volume, 0.0, 2.0, 1.0);
        self.apply_prefs();
    }

    pub fn master_volume(&self) -> f32 {
        self.prefs.master_volume
    }

    pub fn set_bus_volume(&mut self, bus: AudioBus, volume: f32) {
        let volume = crate::audio::finite_clamped(volume, 0.0, 2.0, 1.0);
        match bus {
            AudioBus::Music => self.prefs.music_volume = volume,
            AudioBus::Sfx => self.prefs.sfx_volume = volume,
            AudioBus::Voice => self.prefs.voice_volume = volume,
        }
        self.apply_prefs();
    }

    pub fn bus_volume(&self, bus: AudioBus) -> f32 {
        match bus {
            AudioBus::Music => self.prefs.music_volume,
            AudioBus::Sfx => self.prefs.sfx_volume,
            AudioBus::Voice => self.prefs.voice_volume,
        }
    }

    /// Mutes without losing the bus's stored slider value. Sfx/Voice route
    /// through the controller's own `set_enabled` (a real teardown on
    /// rodio, matching what `sng-rusty`'s existing "Enable Voiceover"
    /// checkbox already does); Music has no such primitive, so muting it
    /// is purely `effective volume = 0` in `apply_prefs`.
    pub fn set_bus_muted(&mut self, bus: AudioBus, muted: bool) {
        match bus {
            AudioBus::Music => {
                self.prefs.music_muted = muted;
                self.apply_prefs();
            }
            AudioBus::Sfx => {
                self.prefs.sfx_enabled = !muted;
                self.sfx.set_enabled(self.prefs.sfx_enabled);
            }
            AudioBus::Voice => {
                self.prefs.voice_enabled = !muted;
                let enabled = self.prefs.voice_enabled;
                self.set_voice_enabled_internal(enabled);
            }
        }
    }

    pub fn is_bus_muted(&self, bus: AudioBus) -> bool {
        match bus {
            AudioBus::Music => self.prefs.music_muted,
            AudioBus::Sfx => !self.prefs.sfx_enabled,
            AudioBus::Voice => !self.prefs.voice_enabled,
        }
    }

    pub fn set_music_bass_boost(&mut self, bass_boost: f32) {
        self.prefs.music_bass_boost = crate::audio::finite_clamped(bass_boost, 0.0, 1.0, 0.0);
        self.music.set_bass_boost(self.prefs.music_bass_boost);
    }

    pub fn music_bass_boost(&self) -> f32 {
        self.prefs.music_bass_boost
    }

    fn apply_prefs(&mut self) {
        let master = self.prefs.master_volume;

        let music_effective = if self.prefs.music_muted {
            0.0
        } else {
            master * self.prefs.music_volume * self.music_creative_mix
        };
        self.music.set_mix_volume(music_effective);

        self.sfx.set_mix_volume(master * self.prefs.sfx_volume);
        self.voice.set_volume(master * self.prefs.voice_volume);
    }
}
