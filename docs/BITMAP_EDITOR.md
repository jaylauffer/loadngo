# Bitmap Editor

`bitmap-editor` is loadngo-cpp's BitmapEditor (2004, Win32, about 1,500
lines of C++) ported to the loadngo Rust framework (Jay, 2026-10-14: "ensure
it can work with all raster type images, jpegs, pngs, gifs, tiffs, ideally
even mp4s. Utilize the loadngo proactor"). It is one binary,
`bitmap_editor`: a window, and the same pictures without one for scripts and
checks.

```
cargo run -p bitmap-editor --release -- photo.png
bitmap_editor --probe clip.mp4 --frame 5 --pixel 10,10
bitmap_editor --edit icon.png --set-alpha 3,4,128 --premultiply --save out.png
bitmap_editor --render icon.png --out scene.png --size 1000x700 --hover 10,10
```

`bitmap_editor --help` lists every option.

## What the C++ editor did, and what this one does

| C++ (`loadngo-cpp/BitmapEditor`) | Here |
|---|---|
| `BitmapView`: the bitmap at 1:1 on a `0xe9e9e9` panel, alpha-blended | The picture panel, on the same grey: shrunk to fit, or enlarged by a whole factor when small. It outlines the part of the picture the grid shows. Press and drag on it to move that part. |
| `PixelView`: every pixel as a 16x12 cell, blended by its alpha over brush `0x000f0503` | The grid: same cells and same backdrop (RGB 3, 5, 15), showing the part that fits. It pans with the wheel or the arrow keys. The C++ drew every pixel, which only works for icons. |
| The hovered cell framed in the pixel's own colour | Framed on its edges in black or white, whichever stands out, so its middle still shows the pixel. The C++ frame vanished on an opaque pixel. |
| Readout: alpha, colour and raw value in hex; a blended swatch; R, G and B swatches | The same, plus the pixel's position, and the size and format |
| Left click: set the pixel's alpha from an edit box (`ES_NUMBER`) | Left click on a cell (or Enter on the hovered pixel): set its alpha from a 0-255 slider. The colour is kept (see "Alpha" below). |
| Right click: blue += 0x8f (a debugging leftover) | Not ported |
| File: Open, Save (`.bmp` only), Close, Exit | File: Open, Save, Save As, Close, and Quit where the window draws its own menus. Files can also be dropped on the window. |
| Edit: Paste (a `CF_DIB` from the clipboard) | Undo and Redo (both new). Paste is not ported: loadngo's hosts have only a text clipboard. |
| Image: Pre-Multiply Alpha, Make Opaque, Refresh | Pre-Multiply Alpha and Make Opaque, with the C++ arithmetic (`c * a / 255`, rounded down). Previous Frame and Next Frame are new (Page Up and Page Down; Home and End for the first and last). Refresh is dropped: the window redraws whenever something changes. |
| Help: About | Not ported |

On macOS the menus are in the system menu bar; on Linux and Windows the
window draws them (`ui_core::MenuBarModel`). Open and Save As use loadngo's
own file dialog. Leaving with unsaved changes (Open, Close, Quit) asks first:
Save, Discard or Cancel. The window's close button still closes without
asking, as in the code editor.

### Alpha

The C++ editor held Windows DIB pixels, which `AlphaBlend` takes
premultiplied, so setting a pixel's alpha there also premultiplied that
pixel. Here pixels are straight-alpha RGBA, as PNG, TIFF and the renderer
keep them. Setting alpha changes only alpha, and Pre-Multiply Alpha is the
explicit command.

## Formats

| Format | Read | Write |
|---|---|---|
| PNG, BMP, TIFF | `image` crate, exact | yes, RGBA |
| JPEG | `image` crate (within 3 per channel of ffmpeg's decode on the test pattern) | yes, quality 92; alpha dropped |
| GIF | `image` crate; every frame, composited onto the canvas as a viewer shows it | yes. An animated GIF saved as GIF keeps every frame and its delays, with the edited frames replaced. |
| MP4, MOV, WebM, MKV, AVI, WebP and anything else ffmpeg reads | ffmpeg, frame by frame (below) | no. Save As writes the frame as a picture. |

TIFF reads the first page. 16-bit images and colour profiles come in as
8-bit sRGB values with the profile ignored. Saving another format than the
source re-encodes the frame; JPEG is lossy each time it is saved.

### Video through ffmpeg

`src/video.rs` runs `ffprobe` once when a file opens. It lists the video
stream's packets; their presentation times, sorted, are the frame index, and
their count is the frame count. Frame `n` is one `ffmpeg` run that seeks
(`-ss` before `-i`) to halfway between frames `n - 1` and `n`, less the
file's start time, and writes one frame as PAM (`-c:v pam -pix_fmt rgba`).
ffmpeg decodes from the keyframe before the seek point and drops the frames
before it, so frame `n` costs a few decodes, not `n` of them. PAM states the
frame's size, which is the size after ffmpeg rotates phone video.

The tests decode every frame of an H.264 clip with B-frames, a keyframe
every 12 frames, NTSC timing (30000/1001) and a 1.5 s start time. Each frame
equals, byte for byte, ffmpeg's own `select=eq(n,N)` decode from the start,
which is also the fallback when a file has no timestamps or two frames share
one. By hand on 2026-10-14 it also matched on VP9 WebM, ProRes MOV and the
10-frame test clip.

ffmpeg and ffprobe are found on `PATH`, else in `/opt/homebrew/bin`,
`/opt/homebrew/opt/ffmpeg-full/bin`, `/opt/homebrew/opt/ffmpeg/bin`,
`/usr/local/bin` or `/usr/bin` (a program started from the Finder has a short
`PATH`). Without them, video and the other ffmpeg formats report that
ffmpeg was not found; the five still formats still work.

## Where the work runs

Opening a file, reading a frame and saving are `editor::Job`s. The window
runs each on the host's offload workers (`loadngo_host_desktop::offload`).
A result comes back as a completion on the host proactor and wakes the
frame, and the editor applies it. At most one read and one save run at once.
A read asked for while another runs waits, and a newer request replaces a
waiting one, so holding Page Down over a long video reads the frames it
lands on, not every frame in between. A decoder that panics on a malformed
file becomes an error in the status line.

Paint and input do no file work. The frame being edited is uploaded under
one image key only when its pixels change. Each frame asks for
`FrameDemand::Idle`, except while the file dialog's list glides after a
scroll.

Two host changes came with this, both found while measuring the editor:

- **Linux and Windows:** an offloaded job's result, a `completion()` and a
  `frame_waker()` now wake winit's event loop (`wake_host`), as macOS's did.
  Before, an idle app saw the result only at its next input: the editor
  showed a file it had opened only after the mouse moved. This also affects
  the code editor's file loading, `cargo check` results and terminal output
  on Linux and Windows.
- **macOS:** a key window gets mouse-moved events from anywhere on the
  screen, and each one ran a frame of every idle loadngo app. Motion outside
  the window with no button held is now reported once, so hover clears, and
  then dropped. Drags still report everywhere.

## Measured

macOS, Mac mini, release build, 2026-10-14, the 64x48 test pattern:

- Opened from the command line: `bitmap_editor_loaded` on stderr at once.
- No input at all: 6 to 8 context switches per 10 s (`top`), 0.0% CPU, no
  frames run.
- Focused with the pointer moving outside the window: before the host
  change, 560 frames in 20 s; after it, none.
- The pointer moving over the window runs frames, as hover needs. A frame
  whose drawing did not change is not presented (the Metal host compares
  commands and image revisions).

Not measured: Linux and Windows idle; iOS and Android, which have the
editor's host but no ffmpeg and were not tried. Thermal state was not
recorded (Mac mini, idle).

## Testing

`cargo test -p bitmap-editor` (21 tests). The ffmpeg tests skip with a note
when ffmpeg is not installed, which is the case on the CI runners.

- `render`: the editor drawn by the software rasterizer, with the hovered
  pixel's cell showing the pixel exactly and its frame on the edges only; a
  translucent pixel blended over the backdrop; the picture at a whole scale.
- `editor`: a click on a cell sets alpha, Undo and Redo; frames read through
  jobs, edits kept across frame changes, undo returning to its frame; one
  read at a time with the newest waiting; Save, and Quit asking about unsaved
  changes; wheel and arrow-key panning; dragging on the picture; the drawn
  menu bar's shortcuts and Page Down, End, Home.
- `picture`: PNG, BMP and TIFF round trips exact; JPEG close and opaque; GIF
  frames and frame count; an animated GIF saved with one frame edited; the
  C++ edits; a file that is no picture.
- `video`: PAM parsing, seek points, container names, and every frame of the
  clip above against ffmpeg.
- `main`: the command line.

Five mutations (hover frame over the whole cell, a backdrop blend divided by
256, seeking to the previous frame, premultiplying by 256, accepting a
superseded read) each fail a test.

By hand on 2026-10-14, against ffmpeg's decode of each file: whole frames
equal for PNG with alpha, BMP, TIFF, all three frames of a GIF and frames 0,
5 and 9 of an H.264 MP4; JPEG within 3 per channel.

`--render` draws everything but text: fonts belong to each host's window
code, and no window is open.
