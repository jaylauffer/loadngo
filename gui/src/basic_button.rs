use anyhow::Result;
use ui_core::{
    button::ButtonModel,
    input::{Key, UiEvent},
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, InvalidateRect,
    RegisterClassW, SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    GWL_USERDATA, HMENU, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CHAR, WM_CREATE, WM_KEYDOWN,
    WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSELEAVE, WM_MOUSEMOVE, WM_PAINT,
    WM_SETFOCUS, WM_SIZE, WS_CHILD, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::event::ComponentEvent;
use crate::event_proc::ComponentEventProc;
use crate::util::point_to_core;
use crate::util::{
    point_from_lparam, pointer_pressed_event, pointer_released_event, primary_pointer,
    rect_to_core, render_paint_ops, to_wstring,
};
use gui_win32::component::HostedComponent;
use ui_core::component::Component;

const CLASS_NAME: &str = "LNGBasicButton";

/// Minimal reimplementation of CBasicButton with Win32 hosting and core UI state.
pub struct BasicButton {
    hwnd: HWND,
    bounds: RECT,
    pub id: i32,
    pub listeners: ComponentEventProc,
    model: ButtonModel,
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
                hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(std::ptr::null_mut()),
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
            let bounds = RECT {
                left: 0,
                top: 0,
                right: 80,
                bottom: 26,
            };
            let mut btn = Self {
                hwnd,
                bounds,
                id,
                listeners: ComponentEventProc::new(),
                model: ButtonModel::new(text, rect_to_core(bounds)),
            };
            SetWindowLongPtrW(hwnd, GWL_USERDATA, &mut btn as *mut _ as isize);
            ShowWindow(hwnd, SW_SHOW);
            Ok(btn)
        }
    }

    fn notify_click(&self) {
        let evt = ComponentEvent::new(self, 0);
        self.listeners.notify(&evt);
    }

    fn request_redraw(&self) {
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    fn handle_core_response(&mut self, event: UiEvent) {
        let response = self.model.handle_event(event);
        if response.request_redraw {
            self.request_redraw();
        }
        if response.command.is_some() {
            self.notify_click();
        }
    }

    fn sync_client_bounds(&mut self) {
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.hwnd, &mut rc);
            self.bounds = rc;
            self.model.set_bounds(rect_to_core(rc));
        }
    }

    fn paint(&self, dc: windows::Win32::Graphics::Gdi::HDC) {
        let mut scene = Vec::new();
        self.model.paint(&mut scene);
        render_paint_ops(dc, &scene);
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
            WM_SIZE => {
                if let Some(s) = Self::state(hwnd) {
                    s.sync_client_bounds();
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(s) = Self::state(hwnd) {
                    s.handle_core_response(UiEvent::PointerMoved(primary_pointer(
                        point_from_lparam(lparam),
                    )));
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if let Some(s) = Self::state(hwnd) {
                    s.handle_core_response(UiEvent::PointerLeft);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(s) = Self::state(hwnd) {
                    s.handle_core_response(pointer_pressed_event(point_from_lparam(lparam)));
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(s) = Self::state(hwnd) {
                    s.handle_core_response(pointer_released_event(point_from_lparam(lparam)));
                }
                LRESULT(0)
            }
            WM_SETFOCUS => {
                if let Some(s) = Self::state(hwnd) {
                    s.handle_core_response(UiEvent::FocusChanged(true));
                }
                LRESULT(0)
            }
            WM_KILLFOCUS => {
                if let Some(s) = Self::state(hwnd) {
                    s.handle_core_response(UiEvent::FocusChanged(false));
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if let Some(s) = Self::state(hwnd) {
                    let key = match wparam.0 as u32 {
                        0x0d => Key::Enter,
                        0x20 => Key::Space,
                        _ => Key::Unknown,
                    };
                    s.handle_core_response(UiEvent::KeyPressed {
                        key,
                        modifiers: Default::default(),
                    });
                }
                LRESULT(0)
            }
            WM_KEYUP | WM_CHAR => LRESULT(0),
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
    fn bounds(&self) -> ui_core::Rect {
        rect_to_core(self.bounds)
    }

    fn set_bounds(&mut self, rect: ui_core::Rect) {
        let rect = crate::util::rect_from_core(rect);
        self.bounds = rect;
        self.model.set_bounds(rect_to_core(rect));
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                self.hwnd, rect.left, rect.top, w, h, true,
            );
        }
    }

    fn mouse_entered(&mut self) {
        self.handle_core_response(UiEvent::PointerMoved(ui_core::PointerState::mouse(
            point_to_core(windows::Win32::Foundation::POINT {
                x: self.bounds.left,
                y: self.bounds.top,
            }),
            Default::default(),
        )));
    }

    fn mouse_exited(&mut self) {
        self.handle_core_response(UiEvent::PointerLeft);
    }

    fn focus_changed(&mut self, gained: bool) {
        self.handle_core_response(UiEvent::FocusChanged(gained));
    }

    fn id(&self) -> i32 {
        self.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl HostedComponent for BasicButton {
    fn hwnd(&self) -> HWND {
        self.hwnd
    }

    fn handle_message(&mut self, _msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
        LRESULT(0)
    }
}

pub struct HitTrackButton(pub BasicButton);

impl HitTrackButton {
    pub fn create(parent: HWND, id: i32, text: &str) -> Result<Self> {
        Ok(Self(BasicButton::create(parent, id, text)?))
    }
}
