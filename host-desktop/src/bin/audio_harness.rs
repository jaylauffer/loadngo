//! Drives `AudioMixer` end to end so a playback backend can be *heard* and
//! measured, not merely compiled.
//!
//! Both desktop backends implement the same `imp` contract, so this runs
//! against either and prints which one it was built with:
//!
//! ```bash
//! # rodio (the current default)
//! cargo run -p loadngo-host-desktop --bin audio_harness
//! # loadngo's own CoreAudio/ALSA output seam
//! cargo run -p loadngo-host-desktop --bin audio_harness \
//!     --features native-desktop-audio
//! ```
//!
//! It exercises the parts that only run when something drives `update(dt)`:
//! the per-track fade ramp, a one-off cue interrupting the playlist, and the
//! resume that follows. Those cannot be reached by a unit test and had never
//! executed at all on the native backend.
//!
//! `LOADNGO_DISABLE_AUDIO=1` should produce a clean silent run rather than an
//! error -- that switch is how headless and shared machines run these apps,
//! so it is worth exercising deliberately.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use loadngo_host_desktop::{
    AudioMixer, AudioMixerConfig, AudioPreferences, MusicCueMode, SfxSettings,
};

const BACKEND: &str = if cfg!(feature = "native-desktop-audio") {
    "native (loadngo audio-io output seam)"
} else {
    "rodio"
};

/// Long enough to hear a 2 s fade settle, fire a cue, and watch the playlist
/// resume behind it, without tying up a machine for minutes.
const RUN_SECONDS: u64 = 14;
const CUE_AT_SECONDS: u64 = 5;
const FRAME: Duration = Duration::from_millis(16);

fn asset(name: &str) -> String {
    // The harness lives in loadngo; the tracks live in a sibling game repo.
    // Overridable so this is not wedded to one checkout layout.
    std::env::var("LOADNGO_AUDIO_HARNESS_DIR").map_or_else(
        |_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../sng-zhoenus/assets/audio")
                .join(name)
                .to_string_lossy()
                .into_owned()
        },
        |dir| PathBuf::from(dir).join(name).to_string_lossy().into_owned(),
    )
}

fn main() {
    let first = asset("arraya.ogg");
    let second = asset("flutterrung.ogg");
    let cue = std::env::var("LOADNGO_AUDIO_HARNESS_CUE").unwrap_or_else(|_| second.clone());

    println!("backend: {BACKEND}");
    println!("playlist: {first}");
    println!("          {second}");
    for path in [&first, &second] {
        if !std::path::Path::new(path).exists() {
            eprintln!("missing asset: {path}");
            eprintln!("set LOADNGO_AUDIO_HARNESS_DIR to a directory holding the .ogg tracks");
            std::process::exit(2);
        }
    }

    let mut mixer = AudioMixer::new(
        AudioMixerConfig {
            boot_music_track: first.clone(),
            music_playlist: vec![first.clone(), second.clone()],
            music_cue_mode: MusicCueMode::Loop,
            // The games use 0.35; full volume here so a fault is obvious.
            music_creative_mix: 1.0,
            sfx_settings: SfxSettings::default(),
        },
        AudioPreferences::default(),
    );

    if let Err(error) = mixer.music().start_playlist(2.0) {
        eprintln!("start_playlist failed: {error}");
        std::process::exit(1);
    }

    let started = Instant::now();
    let mut last = Instant::now();
    let mut cued = false;
    let mut reported = 0u64;

    while started.elapsed() < Duration::from_secs(RUN_SECONDS) {
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32();
        last = now;
        mixer.music().update(dt);

        let elapsed = started.elapsed().as_secs();
        if !cued && elapsed >= CUE_AT_SECONDS {
            cued = true;
            println!("[{elapsed:>2}s] cue -> {cue}");
            if let Err(error) = mixer.music().fade_to_path(&cue, 1.5) {
                eprintln!("fade_to_path failed: {error}");
            }
        }
        if elapsed > reported {
            reported = elapsed;
            // One borrow: `music()` hands out `&mut`, so reaching through it
            // twice in one expression is two overlapping mutable borrows.
            let music = mixer.music();
            let (track, demand) = (
                music.active_track().map(str::to_owned),
                music.frame_demand(),
            );
            println!("[{elapsed:>2}s] track={track:?} frame_demand={demand:?}");
        }
        std::thread::sleep(FRAME);
    }

    println!("stopping");
    mixer.music().pause();
}
