# Audio Backends

Status: **decided 2026-09-11** -- `loadngo-audio-io` replaces `cpal` with
loadngo-owned platform backends, one platform at a time. CoreAudio (macOS)
and ALSA (Linux) are in; Windows stays on `cpal` behind the same seam until
WASAPI lands.

Playback followed capture. iOS is off rodio and cpal entirely (its own
RemoteIO backend), and desktop has a native path behind the
`native-desktop-audio` feature, default off until it has been measured
against rodio on each platform. Android never used cpal at all -- it plays
through `android.media.MediaPlayer`.

Linux needs no ALSA crate. The bindings are declared in-tree, so nothing
depends on `pkg-config` finding a target ALSA install -- which is also what
had stopped `cargo check --target aarch64-unknown-linux-gnu` from working on
a Mac, and it works now.

## Why

`cpal` was the shortcut that got `sng-bass-blaster` its first sound. Three
real problems surfaced in one day of building its recorder on macOS, each
worked around rather than fixed, and each rooted in `cpal`'s abstraction
rather than in our code:

1. **It hides the converter.** `cpal`'s CoreAudio backend reports every
   device as 32-bit float -- the HAL's mixing format -- so a 24-bit
   interface and a 16-bit webcam look identical. Recording "at the
   converter's resolution" needed raw CoreAudio calls anyway.
2. **It can't see output-only devices.** `cpal` 0.15.3 answers output config
   queries with an AudioUnit built with *input* enabled, which fails for the
   Mac mini's own speakers, HDMI displays, and the output half of a USB
   interface -- so they silently vanish from `output_devices()` and can't be
   opened by name. (Fixed in `cpal` 0.16, which `rodio` 0.17 doesn't allow.)
3. **It leaves clock drift to the caller.** Two nominally 48 kHz devices on
   separate crystals disagree by tens of ppm. A plain ring between them
   drains to silence over minutes (seen 2026-09-09) or piles up latency, and
   devices at different rates can't be bridged at all.

The data model we actually need -- devices with stable identities, physical
and stream formats, float frames in callbacks, timestamps, device-death
notification -- is small. Owning it costs less than working around a
general-purpose abstraction that gets these wrong, and fits the rule that
`loadngo` builds its necessities rather than borrowing them.

## Shape

```
capture:  LiveMonitor / RecordingTap / probe_input_capabilities / list_*_devices
                (backend-neutral: rings, taps, DriftResampler, gain/mute)
playback: AudioMixer -> MusicController / SfxController / VoiceController
                (backend-neutral: buses, volumes, mute, preferences)
                          |
                 backend::platform
          +---------------+----------------+
  coreaudio (macOS)     alsa (Linux)      cpal_host (Windows, interim)
```

Playback reaches those same backends through `open_output_stream`, the
public half of what `LiveMonitor` does internally. A game host wants
somewhere to push mixed samples and has no input device at all; giving it a
second, parallel audio stack would double the places a device-death or
format bug can hide.

Each platform module provides the same handful of functions -- no trait
object, one backend per build, selected by `cfg`:

| function | meaning |
| --- | --- |
| `list_devices(direction)` | names, default flagged, no duplicates |
| `describe(direction, name)` | device name, sample rate, channels, buffer frames |
| `open_input(name, callback)` | calls `callback(interleaved_f32, channels)` on the audio thread |
| `open_output(name, callback)` | calls `callback(&mut interleaved_f32, channels)` to fill |
| `probe_input_capabilities(name)` | stream + physical formats |
| `set_input_physical_format(name, format)` | switch the converter |

A stream handle stops its device I/O when dropped and reports device death.

Backend-neutral pieces:

- **`DriftResampler`** (`resample.rs`) reads the monitor ring from the output
  callback at `input_rate / output_rate`, corrected by a PI controller that
  holds the ring at a target fill. Fill is measured with timestamps
  (`InputClock`) -- queued samples plus what the input device has captured
  since its last callback -- because a raw reading carries a slow sawtooth as
  the callbacks' relative phase slides under drift. Simulated: a -300 ppm or
  +400 ppm input clock over ten minutes settles within 60 ppm with no
  underrun; 16 kHz and 44.1 kHz inputs play at the right pitch into 48 kHz.
- The analysis tap, the lossless recording tap, and gain/mute are unchanged
  and live above the seam.

## CoreAudio backend

- **Raw HAL IOProcs** (`AudioDeviceCreateIOProcID`), not AUHAL AudioUnits:
  the lowest layer that delivers the device's float virtual format, with no
  conversion stage we didn't ask for. Multi-stream devices are interleaved
  into one frame layout; single-stream devices are passed through with no
  copy. Scratch buffers are sized from `kAudioDevicePropertyBufferFrameSizeRange`
  up front, so the IO thread never allocates.
- **Devices are CoreAudio objects**, resolved by name only at the API edge.
  Same-named objects are expected (a USB interface's input and output halves
  are separate devices) and resolved by direction.
- **Device death** (`kAudioDevicePropertyDeviceIsAlive`) and IO overloads are
  observed through property listeners that touch only atomics shared with the
  stream handle, never the IO proc's state.
- **Panics never cross the FFI boundary**: the IO proc catches them, outputs
  silence, and flags the stream as failed.
- `preferred_buffer_frames` sets `kAudioDevicePropertyBufferFrameSize`, which
  CoreAudio scopes to this process.

## ALSA backend

- **A thread per stream, not a callback.** ALSA has no IOProc, so each stream
  owns a thread that blocks in `snd_pcm_readi`/`snd_pcm_writei` one period at
  a time. A period *is* the callback size, so `preferred_buffer_frames` maps
  onto it directly. Stopping sets a flag rather than calling into ALSA from
  another thread: the loop notices within one period (2.7 ms at 128 frames)
  and closes the device itself, so every `libasound` call for a device stays
  on the one thread that owns it.
- **Devices open by `hw:CARD,DEV`**, which gives the hardware's own format
  rather than whatever the plug layer would convert to -- the same "no
  conversion we didn't ask for" rule as CoreAudio. `plughw:` is the documented
  fallback for hardware that speaks nothing we convert: the Pi's HDMI accepts
  only IEC958 subframes, so raw access fails outright while `plughw:` plays
  ordinary PCM. `set_rate_resample(0)` stops ALSA resampling behind our back;
  `DriftResampler` bridges rates instead.
- **There is no hidden converter here.** ALSA's `hw` parameters *are* the
  hardware's, so `current_physical` and `current_stream` describe the same
  thing, and the format is chosen when the stream opens -- there is nothing to
  switch, so `set_input_physical_format` reports `UnsupportedOnPlatform`.
  This is the one real capability difference from CoreAudio.
- **`default` is a config alias, not a device.** Unlike CoreAudio's default
  device, ALSA's `default` PCM can have no slave in one direction: any Pi has
  a `default` that plays but cannot capture. Passing no device name therefore
  prefers `default` (so PipeWire or dmix still routes, and the device stays
  shared) but falls back to the first enumerated device in that direction
  when `default` won't open, rather than failing with a usable device listed.
- **Xruns** are recovered with `snd_pcm_recover` and counted -- they are this
  platform's equivalent of CoreAudio's overload notification. `-ENODEV` ends
  the thread and is reported as a disconnected device.
- **The bindings are hand-written** (`ffi.rs`). ALSA's enums are implicit in
  its headers and can't be grepped out, so the constant values were read off
  `dolores` with a C probe rather than guessed.

## iOS backend (playback)

- **One RemoteIO `AudioUnit`** whose render callback mixes a streaming music
  track with resident effects. `audio-io` is desktop-only by `cfg`, so iOS
  drives CoreAudio directly through `coreaudio-sys` rather than the seam
  above -- on iOS the AudioUnit symbols ship inside AudioToolbox.
- **Music streams, effects don't**, because the assets differ by three orders
  of magnitude: the games' music runs 5-10 minutes (`flutterrung.ogg` is
  603 s, ~212 MB decoded to `f32`) while their effects are 20 clips of at
  most 17 KB. Decoding is `lewton`, the same decoder Android uses.
- **48 kHz, and the two 44.1 kHz tracks convert on the decoder thread**, so
  the render callback never resamples.
- **`AVAudioSession` must be set and activated** or nothing plays at all, and
  a refusal leaves the app silent but otherwise healthy -- so it is reported
  rather than discarded.
- **No fades and no playback-rate control**, matching Android rather than
  inventing a bar no mobile backend meets.

## Desktop backend (playback, `native-desktop-audio`)

- **Feature-gated, default off.** Two `mod imp` definitions cannot coexist
  under one `cfg`, so a feature is what keeps rodio and the native path both
  compilable and comparable on one machine. macOS and Linux are where audio
  currently works best; swapping them in one step would leave no way to tell
  a regression from a change.
- **Richer than the mobile backends**, because the games rely on it: several
  tracks decode at once so one can fade under another, there is a cue/resume
  state machine with a two-second advance debounce, and tracks can play from
  `&'static [u8]`.
- **`audio_harness`** (`host-desktop/src/bin`) drives `AudioMixer` end to end
  under either backend. The interesting code only runs when something calls
  `update(dt)` -- the fade ramp, a cue interrupting a playlist, the resume
  behind it -- which no unit test reaches.

## What testing found

- **Simulation caught two controller mistakes before any hardware run.** The
  first proportional gain gave a damping ratio near 0.04, so a +400 ppm clock
  rang for minutes. And reading raw ring fill made the controller chase a slow
  sawtooth: where the output callback lands relative to the input one slides
  under drift (a 26 s cycle at 400 ppm), so the fill reading swings by a whole
  burst. `InputClock` timestamps fix the measurement.
- **Hardware caught two more.** Priming aligned the *queued* samples with the
  target while the controller steers *queued plus in-flight*, so every start
  began half a burst high and spent seconds at the +1000 ppm limit. And the
  target used the output's buffer size from before `open_output` applied the
  requested size, so it sized for 512 frames while the device ran 128.
- **CoreFoundation had been linked by `cpal`.** Without it the backend failed
  to link (`_CFRelease`); the backend now declares the framework itself.
- **Measured on the Mac mini**, USB PnP Audio Device into Mac mini Speakers:

  | buffer frames | drift-stage delay | underruns / overflows / overloads |
  | --- | --- | --- |
  | 512 (device default) | 26.7 ms | 0 / 0 / 0 |
  | 256 | 13.3 ms | 0 / 0 / 0 |
  | 128 | 6.7 ms | 0 / 0 / 0 |
  | 64 | 3.3 ms | 0 / 0 / 0 |

  The correction settles at a few ppm, the real difference between the USB
  interface's crystal and the Mac mini's. `sng-bass-blaster` uses 128.

- **ALSA's `default` is a config alias, and only hardware said so.** Every
  test that named a device passed on the first run; the two that asked for
  "the default input" failed with `Invalid argument`, because a Pi's `default`
  PCM has no capture slave and the backend delegated to it rather than
  resolving a device. It would have looked correct on any Linux desktop with
  an analogue input, and it is exactly the path an app takes before a device
  has been chosen -- see the `default` bullet under the ALSA backend.
- **Measured on `dolores`** (Pi 4), USB PnP Audio Device in and out, 48 kHz
  mono, 128-frame periods, over 600 s:

  | fill (target 320) | correction | underruns / overflows / overloads |
  | --- | --- | --- |
  | 312-321 (6.5-6.7 ms) | -84.7 to +5.4 ppm | 2 / 0 / 0 |

  Steady state holds within a sample or two of target at a couple of ppm; the
  -84.7 ppm reading is the first second, correcting the priming offset. Of the
  two underruns one is that same startup priming, but the other landed at
  t=299 s in an otherwise flat run. The Mac mini did ten minutes with none, so
  this is a real difference and not yet explained -- the capture and playback
  threads here are ordinary threads with no realtime priority, which is the
  first thing to look at if it turns out to matter.

## Order and gates

1. **CoreAudio** -- verified on the Mac mini: device lists (including
   output-only devices), a USB interface into the built-in speakers, drift
   statistics over a long run, recording, and converter format switching.
2. **ALSA** (Linux) -- verified on `dolores` against a USB PnP dongle: device
   lists with the default flagged, capability probe, a recording tap, a duplex
   monitor, and a ten-minute drift run. Then exercised by a real app:
   `sng-bass-blaster` recorded a take through it, dongle in and HDMI out.
3. **WASAPI** (Windows) -- when a Windows machine exists; a type check is not
   a gate.
4. **Playback** -- iOS is done (RemoteIO, verified by ear on both games).
   Desktop has a native path behind `native-desktop-audio`:
   - **macOS**: trace-identical to rodio through fade-in, cue and resume, and
     acoustically within the measuring instrument's ~0.8 dB repeatability
     across 100 Hz-6 kHz on the same passage.
   - **Linux** (`agnes`): the same trace parity, and routing confirmed --
     `wpctl` shows a `PipeWire ALSA [audio_harness]` node with both channels
     linked `[active]` to a hardware sink, appearing and disappearing with the
     process. Note the process holds no `/dev/snd` handle and should not: the
     PipeWire client plugin loads into it while the daemon owns the kernel
     node, so an fd check is the wrong instrument on such a machine.
   - **Not yet**: audible confirmation on Linux. That box's dongle sits in an
     IEC958 passthrough profile, and active links say nothing about analogue
     sound leaving it.

   It stays default-off until that last point is closed. `cpal` leaves the
   workspace only once the flag flips *and* Windows has a WASAPI backend --
   `rodio` is still the default desktop path today.

Android/iOS capture has no caller; no backend is planned until one exists.
