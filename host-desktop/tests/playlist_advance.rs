//! Exercises the one music path a unit test cannot reach: a playlist
//! advancing because a track genuinely ran out.
//!
//! Everything else about `MusicController` can be tested in isolation, but
//! "the decoder hit end-of-stream, the mixer noticed, and `update` moved to
//! the next entry" needs a real device and a track short enough to finish
//! while a test watches. The shipped music is 5-10 minutes long, so the
//! fixtures here are three-second trims -- deliberately longer than the
//! two-second advance debounce, since a shorter one would be blocked by the
//! very guard this is meant to exercise.
//!
//! `#[ignore]`d: it opens the default output device, so it is meaningless on
//! a machine without one. Run it explicitly, under either backend:
//!
//! ```bash
//! cargo test -p loadngo-host-desktop --test playlist_advance -- --ignored --nocapture
//! cargo test -p loadngo-host-desktop --test playlist_advance \
//!     --features native-desktop-audio -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use loadngo_host_desktop::{
    AudioMixer, AudioMixerConfig, AudioPreferences, MusicCueMode, SfxSettings,
};

const BACKEND: &str = if cfg!(feature = "native-desktop-audio") {
    "native"
} else {
    "rodio"
};

/// The fixtures are 3 s; the debounce is 2 s. A change before this could only
/// come from a spurious "finished", which is the bug this guards against.
const EARLIEST_HONEST_ADVANCE: f32 = 2.5;
const RUN_SECONDS: u64 = 9;

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/assets")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

#[test]
#[ignore]
fn a_finished_track_advances_the_playlist() {
    let first = fixture("short-a.ogg");
    let second = fixture("short-b.ogg");
    for path in [&first, &second] {
        assert!(
            std::path::Path::new(path).exists(),
            "missing fixture {path}"
        );
    }

    let mut mixer = AudioMixer::new(
        AudioMixerConfig {
            boot_music_track: first.clone(),
            music_playlist: vec![first.clone(), second.clone()],
            music_cue_mode: MusicCueMode::Loop,
            music_creative_mix: 1.0,
            sfx_settings: SfxSettings::default(),
        },
        AudioPreferences::default(),
    );
    mixer
        .music()
        .start_playlist(0.5)
        .expect("failed to start the playlist");

    let started = Instant::now();
    let mut last = Instant::now();
    // (seconds since start, track) each time the active track changes.
    let mut changes: Vec<(f32, String)> = Vec::new();
    let mut current = mixer.music().active_track().map(str::to_owned);

    while started.elapsed() < Duration::from_secs(RUN_SECONDS) {
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32();
        last = now;
        mixer.music().update(dt);

        let active = mixer.music().active_track().map(str::to_owned);
        if active != current {
            changes.push((started.elapsed().as_secs_f32(), short(active.as_deref())));
            current = active;
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    println!("backend={BACKEND} changes={changes:?}");
    mixer.music().pause();

    let (first_at, first_track) = changes
        .first()
        .expect("the playlist never advanced -- a finished track was never noticed");
    assert!(
        *first_at >= EARLIEST_HONEST_ADVANCE,
        "advanced at {first_at:.2}s, before a 3 s track could have finished -- \
         that is a spurious 'finished', the fault that cycled the playlist"
    );
    assert_eq!(
        first_track, "short-b.ogg",
        "a finished track should hand over to the next playlist entry"
    );
}

fn short(path: Option<&str>) -> String {
    path.and_then(|path| path.rsplit('/').next())
        .unwrap_or("none")
        .to_string()
}
