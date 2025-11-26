use anyhow::Result;
use gui::buffered::BufferedWnd;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{Rectangle, SetDCBrushColor, SetDCPenColor};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, RegisterClassW,
    SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWL_USERDATA, HMENU, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_POPUP,
};

use crate::winutil::to_wstring;

const CLASS_NAME: &str = "LNGTreeEntryWidget";

pub struct TreeEntryWidget {
    hwnd: HWND,
    buffer: BufferedWnd,
    pen: u32,
    brush: u32,
}

impl TreeEntryWidget {
    pub fn create(parent: HWND, rect: RECT) -> Result<HWND> {
        unsafe {
            register_class();
            let hinstance = GetModuleHandleW(None)?;
            let state = Box::new(TreeEntryWidget {
                hwnd: HWND::default(),
                buffer: BufferedWnd::new(),
                pen: 0x00748572,
                brush: 0x00a3af9e,
            });
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(WS_POPUP.0),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                parent,
                HMENU(std::ptr::null_mut()),
                hinstance,
                Some(Box::into_raw(state) as *mut _),
            )?;
            Ok(hwnd)
        }
    }
}

fn register_class() {
    unsafe {
        static mut DONE: bool = false;
        if DONE {
            return;
        }
        let hinstance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
        DONE = true;
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *mut TreeEntryWidget;
            if !ptr.is_null() {
                (*ptr).hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(state) = widget_state(hwnd) {
                let _ = state.buffer.paint(hwnd, |_, dc, width, height| {
                    unsafe {
                        SetDCBrushColor(dc, windows::Win32::Foundation::COLORREF(state.brush));
                        SetDCPenColor(dc, windows::Win32::Foundation::COLORREF(state.pen));
                        let _ = Rectangle(dc, 0, 0, width, height);
                        SetDCBrushColor(dc, windows::Win32::Foundation::COLORREF(0x00e3d9e4));
                        let _ = Rectangle(dc, 3, 3, width - 3, height - 3);
                    }
                    Ok(())
                });
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn widget_state(hwnd: HWND) -> Option<&'static mut TreeEntryWidget> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut TreeEntryWidget;
    ptr.as_mut()
}
