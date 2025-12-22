use std::ptr::null_mut;

use anyhow::Result;
use data::service::Service;
use gui::buffered::BufferedWnd;
use gui::component::Component;
use gui::container::Container;
use tracing::info;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, DrawEdge, FillRect, GetStockObject,
    GradientFill, LineTo, MoveToEx, ScreenToClient, SelectObject, SetBkMode, SetTextColor, TextOutW,
    BDR_RAISEDOUTER, BF_RECT, GRADIENT_FILL_RECT_H, GRADIENT_RECT, HBRUSH, HDC, HGDIOBJ, HFONT,
    LF_FACESIZE, LOGFONTW, TRIVERTEX, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetCursorPos, GetScrollInfo, GetWindowLongPtrW,
    LoadCursorW, MoveWindow, RegisterClassW, SetCursor, SetWindowLongPtrW, CREATESTRUCTW,
    CS_HREDRAW, CS_VREDRAW, GWL_USERDATA, HMENU, IDC_ARROW, IDC_SIZEWE,
    SCROLLBAR_COMMAND, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CAPTURECHANGED, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_VSCROLL,
    WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE, WS_VSCROLL,
};

use crate::dragdrop::{register_drop_target, revoke_drop_target, DropPayload};
use crate::winutil::to_wstring;

const CLASS_NAME: &str = "DayPlanWnd";
const HOST_CLASS: &str = "DayPlanHostWnd";
const HEADER_WIDTH: i32 = 70;
const SPLITTER_BAR_WIDTH: i32 = 5;
const SPLITTER_QUICKTAB_WIDTH: i32 = 8;
const DEFAULT_SPLIT: f64 = 0.55;
const HOST_BANNER_HEIGHT: i32 = 42;
const HOUR_FRACTION: f64 = 0.25; // 15-minute increments
const HOUR_FRACTION_PX: i32 = 18; // pixels per 15-minute increment (matches legacy spacing)
const MIN_PANE_WIDTH: i32 = 80;
const WM_MOUSELEAVE: u32 = 0x02A3;

const HOUR_STRINGS: [&str; 24] = [
    "12am", "1am", "2am", "3am", "4am", "5am", "6am", "7am", "8am", "9am", "10am", "11am", "12pm",
    "1pm", "2pm", "3pm", "4pm", "5pm", "6pm", "7pm", "8pm", "9pm", "10pm", "11pm",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneKind {
    Spec,
    Actual,
}

struct DayPlanPane {
    host_hwnd: HWND,
    rect: RECT,
    kind: PaneKind,
}

impl DayPlanPane {
    fn new(host_hwnd: HWND, kind: PaneKind) -> Self {
        Self {
            host_hwnd,
            rect: RECT::default(),
            kind,
        }
    }

    unsafe fn paint(&self, dc: HDC) {
        let width = self.rect.right - self.rect.left;
        let height = self.rect.bottom - self.rect.top;
        if width <= 0 || height <= 0 {
            return;
        }
        let mut y = 0;
        while y <= height {
            let _ = MoveToEx(dc, self.rect.left + 10, self.rect.top + y, None);
            let _ = LineTo(dc, self.rect.right - 10, self.rect.top + y);
            y += HOUR_FRACTION_PX;
        }
    }
}

impl Component for DayPlanPane {
    fn hwnd(&self) -> HWND {
        self.host_hwnd
    }

    fn bounds(&self) -> RECT {
        self.rect
    }

    fn set_bounds(&mut self, rect: RECT) {
        self.rect = rect;
    }

    fn handle_message(&mut self, _msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
        LRESULT(0)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

struct DayPlanSplitter {
    host_hwnd: HWND,
    rect: RECT,
    bar_rect: RECT,
}

impl DayPlanSplitter {
    fn new(host_hwnd: HWND) -> Self {
        Self {
            host_hwnd,
            rect: RECT::default(),
            bar_rect: RECT::default(),
        }
    }

    fn update_bar_rect(&mut self) {
        self.bar_rect = RECT {
            left: self.rect.left + SPLITTER_QUICKTAB_WIDTH,
            top: self.rect.top,
            right: self.rect.right - SPLITTER_QUICKTAB_WIDTH,
            bottom: self.rect.bottom,
        };
    }

    unsafe fn paint(&self, dc: HDC) {
        let brush = CreateSolidBrush(COLORREF(0x009b9b9b));
        let _ = FillRect(dc, &self.bar_rect, brush);
        let _ = DeleteObject(brush);
        let mut edge_rc = self.bar_rect;
        let _ = DrawEdge(dc, &mut edge_rc, BDR_RAISEDOUTER, BF_RECT);
    }
}

impl Component for DayPlanSplitter {
    fn hwnd(&self) -> HWND {
        self.host_hwnd
    }

    fn bounds(&self) -> RECT {
        self.rect
    }

    fn set_bounds(&mut self, rect: RECT) {
        self.rect = rect;
        self.update_bar_rect();
    }

    fn handle_message(&mut self, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let _ = (msg, lparam);
        LRESULT(0)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

struct DayPlannerState {
    hwnd: HWND,
    container: Container,
    service: *mut Service,
    split_percent: f64,
    splitter_dragging: bool,
    start_hour_pos: f64,
    font: HFONT,
    buffer: BufferedWnd,
    drop_target: Option<windows::Win32::System::Ole::IDropTarget>,
}

impl DayPlannerState {
    fn new(service: *mut Service) -> Self {
        Self {
            hwnd: HWND::default(),
            container: Container::new(HWND::default()),
            service,
            split_percent: DEFAULT_SPLIT,
            splitter_dragging: false,
            start_hour_pos: 8.0, // default 8 AM
            font: HFONT::default(),
            buffer: BufferedWnd::new(),
            drop_target: None,
        }
    }
}

pub fn register_class() -> Result<()> {
    unsafe {
        static mut DONE: bool = false;
        if DONE {
            return Ok(());
        }
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(planner_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            hbrBackground: HBRUSH(null_mut()),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
        DONE = true;
        Ok(())
    }
}

fn register_host_class() -> Result<()> {
    unsafe {
        static mut DONE: bool = false;
        if DONE {
            return Ok(());
        }
        let hinstance = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(host_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(HOST_CLASS).as_ptr()),
            hbrBackground: HBRUSH(null_mut()),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        DONE = true;
    }
    Ok(())
}

pub fn create_day_planner(parent: HWND, service: *mut Service) -> Result<HWND> {
    register_host_class()?;
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let state = Box::new(DayPlannerHostState {
            hwnd: HWND::default(),
            banner_hwnd: HWND::default(),
            body_hwnd: HWND::default(),
            service,
        });
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(to_wstring(HOST_CLASS).as_ptr()),
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
        )?;
        Ok(hwnd)
    }
}

fn create_planner_body(parent: HWND, service: *mut Service) -> Result<HWND> {
    unsafe {
        register_class()?;
        let hinstance = GetModuleHandleW(None)?;
        let state = Box::new(DayPlannerState::new(service));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0 | WS_VSCROLL.0,
            ),
            0,
            0,
            100,
            100,
            parent,
            HMENU(null_mut()),
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )?;
        Ok(hwnd)
    }
}

pub fn refresh(hwnd: HWND) {
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, true);
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

unsafe fn create_children(state: &mut DayPlannerState) {
    let spec = DayPlanPane::new(state.hwnd, PaneKind::Spec);
    let actual = DayPlanPane::new(state.hwnd, PaneKind::Actual);
    let splitter = DayPlanSplitter::new(state.hwnd);
    state.container.add(Box::new(spec));
    state.container.add(Box::new(actual));
    state.container.add(Box::new(splitter));
}

unsafe fn layout_children(state: &mut DayPlannerState, width: i32, height: i32) {
    let plan_height = height.max(0);
    let plan_width = width - (HEADER_WIDTH + SPLITTER_BAR_WIDTH);
    if plan_width <= 0 {
        return;
    }
    let (spec_width, act_width) = if plan_width <= MIN_PANE_WIDTH * 2 {
        let spec_width = plan_width / 2;
        let act_width = plan_width - spec_width - SPLITTER_BAR_WIDTH;
        (spec_width, act_width)
    } else {
        let mut spec_width = (plan_width as f64 * state.split_percent).round() as i32;
        spec_width = spec_width.clamp(MIN_PANE_WIDTH, plan_width - MIN_PANE_WIDTH);
        state.split_percent = (spec_width as f64 / plan_width as f64).clamp(0.1, 0.9);
        let act_width = plan_width - spec_width - SPLITTER_BAR_WIDTH;
        (spec_width, act_width)
    };

    let spec_rect = RECT {
        left: HEADER_WIDTH,
        top: 0,
        right: HEADER_WIDTH + spec_width,
        bottom: plan_height,
    };
    let actual_left = HEADER_WIDTH + spec_width + SPLITTER_BAR_WIDTH;
    let act_rect = RECT {
        left: actual_left,
        top: 0,
        right: actual_left + act_width.max(0),
        bottom: plan_height,
    };
    let split_left = HEADER_WIDTH + spec_width - SPLITTER_QUICKTAB_WIDTH;
    let split_rect = RECT {
        left: split_left,
        top: 0,
        right: split_left + SPLITTER_BAR_WIDTH + (SPLITTER_QUICKTAB_WIDTH * 2),
        bottom: plan_height,
    };

    for child in state.container.children.iter_mut() {
        if let Some(pane) = child.as_any_mut().downcast_mut::<DayPlanPane>() {
            pane.rect = if pane.kind == PaneKind::Spec {
                spec_rect
            } else {
                act_rect
            };
        } else if let Some(splitter) = child.as_any_mut().downcast_mut::<DayPlanSplitter>() {
            splitter.rect = split_rect;
            splitter.update_bar_rect();
        }
    }
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

unsafe extern "system" fn planner_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *mut DayPlannerState;
            if !ptr.is_null() {
                let state = &mut *ptr;
                state.hwnd = hwnd;
                state.container.set_hwnd(hwnd);
                state.font = create_planner_font();
                SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
                init_scroll(state);
                create_children(state);
                register_drop(state);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let width = rc.right - rc.left;
                let height = rc.bottom - rc.top;
                layout_children(state, width, height);
                update_page(state, height);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = get_state(hwnd) {
                let w = LOWORD(lparam.0 as u32) as i32;
                let h = HIWORD(lparam.0 as u32) as i32;
                layout_children(state, w, h);
                update_page(state, h);
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
        WM_LBUTTONDOWN => {
            if let Some(state) = get_state(hwnd) {
                let pt = point_from_lparam(lparam);
                if splitter_hit_test(state, pt) {
                    state.splitter_dragging = true;
                    let _ = SetCapture(hwnd);
                } else {
                    let _ = state.container.handle_message(msg, wparam, lparam);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(state) = get_state(hwnd) {
                let pt = point_from_lparam(lparam);
                if state.splitter_dragging {
                    update_split_from_point(state, pt.x);
                } else {
                    let _ = state.container.handle_message(msg, wparam, lparam);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = get_state(hwnd) {
                if state.splitter_dragging {
                    state.splitter_dragging = false;
                    let _ = ReleaseCapture();
                } else {
                    let _ = state.container.handle_message(msg, wparam, lparam);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(state) = get_state(hwnd) {
                let _ = state.container.handle_message(msg, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if let Some(state) = get_state(hwnd) {
                state.splitter_dragging = false;
                let _ = state.container.handle_message(msg, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(state) = get_state(hwnd) {
                let state_ptr = state as *mut DayPlannerState;
                let buffer = &mut state.buffer;
                let _ = buffer.paint(hwnd, |_, mem_dc, w, h| {
                    let state = unsafe { &*state_ptr };
                    render_scene(state, mem_dc, w, h);
                    Ok(())
                });
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if cursor_over_splitter(hwnd) {
                if let Ok(cursor) = LoadCursorW(None, IDC_SIZEWE) {
                    let _ = SetCursor(cursor);
                    return LRESULT(1);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            if let Some(ptr) = detach_state(hwnd) {
                if !(*ptr).font.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ((*ptr).font.0));
                }
                if (*ptr).drop_target.is_some() {
                    revoke_drop_target(hwnd);
                }
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn cursor_over_splitter(hwnd: HWND) -> bool {
    let mut pt = POINT::default();
    if GetCursorPos(&mut pt).is_err() {
        return false;
    }
    let _ = ScreenToClient(hwnd, &mut pt);
    if let Some(state) = get_state(hwnd) {
        if let Some(rc) = splitter_rect(state) {
            return point_in_rect(pt, rc);
        }
    }
    false
}

unsafe fn render_scene(state: &DayPlannerState, dc: HDC, width: i32, height: i32) {
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };

    let bg = HBRUSH(GetStockObject(WHITE_BRUSH).0);
    let _ = windows::Win32::Graphics::Gdi::FillRect(dc, &rc, bg);

    draw_hour_header(dc, height);

    let _ = MoveToEx(dc, HEADER_WIDTH, 0, None);
    let _ = LineTo(dc, HEADER_WIDTH, height);

    let old_font = SelectObject(dc, state.font);
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, COLORREF(0x00202020));

    let pixels_per_hour = (HOUR_FRACTION_PX as f64 / HOUR_FRACTION) as i32;
    let start_hour = state.start_hour_pos.floor();
    let fractional = state.start_hour_pos - start_hour;
    let mut y = -((fractional / HOUR_FRACTION) * HOUR_FRACTION_PX as f64).round() as i32;
    let mut hour_idx = (start_hour as i32).rem_euclid(24);
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
        y += pixels_per_hour;
        hour_idx = (hour_idx + 1) % 24;
    }

    paint_components(dc, state);

    let _ = SelectObject(dc, old_font);
}

unsafe fn draw_hour_header(dc: HDC, height: i32) {
    // Gradient fill the hour gutter to better match the legacy look.
    let top = 0;
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

unsafe fn paint_components(dc: HDC, state: &DayPlannerState) {
    for child in &state.container.children {
        if let Some(pane) = child.as_any().downcast_ref::<DayPlanPane>() {
            pane.paint(dc);
        } else if let Some(splitter) = child.as_any().downcast_ref::<DayPlanSplitter>() {
            splitter.paint(dc);
        }
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

unsafe fn update_split_from_point(state: &mut DayPlannerState, x: i32) {
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;
    let plan_width = width - (HEADER_WIDTH + SPLITTER_BAR_WIDTH);
    if plan_width <= (MIN_PANE_WIDTH * 2) {
        return;
    }
    let raw = (x - HEADER_WIDTH) as f64 / plan_width as f64;
    state.split_percent = raw.clamp(0.1, 0.9);
    layout_children(state, width, height);
    refresh(state.hwnd);
}

fn LOWORD(l: u32) -> u16 {
    (l & 0xffff) as u16
}

fn HIWORD(l: u32) -> u16 {
    ((l >> 16) & 0xffff) as u16
}

fn GET_X_LPARAM(lp: LPARAM) -> i32 {
    (lp.0 as u32 & 0xffff) as i16 as i32
}

fn GET_Y_LPARAM(lp: LPARAM) -> i32 {
    ((lp.0 as u32 >> 16) & 0xffff) as i16 as i32
}

fn point_from_lparam(lp: LPARAM) -> POINT {
    POINT {
        x: GET_X_LPARAM(lp),
        y: GET_Y_LPARAM(lp),
    }
}

fn splitter_hit_test(state: &DayPlannerState, pt: POINT) -> bool {
    splitter_rect(state)
        .map(|rc| point_in_rect(pt, rc))
        .unwrap_or(false)
}

fn splitter_rect(state: &DayPlannerState) -> Option<RECT> {
    state
        .container
        .children
        .iter()
        .find_map(|child| child.as_any().downcast_ref::<DayPlanSplitter>().map(|s| s.bounds()))
}

fn point_in_rect(pt: POINT, rc: RECT) -> bool {
    pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
}

struct DayPlannerHostState {
    hwnd: HWND,
    banner_hwnd: HWND,
    body_hwnd: HWND,
    service: *mut Service,
}

unsafe extern "system" fn host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *mut DayPlannerHostState;
            if !ptr.is_null() {
                let state = &mut *ptr;
                state.hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
                let banner = crate::date_banner::create_date_banner(hwnd);
                let body = create_planner_body(hwnd, state.service).expect("create planner body");
                state.banner_hwnd = banner;
                state.body_hwnd = body;
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                layout_host_children(state, rc.right - rc.left, rc.bottom - rc.top);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = host_state(hwnd) {
                let width = LOWORD(lparam.0 as u32) as i32;
                let height = HIWORD(lparam.0 as u32) as i32;
                layout_host_children(state, width, height);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ptr) = host_detach(hwnd) {
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn layout_host_children(state: &mut DayPlannerHostState, width: i32, height: i32) {
    let banner_h = HOST_BANNER_HEIGHT;
    let body_h = (height - banner_h).max(0);
    let _ = MoveWindow(state.banner_hwnd, 0, 0, width, banner_h, true);
    let _ = MoveWindow(state.body_hwnd, 0, banner_h, width, body_h, true);
}

unsafe fn host_state(hwnd: HWND) -> Option<&'static mut DayPlannerHostState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlannerHostState;
    ptr.as_mut()
}

unsafe fn host_detach(hwnd: HWND) -> Option<*mut DayPlannerHostState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DayPlannerHostState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}
