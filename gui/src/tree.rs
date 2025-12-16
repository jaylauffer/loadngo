use anyhow::Result;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Controls::{
    HTREEITEM, TVINSERTSTRUCTW, TVI_ROOT, TVM_INSERTITEMW, TVS_HASBUTTONS, TVS_HASLINES,
    TVS_LINESATROOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, MoveWindow, SendMessageW, HMENU, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD,
    WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::component::Component;
use crate::util::to_wstring;

/// Minimal TreeView wrapper.
pub struct TreeControl {
    hwnd: HWND,
    bounds: RECT,
}

impl TreeControl {
    pub fn create(parent: HWND) -> Result<Self> {
        unsafe {
            let cls = to_wstring("SysTreeView32");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(cls.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(
                    WS_CHILD.0
                        | WS_VISIBLE.0
                        | WS_CLIPSIBLINGS.0
                        | TVS_HASBUTTONS
                        | TVS_HASLINES
                        | TVS_LINESATROOT,
                ),
                0,
                0,
                200,
                200,
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
                    right: 200,
                    bottom: 200,
                },
            })
        }
    }

    pub fn insert_root(&self, text: &str) {
        self.insert_item(text, TVI_ROOT.0 as isize);
    }

    pub fn insert_child(&self, parent: isize, text: &str) {
        self.insert_item(text, parent);
    }

    fn insert_item(&self, text: &str, parent: isize) {
        let mut w = to_wstring(text);
        let mut insert = TVINSERTSTRUCTW::default();
        insert.hParent = HTREEITEM(parent as _);
        insert.hInsertAfter = TVI_ROOT;
        insert.Anonymous.itemex.pszText = PWSTR(w.as_mut_ptr());
        unsafe {
            let _ = SendMessageW(
                self.hwnd,
                TVM_INSERTITEMW,
                WPARAM(0),
                LPARAM(&insert as *const _ as isize),
            );
        }
    }
}

impl Component for TreeControl {
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

/// Simple combo wrapper that uses a LISTBOX-style drop-down, not a real tree combo.
pub struct TreeCombo {
    hwnd: HWND,
    bounds: RECT,
}

impl TreeCombo {
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
                160,
                26,
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
                    right: 160,
                    bottom: 26,
                },
            })
        }
    }
}

impl Component for TreeCombo {
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
