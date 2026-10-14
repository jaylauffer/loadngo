//! The BitmapEditor from loadngo-cpp (2004, Win32), ported to the loadngo
//! Rust framework: a picture at its own scale, the same picture as a grid of
//! cells one pixel each, a readout of the pixel under the mouse, and the C++
//! editor's alpha edits. It opens PNG, JPEG, BMP, TIFF and GIF itself and
//! video (MP4, MOV, WebM...) and other formats through ffmpeg, frame by
//! frame; file work runs on the host's offload workers. See
//! `docs/BITMAP_EDITOR.md`.

pub mod editor;
pub mod picture;
pub mod render;
pub mod video;
