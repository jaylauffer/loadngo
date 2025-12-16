use std::any::Any;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};

/// Basic component abstraction that mirrors the legacy CComponent responsibilities.
pub trait Component: Any {
    fn hwnd(&self) -> HWND;
    fn bounds(&self) -> RECT;
    fn set_bounds(&mut self, rect: RECT);

    /// Handle a window message; return non-zero if handled.
    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;

    /// Optional focus notifications.
    fn focus_changed(&mut self, _gained: bool) {}

    /// Optional mouse enter/exit hooks.
    fn mouse_entered(&mut self) {}
    fn mouse_exited(&mut self) {}

    /// Optional drag-over hook (returns true if accepted).
    fn drag_over(&mut self, _pt: POINT) -> bool {
        false
    }

    /// Optional drop hook for a list of file paths; return true if handled.
    fn drop_files(&mut self, _files: &[String], _pt: POINT) -> bool {
        false
    }

    /// Component identifier (used by some listeners/command handlers).
    fn id(&self) -> i32 {
        0
    }

    /// For downcasting specific component types.
    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Simple hit-test against the component's RECT.
    fn hit_test(&self, pt: POINT) -> bool {
        let rc = self.bounds();
        pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
    }
}
