use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, MoveWindow, HMENU, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_CLIPSIBLINGS,
    WS_VISIBLE,
};

use crate::component::Component;
use crate::util::to_wstring;

/// Simple HWND-backed push button implementing the Component trait.
pub struct Button {
    hwnd: HWND,
    bounds: RECT,
}

impl Button {
    pub fn create(parent: HWND, text: &str) -> Result<Self> {
        unsafe {
            let class = to_wstring("BUTTON");
            let text_w = to_wstring(text);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(text_w.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
                0,
                0,
                80,
                24,
                parent,
                HMENU(std::ptr::null_mut()),
                None,
                None,
            )?;
            Ok(Self {
                hwnd,
                bounds: RECT {
                    left: 0,
                    top: 0,
                    right: 80,
                    bottom: 24,
                },
            })
        }
    }
}

impl Component for Button {
    fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn bounds(&self) -> RECT {
        self.bounds
    }

    fn set_bounds(&mut self, rect: RECT) {
        self.bounds = rect;
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        unsafe {
            let _ = MoveWindow(self.hwnd, rect.left, rect.top, w, h, true);
        }
    }

    fn handle_message(&mut self, _msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
        LRESULT(0)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
