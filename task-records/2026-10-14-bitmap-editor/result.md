# Result: loadngo-cpp's BitmapEditor on the loadngo Rust framework

Worker: claude-code-macmini (Claude Code, Opus 5.5, in Jay's session). Intent and check in
loadngo 63850d2c.

commit: a12b9c64d8632994020dd01d13d1495dfeef064a
test: the_rendered_editor_shows_the_hovered_pixel_in_its_cell

## What changed

- `bitmap-editor` (new workspace member, binary `bitmap_editor`), a12b9c64:
  - The C++ editor's picture view, 16x12 zoomed grid blended over its dark backdrop,
    hovered-pixel readout (alpha, colour, raw value) and swatches; a click on a cell sets
    that pixel's alpha (from a slider); Pre-Multiply Alpha and Make Opaque with the C++
    arithmetic. New: undo and redo, Save As, frames (Page Up/Down, Home/End), panning the
    grid, drag and drop, asking before unsaved changes are lost. Menus: system menu bar on
    macOS, drawn elsewhere; loadngo's file dialogs.
  - PNG, JPEG, BMP, TIFF and GIF (every frame) through the image crate; MP4, MOV, WebM and
    anything else ffmpeg reads through ffmpeg, frame by frame (one seek per frame, from
    ffprobe's packet times). Animated GIFs save with every frame; video frames save as
    pictures.
  - Opening, reading frames and saving run on the host's offload workers: one read and one
    save at a time, the newest request replacing a waiting one; results come back as host
    proactor completions and wake the frame. Paint does no file work; every frame asks for
    FrameDemand::Idle.
  - `--probe`, `--edit` and `--render` (the software rasterizer) as the intent names them.
  - docs/BITMAP_EDITOR.md: what was ported and what was not, formats, where the work runs,
    measurements, tests.
- Host changes found while measuring:
  - ce1675b9: on Linux and Windows, offloaded results, completion() and frame_waker() now
    wake winit's loop. Before, an idle app saw a result only at its next input.
  - 3e157985: on macOS, pointer motion outside the window, with no button held, no longer
    runs a frame of every idle app (reported once, then dropped).

## How it was checked

- 21 bitmap-editor tests (render, editor input, formats, every frame of an H.264 clip with
  B-frames and an offset start against ffmpeg's own decode, command line); five mutations
  each fail a test. host-desktop tests (new macOS test for the pointer rule).
- macOS: workspace fmt, strict workspace clippy, host-desktop and bitmap-editor tests, iOS
  check of host-desktop. dolores (Linux): fmt, CI-identical clippy, host-desktop and
  bitmap-editor tests (ffmpeg present there, so the video test ran).
- By hand, against ffmpeg's decode: whole frames equal for PNG, BMP, TIFF, three GIF frames
  and MP4 frames 0, 5, 9; JPEG within 3 (`reviews/2026-10-14-bitmap-editor/`).
- Window: macOS loads at once, idles at 6-8 context switches per 10 s with no input;
  dolores loads an MP4 in 0.33 s with no input (the old Linux host: not within 15 s) and
  idles at 0 context switches per 10 s.
- CI 38509507447 for a12b9c64 passed on all four platforms (macOS, Linux x86-64 and arm64,
  Windows).
- Not run before hand-over: this check (the submitter's run is its first).

## Not done

- Paste (loadngo hosts have a text clipboard only), the C++ right-click blue hack, Refresh,
  About. Writing video; multi-page TIFF (first page only); colour profiles.
- Windows not run by hand (CI only); iOS/Android not tried.
- Step 7 measures context switches with `top` while the window is in front: someone
  moving the pointer over that window during the 10 s makes it run frames (hover), which
  would count against it.

## Disk at hand-over

- Mac internal disk 173 GiB free; Zhoenus II 909 GiB free; dolores 105 GiB free.
