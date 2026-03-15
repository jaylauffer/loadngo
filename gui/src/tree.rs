use anyhow::Result;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Controls::{
    HTREEITEM, TVGN_CARET, TVINSERTSTRUCTW, TVI_ROOT, TVM_INSERTITEMW, TVM_SELECTITEM,
    TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, MoveWindow, SendMessageW, HMENU, WINDOW_EX_STYLE, WINDOW_STYLE, WM_KEYDOWN,
    WM_LBUTTONUP, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::util::{key_from_wparam, point_from_lparam, pointer_released_event, to_wstring};
use gui_win32::component::HostedComponent;
use ui_core::component::Component;

/// Minimal TreeView wrapper.
pub struct NativeTreeControl {
    hwnd: HWND,
    widget: ui_core::TreeControl,
    item_handles: Vec<(Vec<usize>, isize)>,
}

impl NativeTreeControl {
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
                widget: ui_core::TreeControl::new(ui_core::Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 200,
                }),
                item_handles: Vec::new(),
            })
        }
    }

    pub fn insert_root(&mut self, text: &str) -> isize {
        let root_index = self.widget.push_root(text);
        let handle = self.insert_item(text, TVI_ROOT.0 as isize);
        self.item_handles.push((vec![root_index], handle));
        handle
    }

    pub fn insert_child(&mut self, parent: isize, text: &str) -> isize {
        let handle = self.insert_item(text, parent);
        if let Some((path, _)) = self
            .item_handles
            .iter()
            .find(|(_, handle_value)| *handle_value == parent)
        {
            if path.len() == 1 {
                let root_index = path[0];
                if self.widget.push_child(root_index, text) {
                    let child_index = self.widget.roots[root_index].children.len() - 1;
                    self.item_handles
                        .push((vec![root_index, child_index], handle));
                }
            }
        }
        handle
    }

    fn insert_item(&self, text: &str, parent: isize) -> isize {
        let mut w = to_wstring(text);
        let mut insert = TVINSERTSTRUCTW::default();
        insert.hParent = HTREEITEM(parent as _);
        insert.hInsertAfter = TVI_ROOT;
        insert.Anonymous.itemex.pszText = PWSTR(w.as_mut_ptr());
        unsafe {
            SendMessageW(
                self.hwnd,
                TVM_INSERTITEMW,
                WPARAM(0),
                LPARAM(&insert as *const _ as isize),
            )
            .0
        }
    }

    fn sync_selection(&self) {
        let Some(path) = self.widget.selected_path.as_ref() else {
            return;
        };
        let Some((_, handle)) = self
            .item_handles
            .iter()
            .find(|(item_path, _)| item_path == path)
        else {
            return;
        };
        unsafe {
            let _ = SendMessageW(
                self.hwnd,
                TVM_SELECTITEM,
                WPARAM(TVGN_CARET as usize),
                LPARAM(*handle),
            );
        }
    }
}

impl Component for NativeTreeControl {
    fn bounds(&self) -> ui_core::Rect {
        self.widget.bounds
    }

    fn set_bounds(&mut self, rect: ui_core::Rect) {
        self.widget.set_bounds(rect);
        let rect = crate::util::rect_from_core(rect);
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        unsafe {
            let _ = MoveWindow(self.hwnd, rect.left, rect.top, w, h, true);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl HostedComponent for NativeTreeControl {
    fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_LBUTTONUP => {
                let response = self
                    .widget
                    .handle_event(pointer_released_event(point_from_lparam(lparam)));
                if response.request_redraw {
                    self.sync_selection();
                    return LRESULT(1);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if let Some(key) = key_from_wparam(wparam.0) {
                    let response = self.widget.handle_event(ui_core::UiEvent::KeyPressed {
                        key,
                        modifiers: Default::default(),
                    });
                    if response.request_redraw {
                        self.sync_selection();
                        return LRESULT(1);
                    }
                }
                LRESULT(0)
            }
            _ => LRESULT(0),
        }
    }
}

/// Simple combo wrapper that uses a LISTBOX-style drop-down, not a real tree combo.
pub struct NativeTreeCombo {
    hwnd: HWND,
    widget: ui_core::TreeCombo,
}

impl NativeTreeCombo {
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
                widget: ui_core::TreeCombo::new(ui_core::Rect {
                    x: 0,
                    y: 0,
                    width: 160,
                    height: 26,
                }),
            })
        }
    }
}

impl Component for NativeTreeCombo {
    fn bounds(&self) -> ui_core::Rect {
        self.widget.bounds
    }
    fn set_bounds(&mut self, rect: ui_core::Rect) {
        self.widget.set_bounds(rect);
        let rect = crate::util::rect_from_core(rect);
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        unsafe {
            let _ = MoveWindow(self.hwnd, rect.left, rect.top, w, h, true);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl HostedComponent for NativeTreeCombo {
    fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_LBUTTONUP => {
                let response = self
                    .widget
                    .handle_event(pointer_released_event(point_from_lparam(lparam)));
                if response.request_redraw {
                    return LRESULT(1);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if let Some(key) = key_from_wparam(wparam.0) {
                    let response = self.widget.handle_event(ui_core::UiEvent::KeyPressed {
                        key,
                        modifiers: Default::default(),
                    });
                    if response.request_redraw {
                        return LRESULT(1);
                    }
                }
                LRESULT(0)
            }
            _ => LRESULT(0),
        }
    }
}
