use std::sync::{Arc, Mutex};

use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetStockObject, OffsetViewportOrgEx, Rectangle, SelectObject, SetBkMode, SetDCBrushColor,
    SetDCPenColor, SetViewportOrgEx, DC_BRUSH, DC_PEN, HBRUSH, HDC, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetScrollInfo, GetWindowLongPtrW, LoadCursorW,
    MoveWindow, RegisterClassExW, SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWL_USERDATA, GetSystemMetrics, HMENU, IDC_ARROW, SCROLLINFO,
    SCROLLBAR_COMMAND, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS, SM_CXDRAG, SM_CYDRAG,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SIZE, WM_VSCROLL, WNDCLASSEXW,
    WNDCLASS_STYLES, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOPARENTNOTIFY, WS_VSCROLL,
    WS_VISIBLE,
};

use crate::buffered::BufferedWnd;
use crate::component::Component;
use crate::util::to_wstring;

const WM_MOUSELEAVE: u32 = 0x02A3;

pub trait ListBoxItem: Send {
    fn draw(&self, dc: HDC, width: i32, height: i32, highlighted: bool) -> i32;
    fn set_bounds(&mut self, rect: RECT);
    fn bounds(&self) -> RECT;
    fn contains(&self, pt: POINT) -> bool {
        let rc = self.bounds();
        pt.x >= rc.left && pt.x <= rc.right && pt.y >= rc.top && pt.y <= rc.bottom
    }
}

pub struct ListBox {
    hwnd: HWND,
    bounds: RECT,
    items: Arc<Mutex<Vec<Box<dyn ListBoxItem>>>>,
    buffer: BufferedWnd,
    hilite: i32,
    selected: Option<usize>,
    visible_pos: i32,
    visible_count: i32,
    tracking: bool,
    drag_start: Option<POINT>,
    drag_index: Option<usize>,
    drag_handler: Option<Arc<dyn Fn(usize)>>,
}

impl ListBox {
    pub fn create(parent: HWND) -> Result<Box<Self>> {
        unsafe {
            let class = to_wstring("LNGListBox");
            let hinstance = GetModuleHandleW(None)?;
            static mut REGISTERED: bool = false;
            if !REGISTERED {
                let mut wc = WNDCLASSEXW::default();
                wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
                wc.style = WNDCLASS_STYLES(CS_HREDRAW.0 | CS_VREDRAW.0);
                wc.lpfnWndProc = Some(Self::wndproc);
                wc.hInstance = hinstance.into();
                wc.hCursor = LoadCursorW(None, IDC_ARROW)?;
                wc.hbrBackground = HBRUSH(GetStockObject(DC_BRUSH).0);
                wc.lpszClassName = PCWSTR(class.as_ptr());
                RegisterClassExW(&wc);
                REGISTERED = true;
            }
            let host = Box::new(ListBox {
                hwnd: HWND::default(),
                bounds: RECT { left: 0, top: 0, right: 200, bottom: 200 },
                items: Arc::new(Mutex::new(Vec::new())),
                buffer: BufferedWnd::new(),
                hilite: -1,
                selected: None,
                visible_pos: 0,
                visible_count: 0,
                tracking: false,
                drag_start: None,
                drag_index: None,
                drag_handler: None,
            });
            let ptr = Box::into_raw(host);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_NOPARENTNOTIFY.0),
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(
                    WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0 | WS_VSCROLL.0,
                ),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                200,
                200,
                parent,
                HMENU(std::ptr::null_mut()),
                hinstance,
                Some(ptr as *mut _),
            )?;
            unsafe {
                (*ptr).hwnd = hwnd;
            }
            Ok(unsafe { Box::from_raw(ptr) })
        }
    }

    pub fn set_items(&mut self, items: Vec<Box<dyn ListBoxItem>>) {
        if let Ok(mut guard) = self.items.lock() {
            *guard = items;
        }
        self.visible_pos = 0;
        self.hilite = -1;
        self.selected = None;
        self.drag_start = None;
        self.drag_index = None;
        self.invalidate();
    }

    pub fn set_drag_handler(&mut self, handler: Option<Arc<dyn Fn(usize)>>) {
        self.drag_handler = handler;
    }

    fn invalidate(&self) {
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(self.hwnd, None, false);
        }
    }

    fn paint(&mut self) {
        let self_ptr = self as *mut ListBox;
        let _ = self.buffer.paint(self.hwnd, |_, dc, width, height| unsafe {
            unsafe {
                SelectObject(dc, GetStockObject(DC_BRUSH));
                SelectObject(dc, GetStockObject(DC_PEN));
                SetDCBrushColor(dc, COLORREF(0x00d3efc7));
                SetDCPenColor(dc, COLORREF(0x00d3efc7));
                Rectangle(dc, 0, 0, width, height);
                SetBkMode(dc, TRANSPARENT);
            }
            let this = &mut *self_ptr;
            this.draw_items(dc, width, height);
            Ok(())
        });
    }

    fn draw_items(&mut self, dc: HDC, width: i32, height: i32) {
        let mut y = 5;
        let mut orig = POINT { x: 0, y: 0 };
        unsafe {
            let _ = OffsetViewportOrgEx(dc, 6, y, Some(&mut orig));
        }
        if let Ok(mut items) = self.items.lock() {
            let count = items.len() as i32;
            self.visible_count = 0;
            for (i, item) in items
                .iter_mut()
                .enumerate()
                .skip(self.visible_pos as usize)
            {
                if y > height {
                    break;
                }
                let i = i as i32;
                let h = item.draw(dc, width - 6, height - y, self.hilite == i);
                item.set_bounds(RECT { left: 0, top: y, right: width, bottom: y + h });
                y += h;
                unsafe { let _ = OffsetViewportOrgEx(dc, 0, h, None); }
                self.visible_count += 1;
            }
            unsafe { let _ = SetViewportOrgEx(dc, orig.x, orig.y, None); }
            self.update_scroll(count);
        }
    }

    fn update_scroll(&self, total: i32) {
        let mut si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_POS | SIF_RANGE | SIF_PAGE,
            nMin: 0,
            nMax: (total - 1).max(0),
            nPage: self.visible_count.max(1) as u32,
            nPos: self.visible_pos,
            ..Default::default()
        };
        unsafe { let _ = SetScrollInfo(self.hwnd, windows::Win32::UI::WindowsAndMessaging::SB_VERT, &si, true); }
    }

    fn hit_test(&self, pt: POINT) -> Option<usize> {
        if let Ok(items) = self.items.lock() {
            for (i, item) in items.iter().enumerate() {
                if item.contains(pt) {
                    return Some(i);
                }
            }
        }
        None
    }

    fn handle_mouse_move(&mut self, lparam: LPARAM) {
        if let (Some(start), Some(idx)) = (self.drag_start, self.drag_index) {
            let pt = POINT {
                x: (lparam.0 & 0xffff) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
            };
            let drag_x = unsafe { GetSystemMetrics(SM_CXDRAG) };
            let drag_y = unsafe { GetSystemMetrics(SM_CYDRAG) };
            if (pt.x - start.x).abs() > drag_x || (pt.y - start.y).abs() > drag_y {
                if let Some(handler) = self.drag_handler.as_ref() {
                    handler(idx);
                }
                self.drag_start = None;
                self.drag_index = None;
            }
        }
        self.start_tracking();
        let pt = POINT {
            x: (lparam.0 & 0xffff) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        };
        let hit = self.hit_test(pt).map(|i| i as i32).unwrap_or(-1);
        if hit != self.hilite {
            self.hilite = hit;
            self.invalidate();
        }
    }

    fn handle_mouse_leave(&mut self) {
        if self.hilite != -1 {
            self.hilite = -1;
            self.invalidate();
        }
        self.tracking = false;
    }

    fn start_tracking(&mut self) {
        if self.tracking {
            return;
        }
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
            self.tracking = true;
        }
    }

    fn scroll(&mut self, delta: i32) {
        let mut si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_POS | SIF_RANGE | SIF_PAGE | SIF_TRACKPOS,
            ..Default::default()
        };
        unsafe { let _ = GetScrollInfo(self.hwnd, windows::Win32::UI::WindowsAndMessaging::SB_VERT, &mut si); }
        let mut pos = self.visible_pos + delta;
        let max = si.nMax.saturating_sub(self.visible_count.max(1) as i32) + 1;
        if pos < si.nMin {
            pos = si.nMin;
        } else if pos > max {
            pos = max;
        }
        self.visible_pos = pos;
        si.fMask = SIF_POS;
        si.nPos = pos;
        unsafe { let _ = SetScrollInfo(self.hwnd, windows::Win32::UI::WindowsAndMessaging::SB_VERT, &si, true); }
        self.invalidate();
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

    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_PAINT => {
                self.paint();
                LRESULT(1)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                let mut rc = RECT::default();
                unsafe { let _ = GetClientRect(self.hwnd, &mut rc); }
                self.bounds = rc;
                self.invalidate();
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                self.handle_mouse_move(lparam);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                self.handle_mouse_leave();
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                let lines = -delta / 120;
                self.scroll(lines);
                LRESULT(0)
            }
            WM_VSCROLL => {
                use windows::Win32::UI::WindowsAndMessaging::*;
                let code = SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32);
                match code {
                    SB_LINEUP => self.scroll(-1),
                    SB_LINEDOWN => self.scroll(1),
                    SB_PAGEUP => self.scroll(-self.visible_count.max(1)),
                    SB_PAGEDOWN => self.scroll(self.visible_count.max(1)),
                    SB_THUMBTRACK => {
                        let pos = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                        self.visible_pos = pos;
                        self.invalidate();
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                if let Some(idx) = self.hit_test(pt) {
                    self.selected = Some(idx);
                    self.hilite = idx as i32;
                    self.drag_start = Some(pt);
                    self.drag_index = Some(idx);
                    self.invalidate();
                } else {
                    self.drag_start = None;
                    self.drag_index = None;
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                self.drag_start = None;
                self.drag_index = None;
                LRESULT(0)
            }
            _ => LRESULT(0),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl ListBox {
    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                let ptr = cs.lpCreateParams as *mut ListBox;
                if !ptr.is_null() {
                    (*ptr).hwnd = hwnd;
                    SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
                }
                LRESULT(0)
            }
            WM_DESTROY => LRESULT(0),
            _ => {
                let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut ListBox;
                if !ptr.is_null() {
                    let lb = &mut *ptr;
                    lb.handle_message(msg, wparam, lparam)
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
        }
    }
}

/// Thin wrapper over the Win32 COMBOBOX control (unchanged).
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
            let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                self.hwnd,
                windows::Win32::UI::WindowsAndMessaging::CB_ADDSTRING,
                WPARAM(0),
                LPARAM(w.as_ptr() as isize),
            );
        }
    }

    pub fn clear(&self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SendMessageW(
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
