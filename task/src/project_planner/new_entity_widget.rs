use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, SetWindowPos, SetWindowTextW, ShowWindow, HMENU, SWP_NOSIZE, SWP_SHOWWINDOW,
    SW_HIDE, WINDOW_EX_STYLE, WINDOW_STYLE, WS_BORDER, WS_POPUP, WS_VISIBLE,
};

use crate::winutil::to_wstring;

pub struct NewEntityWidget {
    hwnd: HWND,
}

impl NewEntityWidget {
    pub fn create(parent: HWND) -> Self {
        unsafe {
            let hinstance = GetModuleHandleW(None).unwrap();
            let text = to_wstring("New Entity (not yet ported)");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(to_wstring("STATIC").as_ptr()),
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(WS_POPUP.0 | WS_BORDER.0 | WS_VISIBLE.0),
                0,
                0,
                180,
                40,
                parent,
                HMENU(std::ptr::null_mut()),
                hinstance,
                None,
            )
            .unwrap_or(HWND::default());
            ShowWindow(hwnd, SW_HIDE);
            Self { hwnd }
        }
    }

    pub fn begin_edit(&self, pt: POINT) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND::default(),
                pt.x - 90,
                pt.y - 20,
                0,
                0,
                SWP_NOSIZE | SWP_SHOWWINDOW,
            );
            let _ = SetWindowTextW(
                self.hwnd,
                PCWSTR(to_wstring("New Entity (not yet ported)").as_ptr()),
            );
        }
    }

    pub fn cancel_edit(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}
