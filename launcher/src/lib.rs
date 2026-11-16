//! The loadngo launcher: one window listing what can be started on this
//! machine (programs, pages a program serves, links), read from a catalog
//! file. `docs/LAUNCHER.md` has the catalog's format and how the launcher
//! is installed.

pub mod app;
pub mod catalog;
pub mod draw;
pub mod process;

pub use app::{Effect, LauncherApp, State};
