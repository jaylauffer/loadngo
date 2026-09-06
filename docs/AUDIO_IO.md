# Live Audio Input Tooling (`loadngo-audio-io`)

## Status and purpose

Status: **implemented** (2026-09-06), built alongside `sng-bass-blaster`'s
chromatic tuner and live amplifier feature -- the first real caller. This
crate is the `loadngo` counterpart, on the *input* side, to
`loadngo-host-desktop`'s `AudioMixer`/`SfxController`/`MusicController`
(see [`AUDIO.md`](AUDIO.md)), which only ever play decoded assets. Turning
a physical instrument (guitar, bass, mic) into a live signal -- for tuning
or for monitoring it through speakers -- needs raw duplex device I/O
(`cpal`), which nothing in `loadngo` exposed before this.

## What it provides

- `pitch`: platform-agnostic monophonic pitch detection (YIN) and
  equal-temperament note naming (`nearest_note`, `closest_bass_string`,
  `BASS_STANDARD_TUNING`). No device dependency -- always compiles and is
  directly unit-tested with synthesized sine waves, independent of any
  real hardware.
- `list_input_devices` / `list_output_devices`: enumerate the current
  host's devices, flagging the current default, for a settings UI.
- `LiveMonitor`: opens one input stream and one output stream on a
  dedicated worker thread, copies captured audio to playback (gain/mute
  applied), and exposes `drain_tap` so a second consumer (e.g. a tuner's
  `PitchDetector`) can read the same captured signal without opening a
  second input stream.

The last three are gated to `cfg(any(target_os = "macos", target_os =
"linux", target_os = "windows"))` -- desktop only. Mobile live-input
capture (audio session categories, `AVAudioEngine`/`MediaRecorder`) is a
materially different problem that no caller has asked for yet; see
"Non-goals" below rather than a half-built mobile stub.

## Why one shared input stream, not two

`AUDIO.md`'s "Backend status" section documents a real bug this project
already hit once: `MusicController::new`, `SfxController::new`, and
`VoiceController::set_enabled(true)` each used to open their own
independent `OutputStream`, and on a device whose backend can't open more
than one stream concurrently, whichever controller opened first silently
won the device -- found live via `strace` on a Linux box where SFX played
but music never did. `AudioMixer` exists specifically to open one shared
stream and hand it to all three.

A tuner and a live monitor reading the *same* physical input device have
the identical failure mode in reverse (two independent `InputStream`s
racing for one input device), so `LiveMonitor` is built the same way
`AudioMixer` is: exactly one `cpal::Stream` per physical device, with
`drain_tap` as the second consumer's read path instead of a second stream.

## Known limitations (v0.1)

- **AirPlay output devices do not appear in `list_output_devices`, and
  `LiveMonitor` cannot target one.** Confirmed 2026-09-06 while adding a
  device picker to `sng-bass-blaster`: a macOS AirPlay speaker (a
  HomePod-style speaker, visible in System Settings -> Sound -> Output
  with Type "AirPlay") is invisible not just to `cpal` but to
  `system_profiler SPAudioDataType`'s own "Devices" list -- i.e. this is a
  macOS/CoreAudio-level omission, not a gap in `cpal`'s enumeration or
  anything this crate could fix by querying differently. A **Bluetooth**
  device using the classic A2DP profile (headphones, most Bluetooth
  speakers) enumerates as an ordinary CoreAudio device and works
  out-of-the-box; only the newer AirPlay-routed devices (HomePod, and
  anything using "AirPlay 2" instead of A2DP) hit this gap. It was not
  confirmed whether the device becomes a real, enumerable CoreAudio object
  once actively selected as the system's current output in System
  Settings (an experiment to test this, live, was inconclusive -- see
  `sng-bass-blaster/docs/UI_INPUT_FINDINGS.md` for the full investigation
  and exactly what was and wasn't verified). Any caller hitting this
  should route the user to System Settings' own output picker for AirPlay
  targets rather than expecting this crate's device list to include them.
- **No sample-rate conversion.** The output stream is built at the
  input device's sample rate; if the chosen output device can't run at
  that rate, `LiveMonitor::start` returns a `Stream` error rather than
  silently resampling. Pick an input/output pair that share a sample rate
  (the common case for two devices on the same OS default rate).
- **Fixed-size ring buffer, not adaptive.** `MONITOR_RING_CAPACITY`
  (8192 mono samples, ~90-185 ms depending on sample rate) absorbs the two
  devices' independent hardware clocks drifting apart between callbacks.
  It is not a real resampler/clock-lock, so a very long monitoring session
  could in principle drift into an audible under/overrun; not yet observed
  in practice, not yet instrumented either.
- **Mono internally.** Multi-channel input is averaged to mono before
  monitoring or pitch analysis, and the mono signal is duplicated to every
  output channel. A stereo-preserving path is future work if a caller
  needs it.

## Non-goals (for now)

- Mobile (Android/iOS) live-input capture -- no caller needs it yet.
- Real-time pitch-correction/effects processing -- this crate is
  monitoring and *detection*, not a DSP effects chain.
- Adaptive resampling / clock-locked duplex I/O -- see "Known
  limitations" above.
