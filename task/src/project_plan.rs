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
    project_planner, task_list,
    winutil::{to_wstring, WM_SPLITTERREPOS},
};

const CLASS_NAME: &str = "LNGProjectPlan";
const DEFAULT_TASK_WIDTH: i32 = 150;
const DETAIL_HEIGHT: i32 = 127;

struct ProjectPlanState {
    hwnd: HWND,
    service: *mut Service,
    hierarchy: HWND,
    task_list: HWND,
    detail: HWND,
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

pub fn create_project_plan(parent: HWND, service: *mut Service) -> HWND {
    unsafe {
        register_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(ProjectPlanState {
            hwnd: HWND::default(),
            service,
            hierarchy: HWND::default(),
            task_list: HWND::default(),
            detail: HWND::default(),
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
            None,
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create project plan")
    }
}

pub fn refresh(hwnd: HWND) {
    unsafe {
        if let Some(state) = state(hwnd) {
            project_planner::refresh_project_hierarchy(state.hierarchy);
            task_list::refresh_task_list(state.task_list);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SPLITTERREPOS => {
            if let Some(state) = state(hwnd) {
                let mut rc = RECT::default();
                let _ = GetClientRect(state.hwnd, &mut rc);
                let mut width = rc.right - rc.left;
                if width < 0 {
                    width = 0;
                }
                let mut task_width = width - lparam.0 as i32;
                let max = width / 3;
                if task_width > max {
                    task_width = max;
                } else if task_width < DEFAULT_TASK_WIDTH {
                    task_width = DEFAULT_TASK_WIDTH;
                }
                state.task_width = task_width;
                layout_children(state);
            }
            LRESULT(0)
        }
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut ProjectPlanState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
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
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn create_children(state: &mut ProjectPlanState) {
    state.hierarchy = project_planner::create_project_hierarchy(state.hwnd, state.service);
    state.task_list = task_list::create_task_list_wnd(state.hwnd, state.service);
    state.detail = create_placeholder(state.hwnd, "Task details (not yet ported)");
}

unsafe fn layout_children(state: &mut ProjectPlanState) {
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;
    let task_width = state
        .task_width
        .clamp(DEFAULT_TASK_WIDTH, (width / 3).max(DEFAULT_TASK_WIDTH));

    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
        state.hierarchy,
        0,
        0,
        width - task_width,
        height,
        true,
    );

    let list_height = (height - DETAIL_HEIGHT).max(0);
    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
        state.task_list,
        width - task_width,
        0,
        task_width,
        list_height,
        true,
    );
    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
        state.detail,
        width - task_width,
        height - DETAIL_HEIGHT,
        task_width,
        DETAIL_HEIGHT,
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

unsafe fn state(hwnd: HWND) -> Option<&'static mut ProjectPlanState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut ProjectPlanState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut ProjectPlanState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut ProjectPlanState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}
