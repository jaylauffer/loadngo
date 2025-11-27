//! Task application entrypoint (Rust Win32 port skeleton of TaskMainWnd).
//!
//! This recreates the legacy main window with a toolbar area and tab control
//! using the raw Win32 APIs (no external GUI crates).

#![windows_subsystem = "windows"]

mod toolbar;
mod tabs;
mod winutil;

use anyhow::Result;
use std::{mem::size_of, ptr::null_mut};
use tabs::{add_tab, create_tab_host, toggle_toolbar_keyboard_mode};
use tracing::info;
use tracing_subscriber::EnvFilter;
use winutil::to_wstring;
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::HBRUSH,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
            GetWindowLongPtrW, LoadCursorW, MoveWindow, PostQuitMessage, RegisterClassExW,
            SetWindowLongPtrW, ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT,
            GWLP_USERDATA, HMENU, HICON, IDC_ARROW, MSG, SW_SHOW, WM_COMMAND, WM_CREATE,
            WM_DESTROY, WM_NCCREATE, WM_SIZE, WNDCLASSEXW, WNDCLASS_STYLES, WINDOW_EX_STYLE,
            WINDOW_STYLE, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW,
            WS_VISIBLE, WS_EX_CLIENTEDGE,
        },
    },
};

const APP_CLASS: &str = "LoadNgoTaskMainWnd";

#[derive(Default)]
struct UiState {
    hwnd: HWND,
    tab_host: HWND,
    tab_children: Vec<HWND>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    unsafe {
        init_common_controls();
        let hinstance = GetModuleHandleW(None)?.into();
        register_window_class(hinstance)?;
        let state = Box::new(UiState::default());
        let hwnd = create_main_window(hinstance, state)?;
        ShowWindow(hwnd, SW_SHOW);
        info!("TaskMainWnd started");
        message_loop();
    }
    Ok(())
}

unsafe fn init_common_controls() {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    let _ = InitCommonControlsEx(&icc);
}

unsafe fn register_window_class(hinstance: HINSTANCE) -> Result<()> {
    let class_name = to_wstring(APP_CLASS);
    let mut wc = WNDCLASSEXW::default();
    wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
    wc.style = WNDCLASS_STYLES(0);
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = hinstance;
    wc.hCursor = LoadCursorW(None, IDC_ARROW)?;
    wc.hIcon = HICON::default();
    wc.hIconSm = HICON::default();
    wc.hbrBackground = HBRUSH(null_mut());
    wc.lpszClassName = PCWSTR(class_name.as_ptr());
    let atom = RegisterClassExW(&wc);
    if atom == 0 {
        Err(anyhow::anyhow!(
            "RegisterClassExW failed: {:?}",
            GetLastError()
        ))
    } else {
        Ok(())
    }
}

unsafe fn create_main_window(hinstance: HINSTANCE, state: Box<UiState>) -> Result<HWND> {
    let class_name = to_wstring(APP_CLASS);
    let title = to_wstring("Task v0.1 (Rust)");
    let lp_param = Box::into_raw(state) as _;
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        PCWSTR(class_name.as_ptr()),
        PCWSTR(title.as_ptr()),
        WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        1024,
        768,
        None,
        HMENU(null_mut()),
        hinstance,
        Some(lp_param),
    )?;
    Ok(hwnd)
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let createstruct = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = createstruct.lpCreateParams as *mut UiState;
            if !state_ptr.is_null() {
                (*state_ptr).hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                LRESULT(1)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_CREATE => {
            if let Some(state) = get_state(hwnd) {
                create_children(hwnd, state);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = get_state(hwnd) {
                layout_children(state);
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_SYSCOMMAND => {
            if wparam.0 == windows::Win32::UI::WindowsAndMessaging::SC_KEYMENU as usize {
                if let Some(state) = get_state(hwnd) {
                    toggle_toolbar_keyboard_mode(state.tab_host);
                }
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_COMMAND => {
            let cmd_id = lparam.0 as i32;
            match cmd_id {
                toolbar::TBCREATETASK => info!("Toolbar: New Task"),
                toolbar::TBSAVEPLAN => info!("Toolbar: Save All"),
                toolbar::TBMAKEREPORT => info!("Toolbar: Generate Report"),
                toolbar::TBSYNCHRONIZE => info!("Toolbar: Network Sync"),
                toolbar::TBPRINT => info!("Toolbar: Print"),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(state_ptr) = detach_state(hwnd) {
                drop(Box::from_raw(state_ptr));
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn get_state(hwnd: HWND) -> Option<&'static mut UiState> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut UiState> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

unsafe fn create_children(parent: HWND, state: &mut UiState) {
    state.tab_host = create_tab_host(parent, true);

    // Placeholder tab children (static controls for now).
    let static_class = to_wstring("STATIC");
    let day_text = to_wstring("Day Plan View");
    let day = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        PCWSTR(static_class.as_ptr()),
        PCWSTR(day_text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
        0,
        0,
        100,
        100,
        state.tab_host,
        HMENU(null_mut()),
        None,
        None,
    )
    .expect("create day plan view");
    let proj_text = to_wstring("Project Plan View");
    let proj = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        PCWSTR(static_class.as_ptr()),
        PCWSTR(proj_text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0),
        0,
        0,
        100,
        100,
        state.tab_host,
        HMENU(null_mut()),
        None,
        None,
    )
    .expect("create project plan view");

    state.tab_children = vec![day, proj];
    add_tab(state.tab_host, "Day Plan", day);
    add_tab(state.tab_host, "Project Plan", proj);

    layout_children(state);
}

unsafe fn layout_children(state: &UiState) {
    let mut rc: RECT = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let _ = MoveWindow(
        state.tab_host,
        0,
        0,
        rc.right,
        rc.bottom,
        true,
    );
}

unsafe fn message_loop() {
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}
