# Audio Backends

Status: **decided 2026-09-11** -- `loadngo-audio-io` replaces `cpal` with
loadngo-owned platform backends, one platform at a time. CoreAudio (macOS)
is first; Linux and Windows stay on `cpal` behind the same seam until their
own backends land.

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
LiveMonitor / RecordingTap / probe_input_capabilities / list_*_devices
        (backend-neutral: rings, taps, DriftResampler, gain/mute)
                          |
                 backend::platform
          +---------------+----------------+
     coreaudio (macOS)          cpal_host (Linux, Windows, interim)
```

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

## Order and gates

1. **CoreAudio** -- verified on the Mac mini: device lists (including
   output-only devices), a USB interface into the built-in speakers, drift
   statistics over a long run, recording, and converter format switching.
2. **ALSA** (Linux) -- built and tested on `dolores`.
3. **WASAPI** (Windows) -- when a Windows machine exists; a type check is not
   a gate.
4. **Playback** -- `loadngo-host-desktop`'s `AudioMixer` still plays through
   `rodio`, and so through `cpal`. Removing `cpal` from the workspace means
   moving playback onto these backends too.

Android/iOS capture has no caller; no backend is planned until one exists.
