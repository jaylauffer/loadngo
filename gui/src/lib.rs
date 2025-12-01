//! Minimal GUI base layer extracted from the legacy loadngoGUI components.
//! This crate provides lightweight wrappers for HWND-backed components,
//! containers, simple buttons, and bitmap loading. It is intentionally thin
//! scaffolding to support higher-level Day/Project planner windows.

pub mod util;
pub mod component;
pub mod container;
pub mod window;
pub mod bitmap;
pub mod button;
pub mod buffered;
pub mod list;
pub mod tabs;

pub use component::Component;
pub use container::Container;
pub use window::HostWindow;
pub use bitmap::Bitmap;
pub use button::Button;
pub use buffered::{BufferedWnd, ImgBuffer, WM_INVALIDATE};
pub use list::{ListBox, ListCombo};
pub use tabs::{TabbedContainer, TabPage};
