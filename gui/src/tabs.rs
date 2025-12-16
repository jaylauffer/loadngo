use anyhow::Result;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Controls::TCITEMW;
use windows::Win32::UI::Controls::{TCIF_TEXT, TCM_INSERTITEMW, TCM_SETCURSEL, WC_TABCONTROLW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetClientRect, MoveWindow, SendMessageW, SetParent, ShowWindow, HMENU,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_VISIBLE,
};

use crate::component::Component;
use crate::util::to_wstring;

pub struct TabPage {
    pub title_w: Vec<u16>,
    pub hwnd: HWND,
}

/// Minimal tab host using the common controls tab control.
pub struct TabbedContainer {
    pub hwnd: HWND,
    pages: Vec<TabPage>,
    selected: usize,
}

impl TabbedContainer {
    pub fn create(parent: HWND) -> Result<Self> {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(WC_TABCONTROLW.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
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
                pages: Vec::new(),
                selected: 0,
            })
        }
    }

    pub fn add_page(&mut self, title: &str, hwnd: HWND) {
        unsafe {
            let _ = SetParent(hwnd, self.hwnd);
        }
        let mut title_w = to_wstring(title);
        let mut item = TCITEMW::default();
        item.mask = TCIF_TEXT;
        item.pszText = PWSTR(title_w.as_mut_ptr());
        unsafe {
            let _ = SendMessageW(
                self.hwnd,
                TCM_INSERTITEMW,
                WPARAM(self.pages.len()),
                LPARAM(&item as *const _ as isize),
            );
        }
        self.pages.push(TabPage { title_w, hwnd });
        if self.pages.len() == 1 {
            let _ = unsafe { SendMessageW(self.hwnd, TCM_SETCURSEL, WPARAM(0), LPARAM(0)) };
            let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
        } else {
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
        }
        self.layout_pages();
    }

    fn layout_pages(&self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        // Basic layout: stack pages to fill client area below tabs.
        let tab_height = 30; // approximation
        for (idx, page) in self.pages.iter().enumerate() {
            let show = idx == self.selected;
            unsafe {
                let _ = MoveWindow(
                    page.hwnd,
                    0,
                    tab_height,
                    rc.right - rc.left,
                    rc.bottom - rc.top - tab_height,
                    true,
                );
                let _ = ShowWindow(page.hwnd, if show { SW_SHOW } else { SW_HIDE });
            }
        }
    }

    pub fn set_bounds(&mut self, rect: RECT) {
        unsafe {
            let _ = MoveWindow(
                self.hwnd,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                true,
            );
        }
        self.layout_pages();
    }
}

impl Component for TabbedContainer {
    fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn bounds(&self) -> RECT {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        rc
    }

    fn set_bounds(&mut self, rect: RECT) {
        self.set_bounds(rect);
    }

    fn handle_message(&mut self, msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
        if msg == windows::Win32::UI::WindowsAndMessaging::WM_SIZE {
            self.layout_pages();
            LRESULT(1)
        } else {
            LRESULT(0)
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
