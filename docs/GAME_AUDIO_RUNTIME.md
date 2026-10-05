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
| iOS | RemoteIO mixer, clips cached | `lewton` on a dedicated thread, 10 ms sleeps for backpressure | 3 holds; first play decodes on the game thread (2); music thread sleeps (4); no thermal budget (5) |
| Desktop | `rodio` by default; native mixer behind `native-desktop-audio` | same | not yet assessed against this contract |

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
5. **iOS to the same contract**: effects decoded through the proactor before
   first play, music decode-ahead off the sleeping thread, the thermal budget
   from the existing `NativeProvider`.
6. **Desktop native path to the same contract**, then the
   `native-desktop-audio` default flip already gated in AUDIO_BACKENDS.md.

Games need one change, once: declare their effect set at startup so step 2's
preload has something to read. `sng-roguelite`'s `sound_spec` table already
lists every clip.
