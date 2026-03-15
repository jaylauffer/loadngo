use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, MoveWindow, HMENU, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_CLIPSIBLINGS,
    WS_VISIBLE,
};

use crate::component::HostedComponent;
use crate::util::{rect_from_core, rect_to_core, to_wstring};

/// Simple HWND-backed push button host.
pub struct NativeButton {
    hwnd: HWND,
    bounds: RECT,
}

impl NativeButton {
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

impl ui_core::Component for NativeButton {
    fn bounds(&self) -> ui_core::Rect {
        rect_to_core(self.bounds)
    }

    fn set_bounds(&mut self, rect: ui_core::Rect) {
        self.bounds = rect_from_core(rect);
        let w = self.bounds.right - self.bounds.left;
        let h = self.bounds.bottom - self.bounds.top;
        unsafe {
            let _ = MoveWindow(self.hwnd, self.bounds.left, self.bounds.top, w, h, true);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl HostedComponent for NativeButton {
    fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn handle_message(&mut self, _msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
        LRESULT(0)
    }
}
