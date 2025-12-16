//! Task application entrypoint (Rust Win32 port skeleton of TaskMainWnd).
//!
//! This recreates the legacy main window with a toolbar area and tab control
//! using the raw Win32 APIs (no external GUI crates).

#![windows_subsystem = "windows"]

mod day_plan;
mod project_plan;
mod tabs;
mod toolbar;
mod winutil;

use anyhow::Result;
use data::{
    config::Configuration, file_manager::FileManager, model_utils::now_timestamp, service::Service,
    task::Task,
};
use network::Network;
use std::{mem::size_of, path::PathBuf, ptr::null_mut};
use tabs::{add_tab, create_tab_host, toggle_toolbar_keyboard_mode};
use tracing::{info, Level};
use tracing_subscriber::{fmt::writer::MakeWriterExt, EnvFilter};
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::HBRUSH,
    System::{Com::CoInitializeEx, LibraryLoader::GetModuleHandleW},
    UI::{
        Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
            GetWindowLongPtrW, LoadCursorW, MoveWindow, PostQuitMessage, RegisterClassExW,
            SetWindowLongPtrW, ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT,
            GWLP_USERDATA, HICON, HMENU, IDC_ARROW, MSG, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE,
            WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE, WM_SIZE, WM_TIMER, WNDCLASSEXW,
            WNDCLASS_STYLES, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_CLIENTEDGE,
            WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        },
    },
};
use winutil::to_wstring;

const APP_CLASS: &str = "LoadNgoTaskMainWnd";
const AUTOSAVE_TIMER_ID: usize = 0x400;
const AUTOSAVE_INTERVAL_MS: u32 = 60_000;

struct UiState {
    hwnd: HWND,
    tab_host: HWND,
    tab_children: Vec<HWND>,
    day_plan: HWND,
    project_plan: HWND,
    service: Service,
    network: Network,
    plan_name: String,
}

fn main() -> Result<()> {
    let file_appender = tracing_appender::rolling::never(".", "task.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(Level::DEBUG.into()))
        .with_writer(non_blocking.and(std::io::stdout))
        .init();

    unsafe {
        CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED).ok()?;
        init_common_controls();
        let hinstance = GetModuleHandleW(None)?.into();
        let (mut service, mut network) = build_services()?;
        load_plan(&mut service, "user_plan");
        network.init()?;
        register_window_class(hinstance)?;
        let state = Box::new(UiState {
            hwnd: HWND::default(),
            tab_host: HWND::default(),
            tab_children: Vec::new(),
            day_plan: HWND::default(),
            project_plan: HWND::default(),
            service,
            network,
            plan_name: "user_plan".to_string(),
        });
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
                let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(
                    hwnd,
                    AUTOSAVE_TIMER_ID,
                    AUTOSAVE_INTERVAL_MS,
                    None,
                );
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
            if let Some(state) = get_state(hwnd) {
                let cmd_id = lparam.0 as i32;
                handle_toolbar_command(state, cmd_id);
            }
            LRESULT(0)
        }
        toolbar::WM_DELETE_TASK => {
            if let Some(state) = get_state(hwnd) {
                let id = wparam.0 as u64;
                state.service.remove_task(id);
                day_plan::refresh(state.day_plan);
                project_plan::refresh(state.project_plan);
                info!("Deleted task {id} via trash drop");
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == AUTOSAVE_TIMER_ID {
                if let Some(state) = get_state(hwnd) {
                    save_plan(state);
                }
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            if let Some(state) = get_state(hwnd) {
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(hwnd, AUTOSAVE_TIMER_ID);
                save_plan(state);
            }
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
    let tab_host = create_tab_host(parent, true);

    let svc_ptr: *mut Service = &mut state.service;
    let day = day_plan::create_day_plan(tab_host, svc_ptr);
    let proj = project_plan::create_project_plan(tab_host, svc_ptr);

    state.day_plan = day;
    state.project_plan = proj;
    state.tab_children = vec![day, proj];
    add_tab(tab_host, "Day Plan", day);
    add_tab(tab_host, "Project Plan", proj);
    state.tab_host = tab_host;

    layout_children(state);
}

unsafe fn layout_children(state: &mut UiState) {
    let mut rc: RECT = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let _ = MoveWindow(
        state.tab_host,
        0,
        0,
        rc.right - rc.left,
        rc.bottom - rc.top,
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

unsafe fn build_services() -> Result<(Service, Network)> {
    let mut config = Configuration::new();
    config.set("enableMulticast", "0");
    let base_dir = PathBuf::from("taskdata");
    let files = FileManager::new(base_dir);
    let service = Service::new(config, files);
    let network = Network::new();
    Ok((service, network))
}

fn load_plan(service: &mut Service, plan_name: &str) {
    match service.load(plan_name) {
        Ok(_) => info!("Loaded plan \"{plan_name}\""),
        Err(err) => info!("No existing plan \"{plan_name}\" to load ({err:?})"),
    }
}

fn save_plan(state: &UiState) {
    match state.service.save(&state.plan_name) {
        Ok(_) => info!("Plan saved to {}", state.plan_name),
        Err(err) => info!("Save failed: {err:?}"),
    }
}

unsafe fn handle_toolbar_command(state: &mut UiState, cmd_id: i32) {
    match cmd_id {
        toolbar::TBCREATETASK => {
            let task = Task::spawn("New Task", "local-user", 1, 1, now_timestamp());
            state.service.add_task(task);
            info!("Created task ({} total)", state.service.tasks.len());
            day_plan::refresh(state.day_plan);
            project_plan::refresh(state.project_plan);
        }
        toolbar::TBSAVEPLAN => {
            save_plan(state);
        }
        toolbar::TBMAKEREPORT => {
            info!("Report generation not yet ported");
        }
        toolbar::TBSYNCHRONIZE => match state.network.send_sync_request(0) {
            Ok(_) => info!("Sent sync request"),
            Err(err) => info!("Sync request failed: {err:?}"),
        },
        toolbar::TBPRINT => info!("Print not yet implemented"),
        _ => {}
    }
}
