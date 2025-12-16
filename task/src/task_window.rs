use anyhow::Result;
use data::{config::Configuration, service::Service, task::Task};
use network::Network;
use std::{mem::size_of, ptr::null_mut};
use tracing::info;
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{GetStockObject, HBRUSH, WHITE_BRUSH},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
            GetWindowLongPtrW, GetWindowRect, KillTimer, LoadCursorW, LoadImageW, MoveWindow,
            PostQuitMessage, RegisterClassExW, SetTimer, SetWindowLongPtrW, TranslateMessage,
            CREATESTRUCTW, GWLP_USERDATA, HICON, HMENU, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR,
            MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NCCREATE,
            WM_SIZE, WM_SYSCOMMAND, WM_TIMER, WNDCLASSEXW, WNDCLASS_STYLES, WS_CHILD,
            WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW, WS_VISIBLE, CS_HREDRAW,
            CS_VREDRAW, SC_KEYMENU, WS_EX_ACCEPTFILES,
        },
    },
};

use crate::{
    day_planner,
    dragdrop::{register_drop_target, revoke_drop_target, DropPayload},
    project_plan,
    tabs::{add_tab, create_tab_host, toggle_toolbar_keyboard_mode},
    toolbar,
    winutil::to_wstring,
};

const CLASS_NAME: &str = "loadngo::Task::20::MainWindowClass";
const WINDOW_TITLE: &str = "loadngo Task";
const IDI_APPICON: u16 = 114;
const CHAT_HEIGHT: i32 = 80;
const AUTOSAVE_TIMER_ID: usize = 1;
// Matches the legacy cadence in TaskWindow/TaskMainWnd (approx 12 minutes).
const AUTOSAVE_INTERVAL_MS: u32 = 719_011;
pub const WM_STOREPLAN: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1240;
pub const WM_SYNCHPLAN: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1260;

pub struct TaskWindowState {
    pub hwnd: HWND,
    pub tab_host: HWND,
    pub day_plan: HWND,
    pub project_plan: HWND,
    pub schedule: HWND,
    pub chat: HWND,
    pub drop_target: Option<windows::Win32::System::Ole::IDropTarget>,
    pub service: Service,
    pub network: Network,
    pub plan_name: String,
    pub enable_multicast: bool,
}

impl TaskWindowState {
    fn new(service: Service, network: Network, plan_name: String) -> Self {
        let enable_multicast = service.config.get_int("enableMulticast", 0) != 0;
        Self {
            hwnd: HWND::default(),
            tab_host: HWND::default(),
            day_plan: HWND::default(),
            project_plan: HWND::default(),
            schedule: HWND::default(),
            chat: HWND::default(),
            drop_target: None,
            service,
            network,
            plan_name,
            enable_multicast,
        }
    }
}

pub unsafe fn init_common_controls() {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_STANDARD_CLASSES,
    };
    let _ = InitCommonControlsEx(&icc);
}

pub unsafe fn register_window_class(hinstance: HINSTANCE) -> Result<()> {
    let class_name = to_wstring(CLASS_NAME);
    let mut wc = WNDCLASSEXW::default();
    wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
    wc.style = WNDCLASS_STYLES(CS_HREDRAW.0 | CS_VREDRAW.0);
    wc.lpfnWndProc = Some(wndproc);
    wc.hInstance = hinstance;
    wc.hCursor = LoadCursorW(None, IDC_ARROW)?;
    let icons = load_app_icons(hinstance);
    wc.hIcon = icons.0;
    wc.hIconSm = icons.1;
    wc.hbrBackground = HBRUSH(GetStockObject(WHITE_BRUSH).0);
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

pub unsafe fn create_main_window(
    hinstance: HINSTANCE,
    service: Service,
    network: Network,
    plan_name: String,
) -> Result<HWND> {
    let class_name = to_wstring(CLASS_NAME);
    let title = to_wstring(WINDOW_TITLE);
    let state = Box::new(TaskWindowState::new(service, network, plan_name));
    // Restore window bounds from configuration (defaults 40,40,750,550).
    let left = state.service.config.get_int("mainWindowLeft", 40) as i32;
    let top = state.service.config.get_int("mainWindowTop", 40) as i32;
    let right = state.service.config.get_int("mainWindowRight", 750) as i32;
    let bottom = state.service.config.get_int("mainWindowBottom", 550) as i32;
    let width = (right - left).max(200);
    let height = (bottom - top).max(200);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_ACCEPTFILES.0),
        PCWSTR(class_name.as_ptr()),
        PCWSTR(title.as_ptr()),
        WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        left,
        top,
        width,
        height,
        None,
        HMENU(null_mut()),
        hinstance,
        Some(Box::into_raw(state) as *mut _),
    )?;
    Ok(hwnd)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let createstruct = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = createstruct.lpCreateParams as *mut TaskWindowState;
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
                let _ = SetTimer(hwnd, AUTOSAVE_TIMER_ID, AUTOSAVE_INTERVAL_MS, None);
                if state.enable_multicast {
                    info!("(network) user intro placeholder");
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = get_state(hwnd) {
                layout_children(state);
            }
            LRESULT(0)
        }
        WM_SYSCOMMAND => {
            if wparam.0 == SC_KEYMENU as usize {
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
                day_planner::refresh(state.day_plan);
                project_plan::refresh(state.project_plan);
                info!("Deleted task {id} via trash drop");
            }
            LRESULT(0)
        }
        WM_STOREPLAN => {
            if let Some(state) = get_state(hwnd) {
                save_plan(state);
            }
            LRESULT(0)
        }
        WM_SYNCHPLAN => {
            if let Some(state) = get_state(hwnd) {
                let _ = state.network.send_sync_request(0);
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
                let _ = KillTimer(hwnd, AUTOSAVE_TIMER_ID);
                // Persist window bounds back to config.
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_ok() {
                    state
                        .service
                        .config
                        .set_int("mainWindowLeft", rc.left as i64);
                    state
                        .service
                        .config
                        .set_int("mainWindowTop", rc.top as i64);
                    state
                        .service
                        .config
                        .set_int("mainWindowRight", rc.right as i64);
                    state
                        .service
                        .config
                        .set_int("mainWindowBottom", rc.bottom as i64);
                }
                if state.enable_multicast {
                    info!("(network) user depart placeholder");
                }
                save_plan(state);
                if state.drop_target.is_some() {
                    revoke_drop_target(state.tab_host);
                    state.drop_target = None;
                }
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

unsafe fn get_state(hwnd: HWND) -> Option<&'static mut TaskWindowState> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TaskWindowState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut TaskWindowState> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TaskWindowState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

unsafe fn create_children(parent: HWND, state: &mut TaskWindowState) {
    let tab_host = create_tab_host(parent, state.enable_multicast);
    let svc_ptr: *mut Service = &mut state.service;
    let day = day_planner::create_day_planner(tab_host, svc_ptr);
    let proj = project_plan::create_project_plan(tab_host, svc_ptr);
    let sched = create_placeholder(tab_host, "Schedule (not yet ported)");
    state.day_plan = day;
    state.project_plan = proj;
    state.schedule = sched;
    add_tab(tab_host, "Day Plan", day);
    add_tab(tab_host, "Project Plan", proj);
    add_tab(tab_host, "Schedule", sched);
    state.tab_host = tab_host;

    // Accept drops on the tab host (matches legacy RegisterDragDrop on TaskTabWnd).
    if let Ok(target) = register_drop_target(tab_host, |payload| {
        match payload {
            DropPayload::Files(files) => info!("Dropped files: {:?}", files),
            DropPayload::Text(text) => info!("Dropped text: {text}"),
        }
        Ok(())
    }) {
        state.drop_target = Some(target);
    }

    // Simple chat/status area along the bottom.
    state.chat = create_placeholder(parent, "Chat / status (not yet ported)");
    layout_children(state);
}

unsafe fn layout_children(state: &mut TaskWindowState) {
    let mut rc: RECT = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;
    let tab_height = (height - CHAT_HEIGHT).max(0);

    let _ = MoveWindow(state.tab_host, 0, 0, width, tab_height, true);
    let _ = MoveWindow(
        state.chat,
        0,
        tab_height,
        width,
        CHAT_HEIGHT,
        true,
    );
}

fn save_plan(state: &TaskWindowState) {
    match state.service.save_all(&state.plan_name) {
        Ok(_) => info!("Plan saved to {}", state.plan_name),
        Err(err) => info!("Save failed: {err:?}"),
    }
}

unsafe fn handle_toolbar_command(state: &mut TaskWindowState, cmd_id: i32) {
    match cmd_id {
        toolbar::TBCREATETASK => {
            let task =
                Task::spawn("New Task", "local-user", 1, 1, data::model_utils::now_timestamp());
            state.service.add_task(task);
            info!("Created task ({} total)", state.service.tasks.len());
            day_planner::refresh(state.day_plan);
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

unsafe fn create_placeholder(parent: HWND, text: &str) -> HWND {
    let class = to_wstring("STATIC");
    let caption = to_wstring(text);
    let hinstance = GetModuleHandleW(None).unwrap();
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class.as_ptr()),
        PCWSTR(caption.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
        0,
        0,
        100,
        100,
        parent,
        HMENU(null_mut()),
        HINSTANCE(hinstance.0),
        None,
    )
    .expect("create placeholder")
}

pub unsafe fn build_services() -> Result<(Service, Network)> {
    let mut config = Configuration::new();
    config.set("enableMulticast", "0");
    let base_dir = std::path::PathBuf::from("taskdata");
    let files = data::file_manager::FileManager::new(base_dir);
    let service = Service::new(config, files);
    let network = Network::new();
    Ok((service, network))
}

pub fn load_all(service: &mut Service, plan_name: &str) {
    match service.load_all(plan_name) {
        Ok(_) => info!("Loaded plan \"{plan_name}\" (with config)"),
        Err(err) => info!("No existing plan \"{plan_name}\" to load ({err:?})"),
    }
}

fn load_app_icons(hinstance: HINSTANCE) -> (HICON, HICON) {
    unsafe {
        let res = PCWSTR(IDI_APPICON as usize as *const u16);
        let large = LoadImageW(
            hinstance,
            res,
            IMAGE_ICON,
            32,
            32,
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default();
        let small = LoadImageW(
            hinstance,
            res,
            IMAGE_ICON,
            16,
            16,
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default();
        (large, small)
    }
}

// A minimal message loop so callers can reuse this module standalone.
pub unsafe fn message_loop() {
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}
