use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DeleteObject, EndPaint, FillRect, GetStockObject, SelectObject, SetBkColor,
    SetTextColor, HBRUSH, HDC, PAINTSTRUCT, WHITE_BRUSH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, RegisterClassW,
    SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWL_USERDATA, HMENU,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_CREATE, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETFOCUS, WM_SIZE, WS_CHILD,
    WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::component::Component;
use crate::event::ComponentEvent;
use crate::event_proc::ComponentEventProc;
use crate::util::to_wstring;

const CLASS_NAME: &str = "LNGBasicButton";

/// Minimal reimplementation of CBasicButton with hover/press/focus states and event dispatch.
pub struct BasicButton {
    hwnd: HWND,
    bounds: RECT,
    pub id: i32,
    pub listeners: ComponentEventProc,
    state: ButtonState,
    text: String,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct ButtonState {
    pressed: bool,
    hover: bool,
    focus: bool,
}

impl BasicButton {
    pub fn register_class() {
        unsafe {
            let class = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(Self::wndproc),
                hInstance: windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                    .unwrap()
                    .into(),
                lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
                hbrBackground: HBRUSH(std::ptr::null_mut()),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);
        }
    }

    pub fn create(parent: HWND, id: i32, text: &str) -> Result<Self> {
        Self::register_class();
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
                0,
                0,
                80,
                26,
                parent,
                HMENU(std::ptr::null_mut()),
                None,
                None,
            )?;
            let mut btn = Self {
                hwnd,
                bounds: RECT {
                    left: 0,
                    top: 0,
                    right: 80,
                    bottom: 26,
                },
                id,
                listeners: ComponentEventProc::new(),
                state: ButtonState::default(),
                text: text.to_string(),
            };
            unsafe {
                SetWindowLongPtrW(hwnd, GWL_USERDATA, &mut btn as *mut _ as isize);
                ShowWindow(hwnd, SW_SHOW);
            }
            Ok(btn)
        }
    }

    fn notify_click(&self) {
        let evt = ComponentEvent::new(self, 0);
        self.listeners.notify(&evt);
    }

    fn paint(&self, dc: HDC) {
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            let bg = HBRUSH(GetStockObject(WHITE_BRUSH).0);
            let _ = FillRect(dc, &rc, bg);
            let _ = SetBkColor(dc, COLORREF(0x00f0f0f0));
            let _ = SetTextColor(dc, COLORREF(0x00202020));
            let old_font = SelectObject(dc, GetStockObject(windows::Win32::Graphics::Gdi::DEFAULT_GUI_FONT));
            let text = to_wstring(&self.text);
            let mut buf = text;
            if !buf.is_empty() {
                buf.pop();
            }
            let _ = windows::Win32::Graphics::Gdi::DrawTextW(
                dc,
                &mut buf,
                &mut rc,
                windows::Win32::Graphics::Gdi::DT_CENTER
                    | windows::Win32::Graphics::Gdi::DT_VCENTER
                    | windows::Win32::Graphics::Gdi::DT_SINGLELINE,
            );
            if self.state.hover || self.state.focus {
                let pen = windows::Win32::Graphics::Gdi::CreatePen(
                    windows::Win32::Graphics::Gdi::PS_SOLID,
                    1,
                    COLORREF(0x00707070),
                );
                let old_pen = SelectObject(dc, pen);
                let _ = windows::Win32::Graphics::Gdi::Rectangle(dc, rc.left, rc.top, rc.right, rc.bottom);
                let _ = SelectObject(dc, old_pen);
                let _ = DeleteObject(pen);
            }
            let _ = SelectObject(dc, old_font);
        }
    }

    unsafe fn state(hwnd: HWND) -> Option<&'static mut Self> {
        let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut Self;
        ptr.as_mut()
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                let ptr = cs.lpCreateParams as *mut Self;
                if !ptr.is_null() {
                    SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
                }
                LRESULT(0)
            }
            WM_SIZE => LRESULT(0),
            WM_MOUSEMOVE => {
                if let Some(s) = Self::state(hwnd) {
                    s.state.hover = true;
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(s) = Self::state(hwnd) {
                    s.state.pressed = true;
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(s) = Self::state(hwnd) {
                    let was_pressed = s.state.pressed;
                    s.state.pressed = false;
                    if was_pressed {
                        s.notify_click();
                    }
                }
                LRESULT(0)
            }
            WM_SETFOCUS => {
                if let Some(s) = Self::state(hwnd) {
                    s.state.focus = true;
                }
                LRESULT(0)
            }
            WM_KEYDOWN | WM_KEYUP | WM_CHAR => LRESULT(0),
            WM_PAINT => {
                if let Some(s) = Self::state(hwnd) {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut ps);
                    s.paint(dc);
                    let _ = EndPaint(hwnd, &ps);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

impl Component for BasicButton {
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
            let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                self.hwnd, rect.left, rect.top, w, h, true,
            );
        }
    }

    fn handle_message(&mut self, _msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
        LRESULT(0)
    }

    fn mouse_entered(&mut self) {
        self.state.hover = true;
    }

    fn mouse_exited(&mut self) {
        self.state.hover = false;
    }

    fn focus_changed(&mut self, gained: bool) {
        self.state.focus = gained;
    }

    fn id(&self) -> i32 {
        self.id
    }
}

/// Adds hover tracking border like CBasicHitTrackButton.
pub struct HitTrackButton(pub BasicButton);

impl HitTrackButton {
    pub fn create(parent: HWND, id: i32, text: &str) -> Result<Self> {
        Ok(Self(BasicButton::create(parent, id, text)?))
    }
}

impl Component for HitTrackButton {
    fn hwnd(&self) -> HWND {
        self.0.hwnd()
    }
    fn bounds(&self) -> RECT {
        self.0.bounds()
    }
    fn set_bounds(&mut self, rect: RECT) {
        self.0.set_bounds(rect);
    }
    fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        self.0.handle_message(msg, wparam, lparam)
    }
    fn mouse_entered(&mut self) {
        self.0.state.hover = true;
    }
    fn mouse_exited(&mut self) {
        self.0.state.hover = false;
    }
    fn focus_changed(&mut self, gained: bool) {
        self.0.focus_changed(gained);
    }
    fn id(&self) -> i32 {
        self.0.id()
    }
}
