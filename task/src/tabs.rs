use std::ptr::null_mut;

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            BeginPaint, BeginPath, CloseFigure, CreateFontW, DeleteObject, EndPaint, EndPath,
            ExtTextOutW, FillRgn, FrameRgn, GetDC, GetRgnBox, GetStockObject,
            GetTextExtentPoint32W, GradientFill, LineTo, MoveToEx, OffsetViewportOrgEx,
            PathToRegion, PolyBezierTo, PtInRegion, Rectangle, ReleaseDC, SelectClipRgn,
            SelectObject, SetBkMode, SetDCBrushColor, SetDCPenColor, SetViewportOrgEx,
            GRADIENT_FILL_RECT_H, GRADIENT_RECT, HBRUSH, HDC, HFONT, HRGN, PAINTSTRUCT,
            TRIVERTEX, TRANSPARENT, DC_BRUSH, DC_PEN, FW_BOLD, FW_NORMAL,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, MoveWindow,
                PostMessageW, RegisterClassW, SetParent, SetWindowLongPtrW, ShowWindow,
                CREATESTRUCTW, GWL_USERDATA, HMENU, SW_HIDE, SW_SHOW, WNDCLASSW, WM_CREATE,
                WM_DESTROY, WM_LBUTTONDOWN, WM_PAINT, WM_SIZE, WM_USER, WS_CHILD,
                WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TRANSPARENT, WS_VISIBLE, WINDOW_EX_STYLE,
                WINDOW_STYLE,
            },
        },
    },
};

use crate::{
    toolbar::{create_toolbar, toggle_keyboard_mode},
    winutil::to_wstring,
};

const TAB_HOST_CLASS: &str = "LNGCustomTabs";
const WM_TOGGLE_TOOLBAR_KB: u32 = WM_USER + 200;

const TAB_WIDTH: i32 = 60;
const TAB_SPACING: i32 = 100;
const TAB_BASELINE: i32 = 40;
const TAB_TEXT_OFFSET_Y: i32 = 2;
const TOOLBAR_HEIGHT: i32 = 34;

#[derive(Default)]
struct TabInfo {
    title_w: Vec<u16>,
    hwnd: HWND,
    region: HRGN,
    left: i32,
}

struct TabHostState {
    hwnd: HWND,
    parent: HWND,
    toolbar: HWND,
    tabs: Vec<TabInfo>,
    selected: usize,
    enable_multicast: bool,
    font_regular: HFONT,
    font_bold: HFONT,
}

pub fn register_class() {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            lpfnWndProc: Some(tab_host_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(TAB_HOST_CLASS).as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
}

pub fn create_tab_host(parent: HWND, enable_multicast: bool) -> HWND {
    unsafe {
        register_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let font_regular = CreateFontW(
            -15,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            PCWSTR(to_wstring("Georgia").as_ptr()),
        );
        let font_bold = CreateFontW(
            -15,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            PCWSTR(to_wstring("Georgia").as_ptr()),
        );
        let state = Box::new(TabHostState {
            hwnd: HWND(null_mut()),
            parent,
            toolbar: HWND(null_mut()),
            tabs: Vec::new(),
            selected: 0,
            enable_multicast,
            font_regular,
            font_bold,
        });
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TRANSPARENT.0),
            PCWSTR(to_wstring(TAB_HOST_CLASS).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0,
            ),
            0,
            0,
            100,
            100,
            parent,
            HMENU(null_mut()),
            HINSTANCE(hinstance.0),
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create tab host")
    }
}

pub fn add_tab(host: HWND, title: &str, hwnd: HWND) {
    unsafe {
        if let Some(state) = state(host) {
            let mut title_w = to_wstring(title);
            // Keep storage aligned with Win32 expectations (null-terminated).
            if !title_w.ends_with(&[0]) {
                title_w.push(0);
            }
            SetParent(hwnd, host);
            state.tabs.push(TabInfo {
                title_w,
                hwnd,
                region: HRGN::default(),
                left: 0,
            });
            if state.tabs.len() == 1 {
                state.selected = 0;
                let _ = ShowWindow(hwnd, SW_SHOW);
            } else {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            layout(state);
        }
    }
}

pub fn toggle_toolbar_keyboard_mode(host: HWND) {
    unsafe {
        let _ = PostMessageW(host, WM_TOGGLE_TOOLBAR_KB, WPARAM(0), LPARAM(0));
    }
}

unsafe extern "system" fn tab_host_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut TabHostState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);
                state.toolbar = create_toolbar(hwnd, state.enable_multicast);
                layout(state);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state(hwnd) {
                layout(state);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = state(hwnd) {
                handle_click(state, GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam));
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(state) = state(hwnd) {
                paint(state);
            }
            LRESULT(0)
        }
        WM_TOGGLE_TOOLBAR_KB => {
            if let Some(state) = state(hwnd) {
                toggle_keyboard_mode(state.toolbar);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(state_ptr) = detach_state(hwnd) {
                cleanup(state_ptr);
            }
            LRESULT(0)
        }
        // Forward toolbar commands up to the main window.
        windows::Win32::UI::WindowsAndMessaging::WM_COMMAND => {
            if let Some(state) = state(hwnd) {
                let _ = PostMessageW(state.parent, msg, wparam, lparam);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn handle_click(state: &mut TabHostState, x: i32, y: i32) {
    if state.tabs.is_empty() {
        return;
    }
    ensure_regions(state);
    for (idx, tab) in state.tabs.iter().enumerate() {
        if idx != state.selected && PtInRegion(tab.region, x, y).as_bool() {
            select_tab(state, idx);
            break;
        }
    }
}

unsafe fn paint(state: &mut TabHostState) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(state.hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;

    // Background.
    SelectObject(dc, GetStockObject(DC_BRUSH));
    SelectObject(dc, GetStockObject(DC_PEN));
    SetDCBrushColor(dc, COLORREF(0x00efefef));
    SetDCPenColor(dc, COLORREF(0x00efefef));
    Rectangle(dc, 0, 0, width, height);

    // Toolbar swoosh with gradient.
    SetDCBrushColor(dc, COLORREF(0x000000b5));
    render_toolbar_path(dc, width);
    let swoosh_rgn = PathToRegion(dc);
    let mut swoosh_bounds = RECT::default();
    GetRgnBox(swoosh_rgn, &mut swoosh_bounds);
    SelectClipRgn(dc, swoosh_rgn);
    let verts = [
        TRIVERTEX {
            x: swoosh_bounds.left,
            y: swoosh_bounds.top,
            Red: 0xaa00,
            Green: 0xaa00,
            Blue: 0xc000,
            Alpha: 0x0000,
        },
        TRIVERTEX {
            x: swoosh_bounds.right,
            y: swoosh_bounds.bottom,
            Red: 0xef00,
            Green: 0xef00,
            Blue: 0xef00,
            Alpha: 0x0000,
        },
    ];
    let g_rect = [GRADIENT_RECT {
        UpperLeft: 0,
        LowerRight: 1,
    }];
    let _ = GradientFill(
        dc,
        &verts,
        g_rect.as_ptr() as *const _,
        g_rect.len() as u32,
        GRADIENT_FILL_RECT_H,
    );
    SelectClipRgn(dc, HRGN::default());
    let _ = DeleteObject(swoosh_rgn);

    if !state.tabs.is_empty() {
        ensure_regions(state);
        // Draw unselected tabs from back to front.
        for idx in (0..state.tabs.len()).rev() {
            if idx != state.selected {
                draw_tab(dc, state, idx, false);
            }
        }
        // Draw selected tab last so it sits on top.
        draw_tab(dc, state, state.selected, true);
    }

    let _ = EndPaint(state.hwnd, &ps);
}

unsafe fn draw_tab(dc: HDC, state: &TabHostState, idx: usize, selected: bool) {
    let tab = &state.tabs[idx];
    let brush = HBRUSH(GetStockObject(DC_BRUSH).0);
    let old_font = SelectObject(
        dc,
        if selected {
            state.font_bold
        } else {
            state.font_regular
        },
    );
    SelectObject(dc, GetStockObject(DC_BRUSH));
    SelectObject(dc, GetStockObject(DC_PEN));
    let tab_color = if selected { 0x00cccccc } else { 0x00777777 };
    let outline_color = 0x00909090;
    SetDCPenColor(dc, COLORREF(outline_color));
    SetDCBrushColor(dc, COLORREF(tab_color));
    FillRgn(dc, tab.region, brush);
    SetDCBrushColor(dc, COLORREF(tab_color + 0x00080808));
    FrameRgn(dc, tab.region, brush, 3, 2);

    let mut bounds = RECT::default();
    GetRgnBox(tab.region, &mut bounds);
    let mut size = SIZE::default();
    let title_len = tab.title_w.len().saturating_sub(1) as i32;
    let text_slice = &tab.title_w[..title_len as usize];
    GetTextExtentPoint32W(dc, text_slice, &mut size);
    let text_x = ((bounds.right - bounds.left - size.cx) / 2) + bounds.left;
    let text_y = bounds.top + TAB_TEXT_OFFSET_Y;
    SetBkMode(dc, TRANSPARENT);
    let _ = ExtTextOutW(
        dc,
        text_x,
        text_y,
        windows::Win32::Graphics::Gdi::ETO_OPTIONS(0),
        Some(&bounds as *const RECT),
        PCWSTR(tab.title_w.as_ptr()),
        title_len as u32,
        None,
    );
    let _ = SelectObject(dc, old_font);
}

unsafe fn render_toolbar_path(dc: HDC, width: i32) {
    let mut last_x = (width / 2) + 10;
    BeginPath(dc);
    MoveToEx(dc, 0, 36, None);
    LineTo(dc, last_x, 36);

    let pf1 = [
        windows::Win32::Foundation::POINT {
            x: last_x + 30,
            y: 26,
        },
        windows::Win32::Foundation::POINT {
            x: last_x,
            y: 12,
        },
        windows::Win32::Foundation::POINT {
            x: last_x + 40,
            y: 8,
        },
    ];
    PolyBezierTo(dc, &pf1);

    last_x += 120;
    LineTo(dc, last_x + 40, 8);
    last_x += 40;
    LineTo(dc, last_x, 0);
    LineTo(dc, 0, 0);
    CloseFigure(dc);
    EndPath(dc);
}

unsafe fn ensure_regions(state: &mut TabHostState) {
    let dc = GetDC(state.hwnd);
    for tab in &mut state.tabs {
        if tab.region.0 != std::ptr::null_mut() {
            let _ = DeleteObject(tab.region);
            tab.region = HRGN::default();
        }
        tab.region = render_tab_region(dc, tab.left);
    }
    let _ = ReleaseDC(state.hwnd, dc);
}

unsafe fn render_tab_region(dc: HDC, xoffset: i32) -> HRGN {
    let mut pt_orig = windows::Win32::Foundation::POINT { x: 0, y: 0 };
    BeginPath(dc);
    OffsetViewportOrgEx(dc, xoffset, 0, Some(&mut pt_orig));
    MoveToEx(dc, 0, TAB_BASELINE, None);
    let curve1 = [
        windows::Win32::Foundation::POINT { x: 10, y: 20 },
        windows::Win32::Foundation::POINT { x: 30, y: 16 },
        windows::Win32::Foundation::POINT { x: 40, y: 16 },
    ];
    PolyBezierTo(dc, &curve1);
    LineTo(dc, 40 + TAB_WIDTH, 16);
    let curve2 = [
        windows::Win32::Foundation::POINT {
            x: 70 + TAB_WIDTH,
            y: 20,
        },
        windows::Win32::Foundation::POINT {
            x: 70 + TAB_WIDTH,
            y: 40,
        },
        windows::Win32::Foundation::POINT {
            x: 80 + TAB_WIDTH,
            y: 40,
        },
    ];
    PolyBezierTo(dc, &curve2);
    SetViewportOrgEx(dc, pt_orig.x, pt_orig.y, None);
    EndPath(dc);
    PathToRegion(dc)
}

unsafe fn layout(state: &mut TabHostState) {
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;

    if state.toolbar.0 != std::ptr::null_mut() {
        let _ = MoveWindow(state.toolbar, 3, 3, width / 2, TOOLBAR_HEIGHT, true);
    }

    let mut x = (width / 2) + 80;
    for tab in &mut state.tabs {
        tab.left = x;
        x += TAB_SPACING;
    }
    ensure_regions(state);

    // Position tab children below the tab strip.
    for (idx, tab) in state.tabs.iter().enumerate() {
        let show = idx == state.selected;
        let _ = MoveWindow(
            tab.hwnd,
            0,
            TAB_BASELINE + 4,
            width,
            height - (TAB_BASELINE + 4),
            false,
        );
        let _ = ShowWindow(tab.hwnd, if show { SW_SHOW } else { SW_HIDE });
    }
    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(state.hwnd, None, false);
}

unsafe fn select_tab(state: &mut TabHostState, idx: usize) {
    if idx >= state.tabs.len() || idx == state.selected {
        return;
    }
    let prev = state.selected;
    state.selected = idx;
    if let Some(prev_tab) = state.tabs.get(prev) {
        let _ = ShowWindow(prev_tab.hwnd, SW_HIDE);
    }
    if let Some(new_tab) = state.tabs.get(idx) {
        let _ = ShowWindow(new_tab.hwnd, SW_SHOW);
        let _ = SetFocus(new_tab.hwnd);
    }
    let _ = windows::Win32::Graphics::Gdi::InvalidateRect(state.hwnd, None, true);
}

unsafe fn state(hwnd: HWND) -> Option<&'static mut TabHostState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut TabHostState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut TabHostState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut TabHostState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

unsafe fn cleanup(ptr: *mut TabHostState) {
    let boxed = Box::from_raw(ptr);
    if boxed.font_regular.0 != std::ptr::null_mut() {
        let _ = DeleteObject(boxed.font_regular);
    }
    if boxed.font_bold.0 != std::ptr::null_mut() {
        let _ = DeleteObject(boxed.font_bold);
    }
    for tab in boxed.tabs {
        if tab.region.0 != std::ptr::null_mut() {
            let _ = DeleteObject(tab.region);
        }
    }
}

#[inline]
fn GET_X_LPARAM(lp: LPARAM) -> i32 {
    (lp.0 as u32 & 0xFFFF) as i16 as i32
}
#[inline]
fn GET_Y_LPARAM(lp: LPARAM) -> i32 {
    ((lp.0 as u32 >> 16) & 0xFFFF) as i16 as i32
}
