//! Minimal GUI base layer extracted from the legacy loadngoGUI components.
//! This crate provides lightweight wrappers for HWND-backed components,
//! containers, simple buttons, and bitmap loading. It is intentionally thin
//! scaffolding to support higher-level Day/Project planner windows.

pub mod basic_button;
pub mod bitmap;
pub mod buffered;
pub mod button;
pub mod component;
pub mod container;
pub mod container_host;
pub mod event;
pub mod event_proc;
pub mod list;
pub mod listener;
pub mod tabs;
pub mod tree;
pub mod util;
pub mod window;

pub use basic_button::{BasicButton, HitTrackButton};
pub use bitmap::Bitmap;
pub use buffered::{BufferedWnd, ImgBuffer, WM_INVALIDATE};
pub use button::Button;
pub use component::Component;
pub use container::Container;
pub use container_host::ContainerHost;
pub use event::ComponentEvent;
pub use event_proc::ComponentEventProc;
pub use list::{ListBox, ListCombo};
pub use listener::ComponentListener;
pub use tabs::{TabPage, TabbedContainer};
pub use tree::{TreeCombo, TreeControl};
pub use window::HostWindow;
