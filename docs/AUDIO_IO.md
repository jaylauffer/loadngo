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
  second input stream. The tap is a second lock-free SPSC ring, not a
  shared locked buffer: the input callback only pushes, and `drain_tap`
  does the "keep the most recent `tap_capacity` samples" discarding on the
  consumer side. It was a `Mutex<VecDeque<f32>>` until 2026-09-09, which
  meant the audio callback took a lock and could allocate -- a real-time
  violation, and one that stalls capture (and so drains the monitoring
  ring) exactly when the system is already under load.

- `probe_input_capabilities` / `set_input_physical_format`: what a
  converter can really capture, and switching it to a better physical
  format. See "Converter capabilities" below.
- `LiveMonitor::take_recording_tap`: a lossless, full-channel-count
  `RecordingTap` off the same input stream, for recorders. See "Recording
  tap" below.

Device I/O goes through a per-platform backend (`src/backend`): CoreAudio's
HAL directly on macOS, `cpal` on Linux and Windows until their own backends
land -- see [AUDIO_BACKENDS.md](AUDIO_BACKENDS.md). Everything except `pitch`
is gated to `cfg(any(target_os = "macos", target_os =
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

## Converter capabilities (added 2026-09-11)

Built for `sng-bass-blaster`'s recorder (a port of the old Windows
recording tool Frauu), whose requirement was to record at whatever
resolution the converter really delivers and let the user explore what
the converter supports.

There are two different formats hiding behind "what format is this device",
and `cpal` only reports one of them:

| | what it is | macOS | Linux (ALSA) | Windows (WASAPI shared) |
| --- | --- | --- | --- | --- |
| stream format | what the OS hands the app | **always 32-bit float** (HAL virtual format) | the hardware format for a `hw:` device | the mixer format |
| physical format | what the converter runs at | `kAudioStreamPropertyPhysicalFormat` | same as stream | not queried yet |

`cpal`'s CoreAudio backend hardcodes `SampleFormat::F32` for every supported
config, so on macOS a 24-bit interface and a 16-bit webcam look identical
through `cpal`. `physical_macos.rs` reads the real formats straight from
CoreAudio (via `coreaudio-sys`, which `cpal` already builds -- no new crate).
`InputCapabilities::capture_resolution()` resolves the two into the one
answer a recorder needs, and says which source it came from.

Found on the first real probe of this Mac mini's devices:

- **"KT USB Audio" advertises 24-bit and 16-bit physical formats but was
  running at 16-bit.** `set_input_physical_format` switches it -- the same
  system-wide, persistent setting Audio MIDI Setup's "Format" menu changes.
  Verified switching 16 -> 24 -> 16 on the real device
  (`switches_a_converter_physical_format_and_restores_it`, opt-in via
  `LOADNGO_PHYSICAL_FORMAT_DEVICE`). The app-facing stream stays 32-bit
  float either way, so a running monitor is unaffected by a resolution-only
  change; a *rate* change needs the monitor restarted. **The change applies
  asynchronously:** a probe immediately after `set_input_physical_format`
  returns still reports the old format (it settled within a second on KT USB
  Audio), so callers should confirm by re-probing over the next few hundred
  milliseconds rather than reading back once.
- **Verified end to end 2026-09-11** from `sng-bass-blaster`: KT USB Audio
  switched to 24-bit, 2 s captured through the recording tap and written by
  its storage proactor. Apple's `afinfo` reads the file as 48 kHz 24-bit
  signed LE, and 95,678 of 96,256 samples have a non-zero low byte -- real
  24-bit content, not 16-bit padded out.
- **One USB interface can be two CoreAudio devices with the same name.**
  "USB PnP Audio Device" enumerates an output-only object first and the
  input object second, so a first-name-match lookup found no input streams
  and reported no physical format. Lookups now skip same-named devices with
  no input streams.

## Recording tap (added 2026-09-11)

`drain_tap` is the wrong source for a recording: it is mono, and it drops
old samples on purpose so a tuner always sees the latest audio. The input
callback now also writes a third lock-free SPSC ring carrying interleaved
frames at the device's full channel count. Its rules:

- **Idle until armed.** The callback checks one atomic and writes nothing
  unless a recorder has called `RecordingTap::arm`, so a monitor with no
  recorder doesn't spend its life counting overruns.
- **Drops whole frames, counts them.** On a full ring the callback writes
  the frames that fit and adds the rest to `dropped_samples`. Dropping a
  partial frame would swap channels for the rest of the take.
- **Settle before the final drain.** `disarm_and_settle` waits until two
  more callback blocks complete, so a block that read `armed == true` just
  before disarm has finished writing before the recorder drains the tail.
- **Capacity is seconds, not samples** (`recording_buffer_seconds`, default
  4s) -- the longest storage stall a recording survives without a gap.
- **`f32` is exact for integer samples of 24 bits or fewer**, when scaled by
  a power of two. The `i16` input path now divides by `32_768` rather than
  `i16::MAX` so every platform maps integers to floats with the same rule
  CoreAudio uses, and a WAV writer can map them back exactly. A 32-bit
  integer converter loses its bottom 8 bits through `f32`; store those as
  float.

The tap is `Send`: a recorder takes it (`take_recording_tap`), drains it on
its own thread, and hands it back (`return_recording_tap`). A tap from a
monitor that has since been restarted is abandoned (`is_abandoned`) and is
dropped rather than returned.

## Known limitations (v0.1)

- ~~**`cpal` 0.15.3 can't see output-only devices on macOS.**~~ **Gone with
  `cpal` on macOS (2026-09-11).** Its CoreAudio backend answered output config
  queries with an input-enabled AudioUnit, so the Mac mini's own speakers,
  HDMI displays, and the output half of a USB interface vanished from
  `list_output_devices` and couldn't be opened. This was briefly worked around
  with CoreAudio calls, then superseded the same day by the native CoreAudio
  backend, which enumerates devices from the HAL directly. See
  [AUDIO_BACKENDS.md](AUDIO_BACKENDS.md).
- **AirPlay output devices only appear in `list_output_devices` once
  actively selected as the OS's current output -- not before, and not by
  their real name.** First observed 2026-09-06 while adding a device
  picker to `sng-bass-blaster`: an idle macOS AirPlay speaker (a
  HomePod-style speaker, visible in System Settings -> Sound -> Output
  with Type "AirPlay") is invisible not just to `cpal` but to
  `system_profiler SPAudioDataType`'s own "Devices" list -- a macOS/
  CoreAudio-level omission, not a gap in `cpal`'s enumeration. **Resolved
  the same day:** selecting that AirPlay target via the menu-bar Sound
  control (Control Center's Sound module -- the redesigned System
  Settings -> Sound *pane* did not reliably respond to the same kind of
  scripted selection attempt) causes macOS to create a real CoreAudio
  device, confirmed both in `system_profiler` and via `cpal`/
  `list_output_devices`. That device is always named plainly **`AirPlay`**
  (generic, not the speaker's own name), and `LiveMonitor` opened a real
  stream against it successfully. Practical upshot: once a user has
  picked their AirPlay target through the OS's own picker, it just shows
  up in this crate's existing device list under the name `AirPlay` --
  no code change needed for that part. What this crate still cannot do:
  discover named AirPlay targets independently, or select one
  programmatically without going through the OS's own UI -- Apple gates
  that behind the user-facing route picker by design (apps aren't meant
  to silently redirect system audio output). A **Bluetooth** device using
  the classic A2DP profile (headphones, most Bluetooth speakers)
  enumerates as an ordinary CoreAudio device immediately, with no such
  selection step, and works out-of-the-box. Full writeup, including the
  UI-automation approach that worked (and the one that didn't), in
  `sng-bass-blaster/docs/UI_INPUT_FINDINGS.md`. Any caller wanting a
  specific *named* AirPlay target selected from inside the app itself
  (not just picking up whatever's already active) still needs to send the
  user to the OS's own Sound picker for the selection step, or implement
  real device discovery/selection independently of CoreAudio (mDNS/RAOP)
  -- see that doc's "Where this could go next" section.
- ~~**No sample-rate conversion; fixed ring, not adaptive.**~~ **Fixed
  2026-09-11 by `DriftResampler`** (`src/resample.rs`, backend-neutral, so
  every platform gets it). The monitor ring used to be a plain copy between
  the two devices' callbacks: pairs had to share a sample rate, and if the
  input clock was the slower one the ring drained to empty and the monitor
  went silent for good -- observed 2026-09-09 in `sng-bass-blaster`. The
  output callback now reads the ring at `input_rate / output_rate`, corrected
  by a PI controller that holds a target fill measured with timestamps. On the
  Mac mini (USB PnP Audio Device -> Mac mini Speakers, 128-frame buffers) it
  holds 6.7 ms with a measured drift of a few ppm and zero underruns; a C920
  webcam's 16 kHz stereo mic plays into a 48 kHz output. See
  [AUDIO_BACKENDS.md](AUDIO_BACKENDS.md) for the controller design and the two
  mistakes simulation caught.
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
