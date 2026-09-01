# Audio Controllers

`loadngo-host-desktop` exposes separate controllers for long-form music,
single-voice playback, and overlapping sound effects. Applications decide what
a sound means and which asset represents it; `loadngo` owns playback mechanics.

## `AudioMixer`: the recommended entry point

Construct `MusicController`/`SfxController`/`VoiceController` directly only
if you have a reason not to use `AudioMixer`. It composes all three,
fixes a real bug the direct-construction path still has (see "Backend
status" below), and gives games one place to manage master/per-bus volume
and mute instead of hand-computing `master * bus` and pushing it into each
controller separately.

```rust
use loadngo_host_desktop::{
    AudioBus, AudioMixer, AudioMixerConfig, AudioPreferences, MusicCueMode,
    SfxSettings, load_audio_preferences, save_audio_preferences,
};

let prefs_path = std::path::Path::new("/tmp/example/audio_preferences.dat");
let prefs = load_audio_preferences(prefs_path); // AudioPreferences::default() if missing/corrupt

let mut mixer = AudioMixer::new(
    AudioMixerConfig {
        boot_music_track: "bgm/theme.ogg".to_string(),
        music_playlist: Vec::new(),
        music_cue_mode: MusicCueMode::Loop,
        music_creative_mix: 0.35, // the game's own baseline, independent of user prefs
        sfx_settings: SfxSettings::default(),
    },
    prefs,
);

mixer.set_master_volume(0.8);
mixer.set_bus_volume(AudioBus::Sfx, 1.0);
mixer.set_bus_muted(AudioBus::Voice, true);

// Reach the underlying controllers for everything that isn't volume:
mixer.music().fade_to_path("bgm/theme.ogg", 1.5)?;
mixer.sfx().update();

save_audio_preferences(prefs_path, &mixer.preferences())?;
# Ok::<(), String>(())
```

`AudioPreferences` is the persisted half (master volume, one volume per
`AudioBus`, mute state, music bass boost) -- `save_audio_preferences`/
`load_audio_preferences` round-trip it through `loadngo-persistence`
(atomic write, checksummed), typically at a path resolved via
`app_data_dir`. `AudioMixerConfig` is the game's own one-time creative
choices (boot track, playlist, `music_creative_mix`), not a user
preference.

## Sound effects

`SfxController` is intended for shots, impacts, warnings, pickups, UI feedback,
and other short or overlapping sounds. It provides:

- lazy decoded-clip caching on the rodio-backed host path
- overlapping one-shot and looped voices
- bounded global polyphony with priority-aware shedding
- per-play volume, stereo pan, playback rate, and priority
- effects-specific enabled and mix-volume controls
- explicit voice handles, stop, stop-all, and completed-voice cleanup
- silent continuation when the desktop output device is unavailable

The controller accepts paths rather than game-specific cue names. A game-local
adapter should map semantic cues to paths and decide its own per-cue voice
limits, variants, cooldowns, and priorities.

```rust
use loadngo_host_desktop::{SfxController, SfxPlayRequest, SfxSettings};

let mut sfx = SfxController::new(SfxSettings {
    enabled: true,
    mix_volume: 0.8,
    maximum_voices: 24,
});

let voice = sfx.play(SfxPlayRequest {
    path: "assets/audio/sfx/player_fire_01.ogg",
    volume: 0.45,
    pan: -0.2,
    playback_rate: 1.03,
    looped: false,
    priority: 64,
})?;

// Call from the host update path so completed voices are reclaimed.
sfx.update();

if let Some(voice) = voice {
    sfx.stop(voice);
}
# Ok::<(), String>(())
```

`play` returns `Ok(None)` when effects are disabled, the output device is
unavailable, or voice pressure rejects a lower-priority request. It returns an
error for invalid paths and decode/playback failures. Larger `priority` values
are more important; at capacity, an incoming request replaces the oldest voice
whose priority is no greater than its own, or is dropped if every voice is more
important.

Use `SfxPlayRequest::one_shot(path)` when the defaults are sufficient. Pan is
clamped to `-1.0..=1.0`, playback rate to `0.5..=2.0`, and volume/mix volume to
`0.0..=2.0`. Disabling the controller stops its active effects without changing
the music or single-voice controllers.

## Backend status

- The rodio-backed path lazily decodes and caches clips, supports playback rate,
  and runs overlapping spatial sinks.
- **Construct through `AudioMixer`, not the controllers directly, on the
  rodio-backed path (macOS/Linux/Windows/iOS).** `MusicController::new`,
  `SfxController::new`, and `VoiceController::set_enabled(true)` each open
  their own independent `OutputStream` against the OS default output
  device. On a machine whose ALSA/WASAPI/CoreAudio default can't be opened
  concurrently by more than one stream, whichever controller constructs
  first silently wins the device and every later one fails with a "no
  available device"-shaped error, logged and swallowed
  (`"Audio backend unavailable (...), running without music."` etc.) --
  found live via `strace` on a Linux box where SFX played but music never
  did. `AudioMixer::new` opens one shared stream and threads it to all
  three via `MusicController::new_with_handle`/`SfxController::
  new_with_handle`/`VoiceController::enable_with_handle`, avoiding the race
  entirely. The plain constructors are unchanged (each still opens its own
  stream) for any caller with a reason not to use `AudioMixer`.
- Android uses one `MediaPlayer` per active effect, including loops and stereo
  volume. Playback-rate variation currently returns an explicit unsupported
  error, and clips are not decoded into the rodio cache.
- NetBSD currently exposes the same callable contract as a silent controller;
  it validates non-empty paths but returns no voice.

The common request-normalization tests run with the host-desktop library tests.
Backend audio parity still requires runtime checks on the corresponding device;
successful cross-compilation is not listening validation.

