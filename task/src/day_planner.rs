use std::ptr::null_mut;

use anyhow::Result;
use data::service::Service;
use tracing::info;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, GetStockObject, LineTo, MoveToEx, SelectObject, SetBkMode, SetTextColor,
    TextOutW, HBRUSH, HGDIOBJ, HFONT, LF_FACESIZE, LOGFONTW, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetScrollInfo, GetWindowLongPtrW, LoadCursorW,
    MoveWindow, RegisterClassW, SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWL_USERDATA, HMENU, IDC_ARROW, SCROLLBAR_COMMAND, SCROLLINFO, SIF_PAGE,
    SIF_POS, SIF_RANGE, SIF_TRACKPOS, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CREATE, WM_DESTROY,
    WM_ERASEBKGND, WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT, WM_SIZE, WM_VSCROLL, WNDCLASSW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOPARENTNOTIFY, WS_VSCROLL, WS_VISIBLE,
};

use crate::dragdrop::{register_drop_target, revoke_drop_target, DropPayload};
use crate::winutil::to_wstring;

const CLASS_NAME: &str = "DayPlanWnd";
const HEADER_WIDTH: i32 = 70;
const SPLITTER_BAR_WIDTH: i32 = 8;
const DEFAULT_SPLIT: f64 = 0.55;
const HOUR_FRACTION: f64 = 0.25; // 15-minute increments
const HOUR_FRACTION_PX: i32 = 30;
const WM_MOUSELEAVE: u32 = 0x02A3;

const HOUR_STRINGS: [&str; 24] = [
    "12 AM", "1 AM", "2 AM", "3 AM", "4 AM", "5 AM", "6 AM", "7 AM", "8 AM", "9 AM", "10 AM",
    "11 AM", "12 PM", "1 PM", "2 PM", "3 PM", "4 PM", "5 PM", "6 PM", "7 PM", "8 PM", "9 PM",
    "10 PM", "11 PM",
];

struct DayPlannerState {
    hwnd: HWND,
    service: *mut Service,
    spec_hwnd: HWND,
    actual_hwnd: HWND,
    splitter_hwnd: HWND,
    split_percent: f64,
    start_hour_pos: f64,
    font: HFONT,
    drop_target: Option<windows::Win32::System::Ole::IDropTarget>,
}

impl DayPlannerState {
    fn new(service: *mut Service) -> Self {
        Self {
            hwnd: HWND::default(),
            service,
            spec_hwnd: HWND::default(),
            actual_hwnd: HWND::default(),
            splitter_hwnd: HWND::default(),
            split_percent: DEFAULT_SPLIT,
            start_hour_pos: 8.0, // default 8 AM
            font: HFONT::default(),
            drop_target: None,
        }
    }
}

pub fn register_class() -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            hbrBackground: HBRUSH(null_mut()),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
    Ok(())
}

pub fn create_day_planner(parent: HWND, service: *mut Service) -> HWND {
    unsafe {
        register_class().expect("register DayPlanWnd");
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(DayPlannerState::new(service));
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_NOPARENTNOTIFY.0),
            PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_VSCROLL.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0,
            ),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            400,
            400,
            parent,
            HMENU(null_mut()),
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create day planner")
    }
}

pub fn refresh(hwnd: HWND) {
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut DayPlannerState;
            if !state_ptr.is_null() {
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);
                LRESULT(1)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_CREATE => {
            if let Some(state) = get_state(hwnd) {
                state.hwnd = hwnd;
                state.font = create_planner_font();
                init_scroll(state);
                create_children(hwnd, state);
                register_drop(state);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = get_state(hwnd) {
                layout_children(state, LOWORD(lparam.0 as u32) as i32, HIWORD(lparam.0 as u32) as i32);
                update_page(state, HIWORD(lparam.0 as u32) as i32);
            }
            LRESULT(0)
        }
        WM_VSCROLL => {
            if let Some(state) = get_state(hwnd) {
                handle_scroll(state, wparam);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(state) = get_state(hwnd) {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                let lines = -delta / 120;
                adjust_scroll(state, lines);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(state) = get_state(hwnd) {
                paint(state);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(state) = detach_state(hwnd) {
                if !(*state).font.is_invalid() {
                    let _ = windows::Win32::Graphics::Gdi::DeleteObject(HGDIOBJ((*state).font.0));
                }
                if (*state).drop_target.is_some() {
                    revoke_drop_target(hwnd);
                }
                drop(Box::from_raw(state));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSELEAVE => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn get_state(hwnd: HWND) -> Option<&'static mut DayPlannerState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlannerState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut DayPlannerState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlannerState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

unsafe fn create_planner_font() -> HFONT {
    let mut lf: LOGFONTW = std::mem::zeroed();
    lf.lfCharSet = windows::Win32::Graphics::Gdi::DEFAULT_CHARSET;
    lf.lfClipPrecision = windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS;
    lf.lfOutPrecision = windows::Win32::Graphics::Gdi::OUT_TT_ONLY_PRECIS;
    lf.lfQuality = windows::Win32::Graphics::Gdi::ANTIALIASED_QUALITY;
    lf.lfPitchAndFamily = (windows::Win32::Graphics::Gdi::DEFAULT_PITCH.0
        | windows::Win32::Graphics::Gdi::FF_DONTCARE.0) as u8;
    lf.lfHeight = -26;
    lf.lfWeight = windows::Win32::Graphics::Gdi::FW_BOLD.0 as i32;
    let face = to_wstring("Palatino Linotype");
    for (i, ch) in face.iter().enumerate() {
        if i >= LF_FACESIZE as usize - 1 {
            break;
        }
        if *ch == 0 {
            break;
        }
        lf.lfFaceName[i] = *ch;
    }
    CreateFontIndirectW(&lf)
}

unsafe fn create_children(hwnd: HWND, state: &mut DayPlannerState) {
    let hinstance = GetModuleHandleW(None).unwrap();
    let spec = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(to_wstring("STATIC").as_ptr()),
        PCWSTR(to_wstring("Spec Details").as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        0,
        0,
        100,
        100,
        hwnd,
        HMENU(null_mut()),
        hinstance,
        None,
    )
    .expect("spec create");
    let actual = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(to_wstring("STATIC").as_ptr()),
        PCWSTR(to_wstring("Actual Details").as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        0,
        0,
        100,
        100,
        hwnd,
        HMENU(null_mut()),
        hinstance,
        None,
    )
    .expect("actual create");
    let split = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(to_wstring("STATIC").as_ptr()),
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        0,
        0,
        SPLITTER_BAR_WIDTH,
        100,
        hwnd,
        HMENU(null_mut()),
        hinstance,
        None,
    )
    .expect("split create");
    state.spec_hwnd = spec;
    state.actual_hwnd = actual;
    state.splitter_hwnd = split;
}

unsafe fn layout_children(state: &mut DayPlannerState, width: i32, height: i32) {
    let plan_width = width - (HEADER_WIDTH + SPLITTER_BAR_WIDTH);
    let spec_width = (plan_width as f64 * state.split_percent) as i32;
    let act_width = plan_width - spec_width - SPLITTER_BAR_WIDTH;
    let mut x = HEADER_WIDTH;
    let _ = MoveWindow(state.spec_hwnd, x, 0, spec_width, height, true);
    x += spec_width;
    let _ = MoveWindow(state.splitter_hwnd, x, 0, SPLITTER_BAR_WIDTH, height, true);
    x += SPLITTER_BAR_WIDTH;
    let _ = MoveWindow(state.actual_hwnd, x, 0, act_width.max(0), height, true);
}

unsafe fn init_scroll(state: &mut DayPlannerState) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_POS,
        nMin: 0,
        nMax: ((24.0 / HOUR_FRACTION) - 1.0) as i32,
        nPos: (state.start_hour_pos / HOUR_FRACTION) as i32,
        ..Default::default()
    };
    let _ = SetScrollInfo(state.hwnd, windows::Win32::UI::WindowsAndMessaging::SB_VERT, &si, true);
}

unsafe fn update_page(state: &mut DayPlannerState, height: i32) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_PAGE,
        nPage: (height / HOUR_FRACTION_PX).max(1) as u32,
        ..Default::default()
    };
    let _ = SetScrollInfo(state.hwnd, windows::Win32::UI::WindowsAndMessaging::SB_VERT, &si, true);
}

unsafe fn handle_scroll(state: &mut DayPlannerState, wparam: WPARAM) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS | SIF_TRACKPOS | SIF_RANGE | SIF_PAGE,
        ..Default::default()
    };
    let _ = GetScrollInfo(
        state.hwnd,
        windows::Win32::UI::WindowsAndMessaging::SB_VERT,
        &mut si,
    );
    let code = SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32);
    match code {
        windows::Win32::UI::WindowsAndMessaging::SB_LINEUP => si.nPos -= 1,
        windows::Win32::UI::WindowsAndMessaging::SB_LINEDOWN => si.nPos += 1,
        windows::Win32::UI::WindowsAndMessaging::SB_PAGEUP => si.nPos -= si.nPage as i32,
        windows::Win32::UI::WindowsAndMessaging::SB_PAGEDOWN => si.nPos += si.nPage as i32,
        windows::Win32::UI::WindowsAndMessaging::SB_THUMBTRACK
        | windows::Win32::UI::WindowsAndMessaging::SB_THUMBPOSITION => {
            si.nPos = si.nTrackPos;
        }
        _ => {}
    }
    if si.nPos < si.nMin {
        si.nPos = si.nMin;
    } else if si.nPos > si.nMax {
        si.nPos = si.nMax;
    }
    state.start_hour_pos = si.nPos as f64 * HOUR_FRACTION;
    si.fMask = SIF_POS;
    let _ = SetScrollInfo(
        state.hwnd,
        windows::Win32::UI::WindowsAndMessaging::SB_VERT,
        &si,
        true,
    );
    refresh(state.hwnd);
}

unsafe fn adjust_scroll(state: &mut DayPlannerState, delta_lines: i32) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS | SIF_RANGE | SIF_PAGE,
        ..Default::default()
    };
    let _ = GetScrollInfo(
        state.hwnd,
        windows::Win32::UI::WindowsAndMessaging::SB_VERT,
        &mut si,
    );
    si.nPos += delta_lines;
    if si.nPos < si.nMin {
        si.nPos = si.nMin;
    } else if si.nPos > si.nMax {
        si.nPos = si.nMax;
    }
    state.start_hour_pos = si.nPos as f64 * HOUR_FRACTION;
    si.fMask = SIF_POS;
    let _ = SetScrollInfo(
        state.hwnd,
        windows::Win32::UI::WindowsAndMessaging::SB_VERT,
        &si,
        true,
    );
    refresh(state.hwnd);
}

unsafe fn paint(state: &DayPlannerState) {
    let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
    let dc = windows::Win32::Graphics::Gdi::BeginPaint(state.hwnd, &mut ps);
    if dc.0.is_null() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;

    // background
    let bg = HBRUSH(GetStockObject(WHITE_BRUSH).0);
    let _ = windows::Win32::Graphics::Gdi::FillRect(dc, &rc, bg);

    // header separator
    let _ = MoveToEx(dc, HEADER_WIDTH, 0, None);
    let _ = LineTo(dc, HEADER_WIDTH, height);

    let old_font = SelectObject(dc, state.font);
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, COLORREF(0x00202020));

    let mut y = -((state.start_hour_pos / HOUR_FRACTION) as i32 % (HOUR_FRACTION_PX));
    let mut hour_idx = ((state.start_hour_pos / 1.0).floor() as i32) % 24;
    while y < height {
        let _ = MoveToEx(dc, 0, y, None);
        let _ = LineTo(dc, width, y);
        let label = HOUR_STRINGS[(hour_idx.rem_euclid(24)) as usize];
        let mut w = to_wstring(label);
        if !w.is_empty() {
            w.pop();
        }
        let text_y = y + 4;
        if text_y >= -20 && text_y <= height + 20 {
            let _ = TextOutW(dc, 4, text_y, &w);
        }
        y += HOUR_FRACTION_PX;
        hour_idx = (hour_idx + 1) % 24;
    }

    let _ = SelectObject(dc, old_font);
    let _ = windows::Win32::Graphics::Gdi::EndPaint(state.hwnd, &ps);
}

unsafe fn register_drop(state: &mut DayPlannerState) {
    if let Ok(target) = register_drop_target(state.hwnd, |payload| {
        match payload {
            DropPayload::Files(files) => info!("Dropped files on DayPlanner: {:?}", files),
            DropPayload::Text(t) => info!("Dropped text on DayPlanner: {}", t),
        }
        Ok(())
    }) {
        state.drop_target = Some(target);
    }
}

fn LOWORD(l: u32) -> u16 {
    (l & 0xffff) as u16
}

fn HIWORD(l: u32) -> u16 {
    ((l >> 16) & 0xffff) as u16
}
