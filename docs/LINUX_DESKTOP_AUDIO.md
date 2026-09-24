# Linux desktop audio: Rust-owned session playback

Status: implementation in progress, 2026-09-23; not yet a verified release.

Jay's live Dolores playtest exposed a routing defect: game PID 753085 owned
`/dev/snd/pcmC0D0p` (HDMI) while the desktop PipeWire default was the USB PnP
output at 40% volume. PipeWire had no game stream. Installing an ALSA bridge
would change that machine, not give Loadngo dependable desktop integration.

## Contract

- `open_desktop_output_stream` is the session-managed playback entry point.
  Linux uses PipeWire directly through its public ABI; the adapter, lifecycle,
  buffer validation and callbacks are Rust. No ALSA plugin, shell player,
  subprocess, pkg-config or generated C shim is involved.
- Loadngo retains decoding, mixing, music/SFX/voice preferences and gain.
  Desktop master/application volume and mute are applied downstream by
  PipeWire. Do not copy system volume into the mixer and apply it twice.
- No target means session policy; a target is a PipeWire `node.name` or
  `object.serial`. Hardware card names are not PipeWire identities. Routing
  and device disappearance follow WirePlumber policy; do not seize the first
  card, force HDMI, or switch audio just because a controller appears.
- Missing library/server/output, rejected format and disconnected streams
  produce explicit errors. No automatic raw-ALSA fallback that circumvents
  desktop controls. A missing sound server must not crash the game.
- Direct hardware capture/monitoring and physical-format queries retain the
  existing ALSA implementation. This is intentional: those APIs describe a
  converter, not a desktop mix. ALSA remains part of the OS.

## Implementation boundaries

PipeWire >= 0.3.49 is loaded once from `libpipewire-0.3.so.0`. The Rust ABI
subset is in `audio-io/src/backend/pipewire/ffi.rs`, checked against the public
1.4.2 headers. A small Rust SPA POD codec offers fixed f32 stereo/48 kHz;
PipeWire performs downstream conversion. Buffer size is negotiated, not
claimed to equal a latency hint. No physical resolution claim is made.

Each stream has one PipeWire event-loop thread. Control and process callbacks
are serialized (`RT_PROCESS` is not enabled), bounded to 8192 frames, and
never do filesystem/network work. There is no busy-polling or application
timer loop. User callback panics are caught and reported, output is silenced,
and non-finite/out-of-range samples are sanitized. Startup waits at most
three seconds for a streaming, negotiated output. Drop stops/joins before
freeing callback state. Do not drop a stream from its own callback.

This first playback increment does not replace the capture backend, add a
device picker, promise automatic recovery after a server restart, or claim
thermal safety without measurements. Default-device migration is session
policy and must be tested on target systems.

## Validation harness

```
cargo run -p loadngo-audio-io --bin desktop_audio_probe -- --help
cargo run -p loadngo-audio-io --bin desktop_audio_probe -- --seconds 5
# Only when audible output is appropriate:
cargo run -p loadngo-audio-io --bin desktop_audio_probe -- --seconds 10 --tone
```

Default probe output is silence. Verify its stream in `wpctl status`, absence
of direct `/dev/snd/pcm*` handles in its process, closure/removal after exit,
mute and per-stream gain on an isolated test sink, bounded missing-server and
missing-target failure, and callback panic containment (`--panic`). Do not
change global volume or restart Jay's running game for these checks.

Release gates additionally include actual game music/SFX, master mute/volume,
output switching/unplug/replug, stable controller input, and representative
idle/active CPU, wakeups, memory/thread counts and thermal observations on
Dolores. A successful silent probe alone does not establish those properties.

## Upstream API references

- [PipeWire streams](https://docs.pipewire.org/page_streams.html)
- [PipeWire thread-loop locking/lifetime](https://docs.pipewire.org/page_thread_loop.html)
- [1.4.2 stream ABI](https://gitlab.freedesktop.org/pipewire/pipewire/-/blob/1.4.2/src/pipewire/stream.h)
- [SPA format headers](https://gitlab.freedesktop.org/pipewire/pipewire/-/tree/1.4.2/spa/include/spa/param/audio)
