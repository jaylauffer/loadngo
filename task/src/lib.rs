//! loadngo Task: a day plan and a project plan, ported from the C++ Task.
//!
//! This library is the window with no platform in it, on `ui-core`; the
//! `task` binary runs it on `loadngo-host-desktop` (macOS, Linux) with file
//! work on the host's offload workers. Windows still runs the Win32 port in
//! `main.rs`. See `docs/TASK_APP.md`.

pub mod app;
pub mod clock;
mod draw;
pub mod plan;
pub mod store;
pub mod tree_layout;

pub use app::{FrameOutcome, Tab, TaskApp, TaskHost};
