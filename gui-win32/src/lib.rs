//! Win32 shim for the loadngo GUI facade.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

#[cfg(windows)]
#[path = "../../gui/src/basic_button.rs"]
pub mod basic_button;
#[cfg(windows)]
#[path = "../../gui/src/bitmap.rs"]
pub mod bitmap;
#[cfg(windows)]
#[path = "../../gui/src/buffered.rs"]
pub mod buffered;
#[cfg(windows)]
pub mod button;
#[cfg(windows)]
pub mod component;
#[cfg(windows)]
#[path = "../../gui/src/container.rs"]
pub mod container;
#[cfg(windows)]
#[path = "../../gui/src/container_host.rs"]
pub mod container_host;
#[cfg(windows)]
#[path = "../../gui/src/event.rs"]
pub mod event;
#[cfg(windows)]
#[path = "../../gui/src/event_proc.rs"]
pub mod event_proc;
#[cfg(windows)]
#[path = "../../gui/src/list.rs"]
pub mod list;
#[cfg(windows)]
#[path = "../../gui/src/listener.rs"]
pub mod listener;
#[cfg(windows)]
#[path = "../../gui/src/tabs.rs"]
pub mod tabs;
#[cfg(windows)]
#[path = "../../gui/src/tree.rs"]
pub mod tree;
#[cfg(windows)]
#[path = "../../gui/src/util.rs"]
pub mod util;
#[cfg(windows)]
#[path = "../../gui/src/window.rs"]
pub mod window;

#[cfg(windows)]
pub use basic_button::{BasicButton, HitTrackButton};
#[cfg(windows)]
pub use bitmap::Bitmap as NativeBitmap;
#[cfg(windows)]
pub use buffered::{BufferedWnd, ImgBuffer, WM_INVALIDATE};
#[cfg(windows)]
pub use button::NativeButton;
#[cfg(windows)]
pub use component::HostedComponent;
#[cfg(windows)]
pub use container::Container;
#[cfg(windows)]
pub use container_host::ContainerHost;
#[cfg(windows)]
pub use event::ComponentEvent;
#[cfg(windows)]
pub use event_proc::ComponentEventProc;
#[cfg(windows)]
pub use list::{ListBox, NativeListCombo};
#[cfg(windows)]
pub use listener::ComponentListener;
#[cfg(windows)]
pub use tabs::{NativeTabPage, NativeTabbedContainer};
#[cfg(windows)]
pub use tree::{NativeTreeCombo, NativeTreeControl};
#[cfg(windows)]
pub use window::HostWindow;
