# Camera Preview

`host-desktop/src/bin/camera_preview.rs` is `loadngo`'s webcam preview and
capture test harness. It runs on **Linux and macOS**; see
[Windows](#windows-designed-not-implemented) below.

Everything about *talking to a camera* lives in the **`loadngo-camera`**
crate (`camera/`), which the preview and `sng-rusty`'s motion detector both
use, so there is one implementation of the ffmpeg plumbing rather than one
per caller.

## Shape

- Capture is an `ffmpeg` child process on every platform, emitting
  `rawvideo`/`rgba` frames on stdout, scaled by ffmpeg to a fixed size so
  frames can be sliced out of the byte stream without parsing them.
- The preview drives that pipe with `loadngo-proactor`, using
  `loadngo_proactor::PlatformPort` -- `IoUringPort` on Linux, `KqueuePort`
  on macOS -- rather than naming a backend.
- The preview window still uses the existing `loadngo-host-desktop` frame
  loop, so the host presentation side is not a full proactor host. The
  capture path is proactor-driven. This matches the direction in
  [PROACTOR_ARCHITECTURE.md](./PROACTOR_ARCHITECTURE.md): move real I/O onto
  readiness first, finish the host wake model separately.

## What is actually platform-specific

Less than it looks. Only three things differ, and all three live in
`loadngo-camera`'s `device` module:

| | Linux | macOS | Windows |
| --- | --- | --- | --- |
| `ffmpeg -f` | `v4l2` | `avfoundation` | `dshow` |
| device id | `/dev/video*` | device index | device name |
| `-i` spelling | path as-is | `<index>:none` | `video=<name>` |

Two quirks are worth knowing because both fail the *entire* capture rather
than degrading:

- **AVFoundation rejects framerates the device does not advertise.** Asking
  for 6 fps on a C920 fails with "Selected framerate (6.000000) is not
  supported by the device", and so does omitting `-framerate`, because
  ffmpeg's own default guess of 29.97 is not advertised either.
  `device::input_framerate` snaps the request to the nearest rate every UVC
  webcam advertises (30/24/20/15/10/5); the `fps=` output filter then sets
  the real preview rate.
- **AVFoundation opens a microphone if you let it.** The input is pinned to
  `<index>:none` so a video-only preview never grabs an audio device (or
  triggers a microphone permission prompt).

Enumeration is per-platform too: `/dev/video*` plus sysfs labels on Linux,
`ffmpeg -list_devices` parsing on macOS and Windows. The Linux path filters
obvious non-camera nodes -- a Pi exposes a dozen ISP/codec `/dev/video*`
nodes and no webcam, so an unfiltered "first device" default picks a codec.

## Windows: designed, not implemented

`camera_preview` will not stream on Windows today, and says so rather than
pretending.

`loadngo-proactor`'s `ReadinessPort` is `RawFd`-based, and `IocpPort`
deliberately does not implement it: IOCP has no readiness model for the
anonymous pipe `ffmpeg` writes to. So Unix registers the pipe and drains it
from a readiness callback, and Windows cannot.

The intended Windows path is a dedicated **reader thread** calling
`CaptureStream::pump` -- which already returns after a single blocking read
on Windows rather than looping to `WouldBlock` -- and handing each batch back
with `ProactorHandle::enqueue_work`, so frames still arrive on the proactor
thread and nothing above the transport changes.

What blocks it is ownership, not plumbing: the reader thread has to own the
stream in order to block on it, while shutdown has to be able to kill the
child *without* waiting for that read to return. `CaptureStream` needs to
split into a reader half and a control half first. That was left undone
deliberately -- there is no Windows machine here to compile or test against,
and a plausible-looking deadlock would be worse than an honest error.

## Preview behaviour

- The preview uses a stable image key, `camera/live`, so the renderer treats
  the camera as one logical texture. That only works if the graphics backend
  invalidates the GPU texture when the bytes behind that key change; the
  Linux GLES cache drops and recreates textures on a new pixel buffer, which
  keeps the preview live and lets `Restart Stream` show fresh frames instead
  of the first uploaded image.
- `Restart Stream` and `R` stop the current worker, kill the active `ffmpeg`
  child, and start a fresh one. Unexpected EOF or a read failure triggers a
  deferred restart after a short backoff.
- PNG is the default save format because it is lossless; JPG is available
  when size matters. `--capture-once --format png|jpg` does the same without
  a window.

## Operational notes

- `ffmpeg` must be installed.
- `camera_preview --list-devices` prints `id<TAB>name`.
- `cargo run -p loadngo-camera --example list_cameras` shows the same
  enumeration plus the resolved `ffmpeg` input flags, which is the quickest
  way to check a new machine.
