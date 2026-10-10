//! A multi-file code editor built on loadngo. See `docs/CODE_EDITOR.md`.
//!
//! The library holds the editor's state and logic with no window: the
//! `code_editor` binary connects it to `loadngo-host-desktop` and runs its
//! file work on the host's offload workers.

pub mod draw;
pub mod editor;
pub mod file_tree;
pub mod fs_ops;
pub mod session;
pub mod text_file;
pub mod theme;

pub use editor::{Editor, EditorHost, FrameOutcome};
pub use fs_ops::{perform, IoRequest, IoResponse};
