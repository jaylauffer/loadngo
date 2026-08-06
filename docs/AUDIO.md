# Audio Controllers

`loadngo-host-desktop` exposes separate controllers for long-form music,
single-voice playback, and overlapping sound effects. Applications decide what
a sound means and which asset represents it; `loadngo` owns playback mechanics.

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
- Android uses one `MediaPlayer` per active effect, including loops and stereo
  volume. Playback-rate variation currently returns an explicit unsupported
  error, and clips are not decoded into the rodio cache.
- NetBSD currently exposes the same callable contract as a silent controller;
  it validates non-empty paths but returns no voice.

The common request-normalization tests run with the host-desktop library tests.
Backend audio parity still requires runtime checks on the corresponding device;
successful cross-compilation is not listening validation.

