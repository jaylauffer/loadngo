use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};

/// Basic component abstraction that mirrors the legacy CComponent responsibilities.
pub trait Component {
    fn hwnd(&self) -> HWND;
    fn bounds(&self) -> RECT;
    fn set_bounds(&mut self, rect: RECT);

    /// Handle a window message; return non-zero if handled.
    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;

    /// Optional focus notifications.
    fn focus_changed(&mut self, _gained: bool) {}

    /// Simple hit-test against the component's RECT.
    fn hit_test(&self, pt: POINT) -> bool {
        let rc = self.bounds();
        pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
    }
}
