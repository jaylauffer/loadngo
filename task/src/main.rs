//! Task application entrypoint (Rust Win32 port skeleton of TaskMainWnd).
//!
//! This recreates the legacy main window with a toolbar area and tab control
//! using the raw Win32 APIs (no external GUI crates).

#![windows_subsystem = "windows"]

mod toolbar;
mod winutil;

use anyhow::Result;
use std::{mem::size_of, ptr::null_mut};
use toolbar::{create_toolbar, toggle_keyboard_mode, TOOLBAR_CLASS};
use tracing::info;
use tracing_subscriber::EnvFilter;
use winutil::to_wstring;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::{
    Foundation::{
        GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
    },
    Graphics::Gdi::{GetStockObject, HBRUSH, DEFAULT_GUI_FONT},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            InitCommonControlsEx, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX,
            NMHDR, TCITEMW, TCN_SELCHANGE, TCIF_TEXT, TCM_GETCURSEL, TCM_INSERTITEMW,
            WC_TABCONTROLW,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
            GetWindowLongPtrW, LoadCursorW, MoveWindow, PostQuitMessage, RegisterClassExW,
            SendMessageW, SetWindowLongPtrW, ShowWindow, TranslateMessage, CREATESTRUCTW,
            CW_USEDEFAULT, GWLP_USERDATA, HMENU, HICON, IDC_ARROW, MSG, SHOW_WINDOW_CMD, SW_HIDE,
            SW_SHOW, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_NOTIFY, WM_SETFONT,
            WM_SIZE, WNDCLASSEXW, WNDCLASS_STYLES, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD,
            WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
            WS_EX_CLIENTEDGE,
        },
    },
};

const APP_CLASS: &str = "LoadNgoTaskMainWnd";
const ID_TAB: isize = 1001;
const TAB_DAY_PLAN: usize = 0;
const TAB_PROJECT_PLAN: usize = 1;

#[derive(Default)]
struct UiState {
    hwnd: HWND,
    tab: HWND,
    toolbar: HWND,
    tab_children: [HWND; 2],
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
        dwICC: ICC_TAB_CLASSES | ICC_STANDARD_CLASSES,
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
            if wparam.0 == windows::Win32::UI::WindowsAndMessaging::SC_KEYMENU.0 as usize {
                if let Some(state) = get_state(hwnd) {
                    toggle_keyboard_mode(state.toolbar);
                }
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_NOTIFY => {
            // Tab change
            let nmhdr = &*(lparam.0 as *const NMHDR);
            if nmhdr.hwndFrom == get_state(hwnd).map(|s| s.tab).unwrap_or_default()
                && nmhdr.code as u32 == TCN_SELCHANGE
            {
                if let Some(state) = get_state(hwnd) {
                    switch_tab(state);
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
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let tab_class = WC_TABCONTROLW;
    let tab = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        tab_class,
        PWSTR::null(),
        WINDOW_STYLE(
            WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0 | WS_CLIPCHILDREN.0 | WS_TABSTOP.0,
        ),
        0,
        0,
        100,
        100,
        parent,
        HMENU(ID_TAB as usize as *mut _),
        None,
        None,
    )
    .expect("create tab control");
    state.tab = tab;
    SendMessageW(
        tab,
        WM_SETFONT,
        WPARAM(font.0 as usize),
        LPARAM(1),
    );

    add_tab(tab, TAB_DAY_PLAN as i32, "Day Plan");
    add_tab(tab, TAB_PROJECT_PLAN as i32, "Project Plan");

    // Custom, hand-crafted toolbar (legacy look).
    state.toolbar = create_toolbar(parent, true);

    // Placeholder tab children (static controls for now).
    let static_class = to_wstring("STATIC");
    let day_text = to_wstring("Day Plan View");
    state.tab_children[TAB_DAY_PLAN] = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        PCWSTR(static_class.as_ptr()),
        PCWSTR(day_text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
        0,
        0,
        100,
        100,
        tab,
        HMENU(null_mut()),
        None,
        None,
    )
    .expect("create day plan view");
    let proj_text = to_wstring("Project Plan View");
    state.tab_children[TAB_PROJECT_PLAN] = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
        PCWSTR(static_class.as_ptr()),
        PCWSTR(proj_text.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0),
        0,
        0,
        100,
        100,
        tab,
        HMENU(null_mut()),
        None,
        None,
    )
    .expect("create project plan view");

    layout_children(state);
    switch_tab(state);
}

unsafe fn add_tab(tab: HWND, index: i32, label: &str) {
    let mut tci = TCITEMW::default();
    tci.mask = TCIF_TEXT;
    let txt = to_wstring(label);
    tci.pszText = PWSTR(txt.as_ptr() as _);
    let _ = SendMessageW(
        tab,
        TCM_INSERTITEMW,
        WPARAM(index as usize),
        LPARAM(&tci as *const _ as isize),
    );
}

unsafe fn layout_children(state: &UiState) {
    let mut rc: RECT = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let toolbar_height = 32;
    let _ = MoveWindow(
        state.toolbar,
        0,
        0,
        rc.right,
        toolbar_height,
        true,
    );
    let _ = MoveWindow(
        state.tab,
        0,
        toolbar_height,
        rc.right,
        rc.bottom - toolbar_height,
        true,
    );
    // Fit tab children within tab client area.
    let mut tab_rc: RECT = RECT::default();
    let _ = GetClientRect(state.tab, &mut tab_rc);
    tab_rc.top += 24; // account for tab headers
    for child in state.tab_children.iter() {
        let _ = MoveWindow(
            *child,
            4,
            tab_rc.top,
            tab_rc.right - 8,
            tab_rc.bottom - tab_rc.top - 4,
            true,
        );
    }
}

unsafe fn switch_tab(state: &UiState) {
    let selected = SendMessageW(state.tab, TCM_GETCURSEL, WPARAM(0), LPARAM(0)).0 as i32;
    for (idx, child) in state.tab_children.iter().enumerate() {
        let cmd = if idx as i32 == selected { SW_SHOW } else { SW_HIDE };
        let _ = ShowWindow(*child, SHOW_WINDOW_CMD(cmd.0 as i32));
    }
}

unsafe fn message_loop() {
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}
