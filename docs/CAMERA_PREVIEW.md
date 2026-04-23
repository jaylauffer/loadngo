# Camera Preview

`host-desktop/src/bin/camera_preview.rs` is the Linux webcam preview utility for `loadngo`.

## Current shape

- Camera capture runs through `ffmpeg` against `/dev/video*`.
- Linux stream I/O is driven by `loadngo-proactor` with `EpollPort`.
- The proactor registers the `ffmpeg` stdout fd as readable and decodes MJPEG frames as readiness arrives.
- The preview window still uses the existing `loadngo-host-desktop` Linux frame loop, so the host presentation side is not yet a full proactor host. The capture path is proactor-driven now.

This matches the direction in [PROACTOR_ARCHITECTURE.md](./PROACTOR_ARCHITECTURE.md): move real I/O off fixed worker sleeps and onto readiness/deferred work first, then finish the host-side wake model separately.

## Why the preview could freeze on one frame

The preview uses a stable image key, `camera/live`, so the renderer can treat the camera as one logical texture.

That only works if the graphics backend invalidates the GPU texture when the bytes behind that key change. The Linux GLES cache now drops and recreates GPU textures when a retained image key gets a different pixel buffer, which keeps the preview live and lets `Restart Stream` show fresh frames instead of the first uploaded image.

## Restart behavior

- `Restart Stream` and `R` stop the current proactor worker, kill the active `ffmpeg` child, and start a fresh worker.
- Unexpected EOF or read/decode failures trigger a deferred restart after a short backoff.

## Save formats

- PNG is the default GUI save path because it is lossless.
- JPG remains available when file size matters.
- `--capture-once --format png|jpg` keeps the same choice for non-GUI capture.

## Operational notes

- `ffmpeg` must be installed.
- The current implementation expects a V4L2 camera device on Linux.
- `camera_preview --list-devices` filters obvious non-camera `/dev/video*` nodes so the default device selection prefers real webcams.
