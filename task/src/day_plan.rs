use std::ptr::null_mut;

use data::service::Service;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, RegisterClassW,
            SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWL_USERDATA, HMENU,
            WINDOW_EX_STYLE, WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_SIZE, WNDCLASSW, WS_CHILD,
            WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
        },
    },
};

use crate::{
    date_banner, day_planner, task_list,
    winutil::to_wstring,
};

const CLASS_NAME: &str = "LNGDayPlanFrame";
const BANNER_HEIGHT: i32 = 40;
const DEFAULT_TASK_WIDTH: i32 = 150;

struct DayPlanFrameState {
    hwnd: HWND,
    service: *mut Service,
    banner: HWND,
    planner: HWND,
    task_list: HWND,
    task_width: i32,
}

pub fn register_class() {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
}

pub fn create_day_plan(parent: HWND, service: *mut Service) -> HWND {
    unsafe {
        register_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(DayPlanFrameState {
            hwnd: HWND::default(),
            service,
            banner: HWND::default(),
            planner: HWND::default(),
            task_list: HWND::default(),
            task_width: DEFAULT_TASK_WIDTH,
        });
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
            0,
            0,
            100,
            100,
            parent,
            HMENU(null_mut()),
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create day plan frame")
    }
}

pub fn refresh(hwnd: HWND) {
    unsafe {
        if let Some(state) = state(hwnd) {
            day_planner::refresh(state.planner);
            task_list::refresh_dp_task_list(state.task_list);
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(state.banner, None, true);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut DayPlanFrameState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
                if let Some(service) = state.service.as_ref() {
                    state.task_width = service
                        .config
                        .get_int("dayplanTaskWidth", DEFAULT_TASK_WIDTH as i64) as i32;
                }
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);
                create_children(state);
                layout_children(state);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state(hwnd) {
                layout_children(state);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ptr) = detach_state(hwnd) {
                if let Some(service) = unsafe { (*ptr).service.as_mut() } {
                    service
                        .config
                        .set_int("dayplanTaskWidth", (*ptr).task_width as i64);
                }
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn create_children(state: &mut DayPlanFrameState) {
    state.banner = date_banner::create_date_banner(state.hwnd);
    state.planner = match day_planner::create_day_planner(state.hwnd, state.service) {
        Ok(hwnd) => hwnd,
        Err(_) => create_placeholder(state.hwnd, "Day Planner failed to start"),
    };
    state.task_list = task_list::create_dp_task_list_wnd(state.hwnd, state.service);
}

unsafe fn layout_children(state: &mut DayPlanFrameState) {
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;

    let task_width = state
        .task_width
        .clamp(DEFAULT_TASK_WIDTH, (width / 3).max(DEFAULT_TASK_WIDTH));
    state.task_width = task_width;

    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
        state.banner,
        0,
        0,
        width,
        BANNER_HEIGHT,
        true,
    );
    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
        state.planner,
        0,
        BANNER_HEIGHT,
        width - task_width,
        height - BANNER_HEIGHT,
        true,
    );
    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
        state.task_list,
        width - task_width,
        BANNER_HEIGHT,
        task_width,
        height - BANNER_HEIGHT,
        true,
    );
}

unsafe fn create_placeholder(parent: HWND, text: &str) -> HWND {
    let class = to_wstring("STATIC");
    let label = to_wstring(text);
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class.as_ptr()),
        PCWSTR(label.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
        0,
        0,
        100,
        30,
        parent,
        HMENU(null_mut()),
        None,
        None,
    )
    .unwrap_or(HWND::default())
}

unsafe fn state(hwnd: HWND) -> Option<&'static mut DayPlanFrameState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlanFrameState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut DayPlanFrameState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlanFrameState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}
