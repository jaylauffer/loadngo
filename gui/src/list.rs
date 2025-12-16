use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, MoveWindow, SendMessageW, HMENU, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD,
    WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::component::Component;
use crate::util::to_wstring;

const LBS_NOTIFY: u32 = 0x0001;
const LBS_NOINTEGRALHEIGHT: u32 = 0x0100;
const LB_ADDSTRING: u32 = 0x0180;
const LB_RESETCONTENT: u32 = 0x0184;

/// Thin wrapper over the Win32 LISTBOX control.
pub struct ListBox {
    hwnd: HWND,
    bounds: RECT,
}

impl ListBox {
    pub fn create(parent: HWND) -> Result<Self> {
        unsafe {
            let cls = to_wstring("LISTBOX");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(cls.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(
                    WS_CHILD.0
                        | WS_VISIBLE.0
                        | WS_CLIPSIBLINGS.0
                        | LBS_NOTIFY
                        | LBS_NOINTEGRALHEIGHT,
                ),
                0,
                0,
                120,
                120,
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
                    right: 120,
                    bottom: 120,
                },
            })
        }
    }

    pub fn add_item(&self, text: &str) {
        let w = to_wstring(text);
        unsafe {
            let _ = SendMessageW(
                self.hwnd,
                LB_ADDSTRING,
                WPARAM(0),
                LPARAM(w.as_ptr() as isize),
            );
        }
    }

    pub fn clear(&self) {
        unsafe {
            let _ = SendMessageW(self.hwnd, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        }
    }
}

impl Component for ListBox {
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

/// Thin wrapper over the Win32 COMBOBOX control.
pub struct ListCombo {
    hwnd: HWND,
    bounds: RECT,
}

impl ListCombo {
    pub fn create(parent: HWND) -> Result<Self> {
        unsafe {
            let cls = to_wstring("COMBOBOX");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(cls.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
                0,
                0,
                140,
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
                    right: 140,
                    bottom: 24,
                },
            })
        }
    }

    pub fn add_item(&self, text: &str) {
        let w = to_wstring(text);
        unsafe {
            let _ = SendMessageW(
                self.hwnd,
                windows::Win32::UI::WindowsAndMessaging::CB_ADDSTRING,
                WPARAM(0),
                LPARAM(w.as_ptr() as isize),
            );
        }
    }

    pub fn clear(&self) {
        unsafe {
            let _ = SendMessageW(
                self.hwnd,
                windows::Win32::UI::WindowsAndMessaging::CB_RESETCONTENT,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

impl Component for ListCombo {
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
