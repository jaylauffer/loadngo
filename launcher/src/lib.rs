//! The loadngo launcher: one window listing what can be started on this
//! machine (programs, pages a program serves, links), read from a catalog
//! file. `docs/LAUNCHER.md` has the catalog's format and how the launcher
//! is installed. On a phone it opens links and starts nothing
//! ([`Abilities::PHONE`]).

pub mod app;
pub mod catalog;
pub mod draw;
pub mod process;

pub use app::{Abilities, Edges, Effect, LauncherApp, State};
