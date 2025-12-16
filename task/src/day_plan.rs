use std::ptr::null_mut;

use data::service::Service;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, EndPaint, FillRect, GetStockObject, SelectObject, SetBkMode, SetTextColor,
            TextOutW, DEFAULT_GUI_FONT, HBRUSH, PAINTSTRUCT, TRANSPARENT, WHITE_BRUSH,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, RegisterClassW,
            SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, GWL_USERDATA,
            WINDOW_EX_STYLE, WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_PAINT, WM_SIZE, WNDCLASSW,
            WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
        },
    },
};

use crate::winutil::to_wstring;

const CLASS_NAME: &str = "LNGDayPlan";

struct DayPlanState {
    hwnd: HWND,
    service: *mut Service,
}

pub fn register_class() {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            hbrBackground: HBRUSH(null_mut()),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
}

pub fn create_day_plan(parent: HWND, service: *mut Service) -> HWND {
    unsafe {
        register_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(DayPlanState {
            hwnd: HWND::default(),
            service,
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
        .expect("create day plan")
    }
}

pub fn refresh(hwnd: HWND) {
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut DayPlanState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);
            }
            LRESULT(0)
        }
        WM_SIZE => LRESULT(0),
        WM_PAINT => {
            if let Some(state) = state(hwnd) {
                paint(state);
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

unsafe fn paint(state: &mut DayPlanState) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(state.hwnd, &mut ps);

    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);

    // Light background.
    let bg = HBRUSH(GetStockObject(WHITE_BRUSH).0);
    let _ = FillRect(dc, &rc, bg);

    // Prepare text output.
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, COLORREF(0x00202020));
    let old_font = SelectObject(
        dc,
        GetStockObject(windows::Win32::Graphics::Gdi::DEFAULT_GUI_FONT),
    );

    // Build a small status string plus a handful of tasks.
    let mut lines = vec![format!(
        "Day Plan ({} tasks)",
        service(state).map(|s| s.tasks.len()).unwrap_or(0)
    )];
    if let Some(service) = service(state) {
        let mut tasks: Vec<_> = service.tasks.values().collect();
        tasks.sort_by_key(|t| t.entity.id);
        for task in tasks.into_iter().take(12) {
            lines.push(format!("{}: {}", task.entity.id, task.name));
        }
        if service.tasks.len() > 12 {
            lines.push(format!("... {} more", service.tasks.len() - 12));
        }
    }

    let mut y = 6;
    for line in lines {
        let mut w = to_wstring(&line);
        if !w.is_empty() {
            w.pop(); // drop the trailing null for TextOutW
        }
        let _ = TextOutW(dc, 8, y, &w);
        y += 18;
    }

    let _ = SelectObject(dc, old_font);
    let _ = EndPaint(state.hwnd, &ps);
}

unsafe fn service(state: &DayPlanState) -> Option<&Service> {
    state.service.as_ref()
}

unsafe fn state(hwnd: HWND) -> Option<&'static mut DayPlanState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlanState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut DayPlanState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlanState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}
