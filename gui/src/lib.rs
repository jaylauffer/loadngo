//! Minimal GUI base layer extracted from the legacy loadngoGUI components.
//! This crate provides lightweight wrappers for HWND-backed components,
//! containers, simple buttons, and bitmap loading. It is intentionally thin
//! scaffolding to support higher-level Day/Project planner windows.

pub mod util;
pub mod component;
pub mod container;
pub mod event;
pub mod listener;
pub mod event_proc;
pub mod window;
pub mod bitmap;
pub mod button;
pub mod basic_button;
pub mod buffered;
pub mod list;
pub mod tree;
pub mod tabs;

pub use component::Component;
pub use container::Container;
pub use event::ComponentEvent;
pub use event_proc::ComponentEventProc;
pub use listener::ComponentListener;
pub use window::HostWindow;
pub use bitmap::Bitmap;
pub use button::Button;
pub use basic_button::{BasicButton, HitTrackButton};
pub use buffered::{BufferedWnd, ImgBuffer, WM_INVALIDATE};
pub use list::{ListBox, ListCombo};
pub use tree::{TreeCombo, TreeControl};
pub use tabs::{TabbedContainer, TabPage};
