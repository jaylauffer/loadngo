use anyhow::Result;
use std::ptr::null_mut;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, RegisterClassExW, SetWindowLongPtrW, ShowWindow,
    CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA, HMENU, HICON, IDC_ARROW, SW_SHOW, WNDCLASSEXW,
    WNDCLASS_STYLES, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_OVERLAPPEDWINDOW,
};

use crate::util::to_wstring;

/// Basic host window that carries an opaque pointer for state.
pub struct HostWindow {
    pub hwnd: HWND,
}

impl HostWindow {
    pub fn register(class_name: &str) -> Result<()> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let name_w = to_wstring(class_name);
            let mut wc = WNDCLASSEXW::default();
            wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.style = WNDCLASS_STYLES(0);
            wc.lpfnWndProc = Some(Self::wndproc);
            wc.hInstance = hinstance.into();
            wc.hCursor = windows::Win32::UI::WindowsAndMessaging::LoadCursorW(None, IDC_ARROW)?;
            wc.hIcon = HICON::default();
            wc.hIconSm = HICON::default();
            wc.hbrBackground = HBRUSH(null_mut());
            wc.lpszClassName = PCWSTR(name_w.as_ptr());
            if RegisterClassExW(&wc) == 0 {
                anyhow::bail!("RegisterClassExW failed: {:?}", GetLastError());
            }
        }
        Ok(())
    }

    pub fn create(class_name: &str, title: &str, lp: *mut std::ffi::c_void) -> Result<Self> {
        unsafe {
            Self::register(class_name)?;
            let hinstance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(to_wstring(class_name).as_ptr()),
                PCWSTR(to_wstring(title).as_ptr()),
                WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                800,
                600,
                None,
                HMENU(null_mut()),
                hinstance,
                Some(lp),
            )?;
            ShowWindow(hwnd, SW_SHOW);
            Ok(Self { hwnd })
        }
    }

    pub fn client_rect(&self) -> RECT {
        let mut rc = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut rc); }
        rc
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            windows::Win32::UI::WindowsAndMessaging::WM_NCCREATE => {
                let cs = &*(lparam.0 as *const CREATESTRUCTW);
                if !cs.lpCreateParams.is_null() {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
                }
                LRESULT(1)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
