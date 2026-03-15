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

use crate::util::{key_from_wparam, point_from_lparam, pointer_released_event, to_wstring};
use gui_win32::component::HostedComponent;
use ui_core::component::Component;

pub struct NativeTabPage {
    pub title_w: Vec<u16>,
    pub hwnd: HWND,
}

/// Minimal tab host using the common controls tab control.
pub struct NativeTabbedContainer {
    pub hwnd: HWND,
    pages: Vec<NativeTabPage>,
    widget: ui_core::TabbedContainer,
}

impl NativeTabbedContainer {
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
                widget: ui_core::TabbedContainer::new(ui_core::Rect {
                    x: 0,
                    y: 0,
                    width: 200,
                    height: 200,
                }),
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
        self.widget.add_page(title, None);
        self.pages.push(NativeTabPage { title_w, hwnd });
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
            let show = idx == self.widget.selected;
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

    fn sync_selection(&self) {
        unsafe {
            let _ = SendMessageW(
                self.hwnd,
                TCM_SETCURSEL,
                WPARAM(self.widget.selected),
                LPARAM(0),
            );
        }
        self.layout_pages();
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

impl Component for NativeTabbedContainer {
    fn bounds(&self) -> ui_core::Rect {
        self.widget.bounds
    }

    fn set_bounds(&mut self, rect: ui_core::Rect) {
        self.widget.set_bounds(rect);
        let rect = crate::util::rect_from_core(rect);
        self.set_bounds(rect);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl HostedComponent for NativeTabbedContainer {
    fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            windows::Win32::UI::WindowsAndMessaging::WM_SIZE => {
                self.layout_pages();
                LRESULT(1)
            }
            windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONUP => {
                let response = self
                    .widget
                    .handle_event(pointer_released_event(point_from_lparam(lparam)));
                if response.request_redraw {
                    self.sync_selection();
                    return LRESULT(1);
                }
                LRESULT(0)
            }
            windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN => {
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
