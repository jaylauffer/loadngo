# Game Audio Runtime

Status: direction set by Jay on 2026-10-05: games use the loadngo proactor for
consistent performance, with thermal awareness, on every host. Android first,
because it is the one host where audio is measured to cost frames today. Each
step below says whether it is built; nothing here is claimed done without the
evidence named beside it.

This document covers what a game's sound effects and music cost the game. It
builds on [AUDIO_BACKENDS.md](AUDIO_BACKENDS.md) (who owns the output device),
[PROACTOR_ENGINE_ADOPTION.md](PROACTOR_ENGINE_ADOPTION.md) (demand-driven
frames, no app-local loops) and [THERMAL_AWARENESS.md](THERMAL_AWARENESS.md)
(the governor and its pressure levels).

## Why: Android, measured

Release `sng-roguelite` 0.5.15 on a Xiaomi 22111317I (Android 14, Adreno 619,
2400x1080 at 60 Hz), played by Jay on 2026-10-05. Evidence, logs and the timing
patch: `~/pudding/reviews/2026-10-05-android-profile-evidence/`.

- The title screen held 60 fps. Combat fell to 18-33 fps for long stretches.
- Drawing was not the cost: almost every frame in those stretches drew and
  presented through GLES in under 8 ms.
- The CPU was not the cost: the game's main thread used 21-24% CPU while slow,
  less than on the title screen. It was waiting.
- Every sound effect created, prepared and started a new
  `android.media.MediaPlayer` on the main thread, and `update` asked every live
  player `isPlaying()`. Each of those is a blocking call into the media server.
  Timed on the device: **40-53 ms per sound effect** (max 105 ms); in heavy
  combat 64-69 sounds per 5 s blocked the main thread for **72-84% of the
  time**. Frame rate tracked it: about 58 fps when sound calls took under 5% of
  the time, about 32 fps when they took 30% or more.

The other hosts avoid the IPC but share the shape of the problem: iOS decodes a
clip synchronously on the game thread the first time it plays, and feeds music
from a dedicated thread that sleeps in 10 ms steps while its queue is full.

## Contract (every host)

1. **The game thread never waits on audio.** `SfxController::play`, `stop`,
   `update` and the music cue calls do bounded, allocation-free work: admit a
   request to a fixed voice table or a bounded command queue and return. No
   system IPC, no file I/O, no decoding on that thread.
2. **Assets are read through the proactor and decoded once, off the game
   thread.** Clip files are read with the host proactor's file operations; the
   decode runs in the completion, not on the game thread. Decoded effects stay
   resident in a cache bounded by bytes. A game declares its effect set up
   front (its cue table already lists every clip), so clips are ready before
   they are first played. A play request for a clip that is not ready yet is
   dropped and counted, never decoded inline.
3. **One owned output stream per process mixes everything** in the platform
   callback (AAudio on Android, RemoteIO on iOS, the `audio-io` seam on
   desktop). Voices are preallocated; the callback never allocates and never
   blocks on a lock the game thread can hold for long.
4. **Music streams with bounded decode-ahead** driven by proactor completions
   and deadlines, not by a thread that sleeps until there is room.
5. **Thermal pressure sets the audio budget.** The host's governor, not the
   game, decides; audio applies it to optional work only and stays correct
   when no provider exists (`Unavailable` is not `Nominal`, but it is not a
   reason to go silent either):

   | Pressure | Effects | Music |
   | --- | --- | --- |
   | Nominal, Fair, Unavailable | full voice budget | normal |
   | Serious | half the voices; low-priority effects dropped | normal |
   | Critical | high- and critical-priority effects only | normal |

   Music is one stream and the cheapest thing that tells the player the game
   is alive, so it is the last thing to go and no step here removes it.
6. **Every claim is measured on the device.** The opt-in `[loadngo-sfx]`
   report (with `LOADNGO_FRAME_METRICS=1`, on Android `adb shell setprop
   debug.loadngo_frame_metrics 1`) gives the time the calling thread spent in
   audio calls per 5 s, alongside the `[loadngo-frame]` pacing report.

## Current state

| Host | Effects | Music | Against the contract |
| --- | --- | --- | --- |
| Android | AAudio mixer since step 2; clips read through the proactor | one `MediaPlayer` per track | effects meet 1-3 and 6 (measured below); music (4) and thermal (5) open |
| iOS | RemoteIO mixer; clips decoded on the offload pool; unit created, started and stopped on the pool, stops after 3 s of silence | decode-ahead jobs on the offload pool, scheduled from `update` | 1-3 and 4 hold (2026-10-13; built, not yet run on a device); no thermal budget (5) |
| Desktop | loadngo mixer on `loadngo-audio-io` (rodio removed 2026-10-13); clips decoded on the offload pool; output opened and closed on the pool, closes after 3 s of silence | decode-ahead jobs on the offload pool, scheduled from `update` | 1-4 hold, measured on the Mac mini (below); no thermal budget (5) |

### Where the work runs (iOS and desktop, 2026-10-13)

On macOS, Linux, Windows and iOS the host proactor dispatches on the main
thread, the same thread as the game, so blocking work handed to it directly
would cost frames. The audio backends therefore put every blocking or CPU
step on loadngo's offload pool (`host-desktop/src/offload.rs`, two workers,
the same pool `offload()` gives apps), as fire-and-forget jobs that leave
their result in the mixer's shared state:

| Work | Where |
| --- | --- |
| Device open, start, stop, close | pool, one switch at a time (`OutputSwitch`) |
| Effect and voice-line decode | pool, once per clip, into a shared cache |
| Music decode | pool, a job whenever a track's queue (~2 s) drops below ~1.4 s (`audio_stream.rs`) |
| Admitting a voice, starting a track, volume | game thread, bounded and allocation-free |
| Mixing | the platform's audio callback; never blocks, never allocates |

Nothing owns a thread or sleeps any more: the per-track decoder threads with
their 10 ms backpressure sleeps are gone, and so is the output thread added
earlier the same day. A game must call the music controller's `update` every
frame while music plays (the decode-ahead is scheduled from it) and follow
`frame_demand`, which asks for a frame every 250 ms while a track plays.
Clips keep their recorded rate and tracks the rate they were decoded at; the
desktop mixer resamples to the device, so the device can open late or reopen
at another rate. An effect played before its clip is decoded is dropped and
counted (`SfxController::not_ready_count`), never decoded inline.

Measured on the Mac mini with a C920 microphone: a 4 s, 44.1 kHz, 1 kHz tone
played as music was continuous at +20 dB over the noise floor for its whole
length, with nothing at 918.75 Hz (where it would land unresampled); a 44.1
kHz effect played on time at +17-25 dB; `play_track_path` returned in 0.003
ms and `play` in 0.002 ms; the playlist test advanced at 3.09 s and 6.11 s on
3 s tracks. Evidence: `~/pudding/reviews/2026-10-13-audio-idle-close/`.

### Closing the output when quiet

An open output costs CPU even when it plays silence: the device calls the
render callback every few milliseconds. `sng-mahjong` 0.1.2 on its title menu,
nothing playing, on the Mac mini (2026-10-13): no audio 0.0% CPU; rodio 0.3%
CPU and four extra threads, for as long as the process lives. So every owned
backend now closes its output after `IDLE_STOP` (3 s) with nothing to play,
and reopens it when something plays:

- **Android** (since step 2): stops the AAudio stream on the proactor thread.
- **iOS and desktop** (2026-10-13): `host-desktop/src/audio_output_idle.rs`.
  The game thread decides (`IdlePolicy`, from each controller's `update`);
  the offload pool does the blocking open/close (`OutputSwitch`), because on
  the Mac mini a reopen took 8-11 ms and a close about 40 ms. Each
  controller's `frame_demand` includes the close deadline, so an idle game is
  woken once, at the deadline, and then idles.
- rodio could not pause or reopen its stream without rebuilding every sink,
  which is one reason it was removed (2026-10-13).

Measured with the native backend on the Mac mini: `play` after a close
returned in 0.013 ms; the reopened tone reached the C920 microphone (1 kHz,
15-18 dB over the noise floor, the same as a tone on the already-open
output); `sng-mahjong` on its title menu went back to 0.0% CPU and zero idle
wakeups. Three CoreAudio threads (`caulk.messenger` x2, `AudioSession -
RootQueue`) stay parked after a close, at no CPU. Evidence:
`~/pudding/reviews/2026-10-13-audio-idle-close/`. iOS is type-checked and
linted only; not yet measured on a device.

A reopen can land on a device running at another rate (headphones plugged in
while closed). Cached effects keep their pitch: each voice records the rate
it was decoded at and the mixer steps through it at `source_rate /
device_rate`. A track started in the few milliseconds before a reopen
completes decodes at the old rate; that window is not handled.

## Steps

Each step ships on its own, keeps every host building, and names its gate.

1. **Measurement in tree.** The Android `[loadngo-sfx]` report that produced
   the numbers above. *Built 2026-10-05.*
2. **Android effects through an owned AAudio mixer.** One AAudio output stream
   (`ndk-sys`'s `audio` feature; `lewton` is already an Android dependency)
   whose data callback mixes preallocated voices; the mixer core shared with
   iOS rather than copied; clips read through the host proactor and decoded in
   the completion; `play` admits a voice and returns. Music stays on its
   `MediaPlayer` for this step. Gate on the Xiaomi: `play` p99 under 0.5 ms in
   the `[loadngo-sfx]` report, combat windows at 58 fps or better, and Jay's
   ear for clicks, latency and missing sounds. *Built 2026-10-05.* Jay
   completed a full run on the Xiaomi (`logcat-play-3-aaudio-mixer.txt` in
   the evidence directory): `play` took 7-80 us on average per 5 s window, at
   most 0.40 ms, and sound calls took 0.02-0.06% of the time (72-84% before);
   no effect was dropped as not ready; AAudio opened at 48 kHz stereo float
   with a 192-frame burst. All 45 five-second windows ran at 58.8 fps or
   better (mean 59.5), p99 frame interval under 20 ms, against 18-33 fps in
   combat before. The one long interval (1.5 s) was during activity start.
   Jay's listening check is still to report. The output stops after 3 s of
   silence and reopens on the next effect; an AAudio error (such as a
   headset unplugged) is recovered on the proactor thread at the next play.
3. **Android music onto the same stream**, decode-ahead driven by proactor
   completions; `MediaPlayer` retired. Gate: no music underruns over a full
   run, and the same pacing.
4. **Android thermal provider and the audio budget.** `AThermal` status
   (API 30+) feeding the shared governor through the host; the table above
   applied to the effect voices. Gate: governor tests with `FakeProvider`, and
   a device run reporting the provider's real state.
5. **iOS to the same contract**: effects decoded on the offload pool before
   first play and music decode-ahead off the sleeping thread are done
   (2026-10-13, not yet run on a device); the thermal budget from the existing
   `NativeProvider` remains.
6. **Desktop to the same contract.** loadngo's mixer is the only desktop
   backend, with effects, music and the device on the offload pool
   (2026-10-13); the thermal budget remains.

Games need one change, once: declare their effect set at startup so step 2's
preload has something to read. `sng-roguelite`'s `sound_spec` table already
lists every clip.
