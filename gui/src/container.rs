use crate::component::Component;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MoveWindow, WM_CAPTURECHANGED, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE,
};

const WM_MOUSELEAVE_CONST: u32 = 0x02A3;

/// Simple container that owns child Components and forwards messages.
pub struct Container {
    pub hwnd: HWND,
    pub children: Vec<Box<dyn Component>>,
    focus_idx: Option<usize>,
    hover_idx: Option<usize>,
    capturing_idx: Option<usize>,
    tracking_mouse: bool,
}

impl Container {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            children: Vec::new(),
            focus_idx: None,
            hover_idx: None,
            capturing_idx: None,
            tracking_mouse: false,
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
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    /// Dispatch a message to children until handled.
    pub fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_MOUSEMOVE => self.handle_mouse_move(wparam, lparam),
            WM_LBUTTONDOWN | WM_LBUTTONUP => self.handle_mouse_button(msg, wparam, lparam),
            WM_KEYDOWN | WM_KEYUP | WM_CHAR => self.forward_to_focus(msg, wparam, lparam),
            WM_MOUSELEAVE_CONST => self.handle_mouse_leave(),
            WM_CAPTURECHANGED => {
                self.capturing_idx = None;
                LRESULT(0)
            }
            _ => self.forward_first(msg, wparam, lparam),
        }
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
            unsafe {
                let _ = SetFocus(c.hwnd());
            }
        }
    }

    fn forward_first(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        for (idx, child) in self.children.iter_mut().enumerate() {
            let res = child.handle_message(msg, wparam, lparam);
            if res.0 != 0 {
                self.focus_idx = Some(idx);
                return res;
            }
        }
        LRESULT(0)
    }

    fn forward_to_focus(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if let Some(idx) = self.focus_idx {
            if let Some(child) = self.children.get_mut(idx) {
                return child.handle_message(msg, wparam, lparam);
            }
        }
        LRESULT(0)
    }

    fn handle_mouse_move(&mut self, _wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking_mouse = unsafe { TrackMouseEvent(&mut tme).is_ok() };
        }
        let pt = self.to_client_point(lparam);
        if let Some(idx) = self.capturing_idx {
            let lp = self.repack_lparam(pt);
            if let Some(child) = self.children.get_mut(idx) {
                return child.handle_message(WM_MOUSEMOVE, WPARAM(0), lp);
            }
        }
        let hit_idx = self.hit_test(pt);
        if hit_idx != self.hover_idx {
            if let Some(prev) = self.hover_idx {
                if let Some(c) = self.children.get_mut(prev) {
                    c.mouse_exited();
                }
            }
            if let Some(new_idx) = hit_idx {
                if let Some(c) = self.children.get_mut(new_idx) {
                    c.mouse_entered();
                }
            }
            self.hover_idx = hit_idx;
        }
        if let Some(idx) = hit_idx {
            let lp = self.repack_lparam(pt);
            if let Some(child) = self.children.get_mut(idx) {
                return child.handle_message(WM_MOUSEMOVE, WPARAM(0), lp);
            }
        }
        LRESULT(0)
    }

    fn handle_mouse_button(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let pt = self.to_client_point(lparam);
        if let Some(idx) = self.hit_test(pt) {
            self.focus_child(idx);
            if msg == WM_LBUTTONDOWN {
                self.capturing_idx = Some(idx);
                unsafe {
                    let _ = SetCapture(self.hwnd);
                }
            }
            let lp = self.repack_lparam(pt);
            if let Some(child) = self.children.get_mut(idx) {
                let res = child.handle_message(msg, wparam, lp);
                if msg == WM_LBUTTONUP {
                    self.capturing_idx = None;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                return res;
            }
        }
        LRESULT(0)
    }

    fn handle_mouse_leave(&mut self) -> LRESULT {
        if let Some(prev) = self.hover_idx.take() {
            if let Some(c) = self.children.get_mut(prev) {
                c.mouse_exited();
            }
        }
        self.tracking_mouse = false;
        LRESULT(0)
    }

    fn hit_test(&self, pt: POINT) -> Option<usize> {
        self.children.iter().position(|c| c.hit_test(pt))
    }

    fn to_client_point(&self, lparam: LPARAM) -> POINT {
        POINT {
            x: (lparam.0 & 0xffff) as i32 as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i32 as i16 as i32,
        }
    }

    fn repack_lparam(&self, pt: POINT) -> LPARAM {
        let packed = ((pt.y as u32) << 16) | (pt.x as u32 & 0xffff);
        LPARAM(packed as isize)
    }
}
