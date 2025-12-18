use anyhow::Result;
use std::ptr::null_mut;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassExW, SetWindowLongPtrW, CREATESTRUCTW,
    CW_USEDEFAULT, GWLP_USERDATA, HICON, HMENU, IDC_ARROW, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CREATE, WM_DESTROY, WM_SIZE, WNDCLASSEXW, WNDCLASS_STYLES, GetClientRect,
};

use crate::container::Container;
use crate::util::to_wstring;

/// Lightweight host window that routes input to a gui::Container, similar to legacy CContainerWnd.
pub struct ContainerHost {
    pub hwnd: HWND,
    pub container: Container,
}

impl ContainerHost {
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
                // Allow "already exists" to succeed so callers can register idempotently.
                if GetLastError()
                    != windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS
                {
                    anyhow::bail!("RegisterClassExW failed");
                }
            }
        }
        Ok(())
    }

    pub fn create(parent: HWND, class_name: &str) -> Result<HWND> {
        Self::register(class_name)?;
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let host = Box::new(ContainerHost {
                hwnd: HWND::default(),
                container: Container::new(HWND::default()),
            });
            let ptr = Box::into_raw(host);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(to_wstring(class_name).as_ptr()),
                PCWSTR::null(),
                WINDOW_STYLE(
                    windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0
                        | windows::Win32::UI::WindowsAndMessaging::WS_VISIBLE.0
                        | windows::Win32::UI::WindowsAndMessaging::WS_CLIPCHILDREN.0
                        | windows::Win32::UI::WindowsAndMessaging::WS_CLIPSIBLINGS.0
                        | windows::Win32::UI::WindowsAndMessaging::WS_VSCROLL.0,
                ),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                100,
                100,
                parent,
                HMENU(null_mut()),
                hinstance,
                Some(ptr as *mut _),
            )?;
            Ok(hwnd)
        }
    }

    /// Execute a closure with a mutable reference to the host's Container.
    pub unsafe fn with_container<R>(hwnd: HWND, f: impl FnOnce(&mut Container) -> R) -> Option<R> {
        if let Some(host) = Self::get_mut(hwnd) {
            Some(f(&mut host.container))
        } else {
            None
        }
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
                let ptr = cs.lpCreateParams as *mut ContainerHost;
                if !ptr.is_null() {
                    let host = &mut *ptr;
                    host.hwnd = hwnd;
                    host.container.hwnd = hwnd;
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(host) = Self::get_mut(hwnd) {
                    // Allow children/components to react before teardown.
                    let _ = host.container.handle_message(WM_DESTROY, WPARAM(0), LPARAM(0));
                }
                let ptr = Self::take_ptr(hwnd);
                if !ptr.is_null() {
                    // Clean up any drop target registration.
                    unsafe { (&mut *ptr).container.revoke_file_drop(); }
                    drop(Box::from_raw(ptr));
                }
                LRESULT(0)
            }
            WM_SIZE => {
                if let Some(host) = Self::get_mut(hwnd) {
                    let mut rc = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    for child in host.container.children.iter_mut() {
                        child.set_bounds(rc);
                    }
                    return host.container.handle_message(msg, wparam, lparam);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => {
                if let Some(host) = Self::get_mut(hwnd) {
                    // Forward input to the container; components handle drawing in their own paint paths.
                    host.container.handle_message(msg, wparam, lparam)
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
        }
    }

    unsafe fn get_mut<'a>(hwnd: HWND) -> Option<&'a mut ContainerHost> {
        let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
            as *mut ContainerHost;
        ptr.as_mut()
    }

    unsafe fn take_ptr(hwnd: HWND) -> *mut ContainerHost {
        let ptr = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
            as *mut ContainerHost;
        if !ptr.is_null() {
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        ptr
    }
}
