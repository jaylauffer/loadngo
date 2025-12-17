use std::ptr::null_mut;

use anyhow::Result;
use data::service::Service;
use tracing::info;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, CreateSolidBrush,
    DeleteDC, DeleteObject, GetStockObject, GradientFill, LineTo, MoveToEx, SelectObject, SetBkMode,
    SetTextColor, ScreenToClient, TextOutW, AC_SRC_ALPHA, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, DIB_RGB_COLORS, GRADIENT_FILL_RECT_H, GRADIENT_RECT, HBRUSH, HDC, HGDIOBJ,
    HFONT, LF_FACESIZE, LOGFONTW, TRIVERTEX, TRANSPARENT, WHITE_BRUSH, BI_RGB,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, GetScrollInfo, GetWindowLongPtrW,
    LoadCursorW, MoveWindow, RegisterClassW, SetCursor, SetWindowLongPtrW, CREATESTRUCTW,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWL_USERDATA, HMENU, IDC_ARROW, IDC_SIZEWE,
    SCROLLBAR_COMMAND, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_VSCROLL, WNDCLASSW, WS_CHILD,
    WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOPARENTNOTIFY, WS_VSCROLL, WS_VISIBLE,
};

use crate::dragdrop::{register_drop_target, revoke_drop_target, DropPayload};
use crate::winutil::to_wstring;

const CLASS_NAME: &str = "DayPlanWnd";
const HEADER_WIDTH: i32 = 70;
const SPLITTER_BAR_WIDTH: i32 = 8;
const DEFAULT_SPLIT: f64 = 0.55;
const BANNER_HEIGHT: i32 = 42;
const HOUR_FRACTION: f64 = 0.25; // 15-minute increments
const HOUR_FRACTION_PX: i32 = 18;
const MIN_PANE_WIDTH: i32 = 80;
const WM_MOUSELEAVE: u32 = 0x02A3;

const HOUR_STRINGS: [&str; 24] = [
    "12am", "1am", "2am", "3am", "4am", "5am", "6am", "7am", "8am", "9am", "10am", "11am", "12pm",
    "1pm", "2pm", "3pm", "4pm", "5pm", "6pm", "7pm", "8pm", "9pm", "10pm", "11pm",
];

struct PaneState {
    color: COLORREF,
    label: Vec<u16>,
}

struct SplitterState {
    color: COLORREF,
    parent: HWND,
    dragging: bool,
}

struct DayPlannerState {
    hwnd: HWND,
    service: *mut Service,
    spec_hwnd: HWND,
    actual_hwnd: HWND,
    splitter_hwnd: HWND,
    banner_hwnd: HWND,
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
            banner_hwnd: HWND::default(),
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

fn register_pane_class() -> Result<()> {
    unsafe {
        static mut DONE: bool = false;
        if DONE {
            return Ok(());
        }
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(pane_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring("DayPlanPane").as_ptr()),
            hbrBackground: HBRUSH(null_mut()),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
        DONE = true;
        Ok(())
    }
}

fn register_splitter_class() -> Result<()> {
    unsafe {
        static mut DONE: bool = false;
        if DONE {
            return Ok(());
        }
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(splitter_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring("DayPlanSplitter").as_ptr()),
            hbrBackground: HBRUSH(null_mut()),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
        DONE = true;
        Ok(())
    }
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
    register_pane_class().ok();
    register_splitter_class().ok();
    let hinstance = GetModuleHandleW(None).unwrap();

    // Banner across the top.
    let banner = crate::date_banner::create_date_banner(hwnd);

    let spec_state = Box::new(PaneState {
        color: COLORREF(0x00e4f0ff),
        label: to_wstring("Plan Details"),
    });
    let actual_state = Box::new(PaneState {
        color: COLORREF(0x00fff4e4),
        label: to_wstring("Actual Details"),
    });
    let split_state = Box::new(SplitterState {
        color: COLORREF(0x00d0d0d0),
        parent: hwnd,
        dragging: false,
    });

    let spec = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_NOPARENTNOTIFY.0),
        PCWSTR(to_wstring("DayPlanPane").as_ptr()),
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        0,
        0,
        100,
        100,
        hwnd,
        HMENU(null_mut()),
        hinstance,
        Some(Box::into_raw(spec_state) as *mut _),
    )
    .expect("spec create");
    let actual = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_NOPARENTNOTIFY.0),
        PCWSTR(to_wstring("DayPlanPane").as_ptr()),
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        0,
        0,
        100,
        100,
        hwnd,
        HMENU(null_mut()),
        hinstance,
        Some(Box::into_raw(actual_state) as *mut _),
    )
    .expect("actual create");
    let split = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(to_wstring("DayPlanSplitter").as_ptr()),
        PCWSTR::null(),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
        0,
        0,
        SPLITTER_BAR_WIDTH,
        100,
        hwnd,
        HMENU(null_mut()),
        hinstance,
        Some(Box::into_raw(split_state) as *mut _),
    )
    .expect("split create");
    state.banner_hwnd = banner;
    state.spec_hwnd = spec;
    state.actual_hwnd = actual;
    state.splitter_hwnd = split;
}

unsafe fn layout_children(state: &mut DayPlannerState, width: i32, height: i32) {
    let banner_h = BANNER_HEIGHT;
    let plan_height = (height - banner_h).max(0);
    let plan_width = width - (HEADER_WIDTH + SPLITTER_BAR_WIDTH);
    if plan_width <= MIN_PANE_WIDTH * 2 {
        let spec_width = plan_width / 2;
        let act_width = plan_width - spec_width - SPLITTER_BAR_WIDTH;
        let mut x = HEADER_WIDTH;
        let _ = MoveWindow(state.banner_hwnd, 0, 0, width, banner_h, true);
        let _ = MoveWindow(state.spec_hwnd, x, banner_h, spec_width, plan_height, true);
        x += spec_width;
        let _ = MoveWindow(state.splitter_hwnd, x, banner_h, SPLITTER_BAR_WIDTH, plan_height, true);
        x += SPLITTER_BAR_WIDTH;
        let _ = MoveWindow(state.actual_hwnd, x, banner_h, act_width.max(0), plan_height, true);
        return;
    }

    let mut spec_width = (plan_width as f64 * state.split_percent).round() as i32;
    spec_width = spec_width.clamp(MIN_PANE_WIDTH, plan_width - MIN_PANE_WIDTH);
    state.split_percent = (spec_width as f64 / plan_width as f64).clamp(0.1, 0.9);

    let act_width = plan_width - spec_width - SPLITTER_BAR_WIDTH;
    let mut x = HEADER_WIDTH;
    let _ = MoveWindow(state.banner_hwnd, 0, 0, width, banner_h, true);
    let _ = MoveWindow(state.spec_hwnd, x, banner_h, spec_width, plan_height, true);
    x += spec_width;
    let _ = MoveWindow(state.splitter_hwnd, x, banner_h, SPLITTER_BAR_WIDTH, plan_height, true);
    x += SPLITTER_BAR_WIDTH;
    let _ = MoveWindow(state.actual_hwnd, x, banner_h, act_width.max(0), plan_height, true);
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

    draw_hour_header(dc, height);

    // header separator and banner baseline
    let _ = MoveToEx(dc, 0, BANNER_HEIGHT, None);
    let _ = LineTo(dc, width, BANNER_HEIGHT);
    let _ = MoveToEx(dc, HEADER_WIDTH, BANNER_HEIGHT, None);
    let _ = LineTo(dc, HEADER_WIDTH, height);

    let old_font = SelectObject(dc, state.font);
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, COLORREF(0x00202020));

    let mut y = BANNER_HEIGHT + -((state.start_hour_pos / HOUR_FRACTION) as i32 % (HOUR_FRACTION_PX));
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

unsafe fn draw_hour_header(dc: HDC, height: i32) {
    // Gradient fill the hour gutter to better match the legacy look.
    let top = BANNER_HEIGHT;
    let bottom = height;
    let verts = [
        TRIVERTEX {
            x: 0,
            y: top,
            Red: 0xf0 << 8,
            Green: 0xf0 << 8,
            Blue: 0xf0 << 8,
            Alpha: 0,
        },
        TRIVERTEX {
            x: HEADER_WIDTH + 1,
            y: bottom,
            Red: 0xdc << 8,
            Green: 0xdc << 8,
            Blue: 0xdc << 8,
            Alpha: 0,
        },
    ];
    let rect = [GRADIENT_RECT {
        UpperLeft: 0,
        LowerRight: 1,
    }];
    let _ = GradientFill(
        dc,
        &verts,
        rect.as_ptr() as *const _,
        rect.len() as u32,
        GRADIENT_FILL_RECT_H,
    );
}

unsafe fn alpha_fill(dc: HDC, rc: &RECT, color: COLORREF, alpha: u8) {
    let width = 1;
    let height = 1;
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0 as u32,
        biSizeImage: (width * height * 4) as u32,
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let hbitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok();
    if let Some(hbmp) = hbitmap {
        let mem_dc = CreateCompatibleDC(dc);
        let old = SelectObject(mem_dc, hbmp);
        if !bits.is_null() {
            let b = (color.0 & 0xFF) as u8;
            let g = ((color.0 >> 8) & 0xFF) as u8;
            let r = ((color.0 >> 16) & 0xFF) as u8;
            let pixel: [u8; 4] = [
                ((b as u16 * alpha as u16) / 255) as u8,
                ((g as u16 * alpha as u16) / 255) as u8,
                ((r as u16 * alpha as u16) / 255) as u8,
                alpha,
            ];
            std::ptr::copy_nonoverlapping(pixel.as_ptr(), bits as *mut u8, 4);
        }
        let bf = BLENDFUNCTION {
            BlendOp: AC_SRC_ALPHA as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1, // use per-pixel alpha
        };
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        let _ = AlphaBlend(mem_dc, 0, 0, 1, 1, mem_dc, 0, 0, 1, 1, bf); // noop to satisfy some drivers
        let _ = AlphaBlend(dc, rc.left, rc.top, w, h, mem_dc, 0, 0, 1, 1, bf);
        let _ = SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
        let _ = DeleteObject(hbmp);
    }
}

unsafe extern "system" fn pane_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *mut PaneState;
            if !ptr.is_null() {
                SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let dc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !dc.0.is_null() {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                if let Some(state) = (GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut PaneState).as_ref() {
                    alpha_fill(dc, &rc, state.color, 160);
                    if !state.label.is_empty() {
                        let _ = SetBkMode(dc, TRANSPARENT);
                        let _ = SetTextColor(dc, COLORREF(0x00303030));
                        let _ = TextOutW(dc, 6, 6, &state.label);
                    }
                }
            }
            let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &mut ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut PaneState;
            if !ptr.is_null() {
                let _ = SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn splitter_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *mut SplitterState;
            if !ptr.is_null() {
                SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = (GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut SplitterState).as_mut()
            {
                state.dragging = true;
                let _ = SetCapture(hwnd);
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_MOUSEMOVE => {
            if let Some(split) = (GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut SplitterState).as_mut()
            {
                if split.dragging {
                    update_split_from_cursor(split);
                    return LRESULT(0);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONUP => {
            if let Some(split) = (GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut SplitterState).as_mut()
            {
                if split.dragging {
                    split.dragging = false;
                    let _ = ReleaseCapture();
                }
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_SETCURSOR => {
            if let Ok(cursor) = LoadCursorW(None, IDC_SIZEWE) {
                let _ = SetCursor(cursor);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let dc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !dc.0.is_null() {
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let color = if let Some(state) =
                    (GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut SplitterState).as_ref()
                {
                    state.color
                } else {
                    COLORREF(0x00c0c0c0)
                };
                let brush = CreateSolidBrush(color);
                let _ = windows::Win32::Graphics::Gdi::FillRect(dc, &rc, brush);
                let _ = DeleteObject(brush);
            }
            let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &mut ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut SplitterState;
            if !ptr.is_null() {
                let _ = SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
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

unsafe fn update_split_from_cursor(split: &mut SplitterState) {
    let mut pt = POINT::default();
    if GetCursorPos(&mut pt).is_err() {
        return;
    }
    let parent = split.parent;
    let _ = ScreenToClient(parent, &mut pt);
    if let Some(state) = get_state(parent) {
        let mut rc = RECT::default();
        let _ = GetClientRect(parent, &mut rc);
        let width = rc.right - rc.left;
        let height = rc.bottom - rc.top;
        let plan_width = width - (HEADER_WIDTH + SPLITTER_BAR_WIDTH);
        if plan_width <= (MIN_PANE_WIDTH * 2) {
            return;
        }
        let raw = (pt.x - HEADER_WIDTH) as f64 / plan_width as f64;
        state.split_percent = raw.clamp(0.1, 0.9);
        layout_children(state, width, height);
        refresh(parent);
    }
}

fn LOWORD(l: u32) -> u16 {
    (l & 0xffff) as u16
}

fn HIWORD(l: u32) -> u16 {
    ((l >> 16) & 0xffff) as u16
}
