//! Platform-agnostic GUI facade for loadngo.
//! Shared widget state and paint/event primitives live in `ui-core`.
//! Platform-specific adapters are re-exported behind backend shims.

pub use ui_core::{
    bitmap::{Bitmap, BitmapModel},
    button::{Button, ButtonModel},
    combo::ListCombo,
    component::Component,
    geometry::{Color, Insets, Point, Rect, Size},
    input::{Key, Modifiers, PointerButton, PointerState, UiEvent},
    label::{Label, LabelModel},
    list::{ListInteraction, ListState},
    list_row::{ListRow, ListRowModel},
    paint::{HorizontalAlign, PaintOp, TextLayoutMode, TextOverflow, TextStyle, VerticalAlign},
    panel::{Panel, PanelModel},
    scroll_container::{ScrollContainer, ScrollContainerModel},
    stack::{VerticalStack, VerticalStackModel},
    tabs::{TabGroupModel, TabPage, TabbedContainer},
    text_block::{TextBlock, TextBlockModel},
    tree::{TreeCombo, TreeControl, TreeNode},
    widget::{WidgetId, WidgetResponse},
    workspace::{
        WorkspaceLeaf, WorkspaceLeafView, WorkspaceNode, WorkspaceSplitNode, WorkspaceTabGroup,
        WorkspaceTabPage,
    },
};

pub mod button;
pub mod component;

#[cfg(windows)]
pub mod basic_button {
    pub use gui_win32::basic_button::*;
}
#[cfg(windows)]
pub mod bitmap {
    pub use gui_win32::bitmap::*;
}
#[cfg(windows)]
pub mod buffered {
    pub use gui_win32::buffered::*;
}
#[cfg(windows)]
pub mod container {
    pub use gui_win32::container::*;
}
#[cfg(windows)]
pub mod container_host {
    pub use gui_win32::container_host::*;
}
#[cfg(windows)]
pub mod event {
    pub use gui_win32::event::*;
}
#[cfg(windows)]
pub mod event_proc {
    pub use gui_win32::event_proc::*;
}
#[cfg(windows)]
pub mod list {
    pub use gui_win32::list::*;
}
#[cfg(windows)]
pub mod listener {
    pub use gui_win32::listener::*;
}
#[cfg(windows)]
pub mod tabs {
    pub use gui_win32::tabs::*;
}
#[cfg(windows)]
pub mod tree {
    pub use gui_win32::tree::*;
}
#[cfg(windows)]
pub mod util {
    pub use gui_win32::util::*;
}
#[cfg(windows)]
pub mod window {
    pub use gui_win32::window::*;
}

#[cfg(windows)]
pub use gui_win32::{
    BasicButton, BufferedWnd, ComponentEvent, ComponentEventProc, ComponentListener, Container,
    ContainerHost, HitTrackButton, HostWindow, HostedComponent, ImgBuffer, ListBox, NativeBitmap,
    NativeButton, NativeListCombo, NativeTabPage, NativeTabbedContainer, NativeTreeCombo,
    NativeTreeControl, WM_INVALIDATE,
};
