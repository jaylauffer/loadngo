use crate::component::Component;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::MoveWindow;

/// Simple container that owns child Components and forwards messages.
pub struct Container {
    pub hwnd: HWND,
    pub children: Vec<Box<dyn Component>>,
    focus_idx: Option<usize>,
}

impl Container {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            children: Vec::new(),
            focus_idx: None,
        }
    }

    pub fn add(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    pub fn remove_by_hwnd(&mut self, hwnd: HWND) {
        self.children.retain(|c| c.hwnd() != hwnd);
    }

    /// Layout children horizontally with a small gap (placeholder layout).
    pub fn layout_horizontal(&mut self, start: POINT, gap: i32) {
        let mut x = start.x;
        let y = start.y;
        for child in self.children.iter_mut() {
            let mut rc = child.bounds();
            let w = rc.right - rc.left;
            let h = rc.bottom - rc.top;
            rc.left = x;
            rc.top = y;
            rc.right = x + w;
            rc.bottom = y + h;
            child.set_bounds(rc);
            unsafe {
                let _ = MoveWindow(child.hwnd(), rc.left, rc.top, w, h, true);
            }
            x += w + gap;
        }
        unsafe { let _ = InvalidateRect(self.hwnd, None, false); }
    }

    /// Dispatch a message to children until handled.
    pub fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        for (idx, child) in self.children.iter_mut().enumerate() {
            let res = child.handle_message(msg, wparam, lparam);
            if res.0 != 0 {
                self.focus_idx = Some(idx);
                return res;
            }
        }
        LRESULT(0)
    }

    pub fn focus_child(&mut self, idx: usize) {
        if let Some(prev) = self.focus_idx {
            if let Some(c) = self.children.get_mut(prev) {
                c.focus_changed(false);
            }
        }
        if let Some(c) = self.children.get_mut(idx) {
            c.focus_changed(true);
            self.focus_idx = Some(idx);
        }
    }
}
