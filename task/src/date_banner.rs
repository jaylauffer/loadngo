use std::ptr::null_mut;

use anyhow::Result;
use chrono::Local;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, GetStockObject, Rectangle, SelectObject,
    SetBkMode, SetDCBrushColor, SetDCPenColor, SetTextColor, TextOutW, HDC, HFONT, LF_FACESIZE,
    LOGFONTW, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, LoadCursorW, RegisterClassW,
    SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWL_USERDATA, HMENU,
    IDC_ARROW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_PAINT,
    WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_VISIBLE,
};

use crate::winutil::to_wstring;

const CLASS_NAME: &str = "LNGDateBanner";
const BUTTON_WIDTH: i32 = 40;
const BG_PRESENT: COLORREF = COLORREF(0x00ffffff);
const BG_FUTURE: COLORREF = COLORREF(0x00ddffee);
const BG_PAST: COLORREF = COLORREF(0x00ddeedd);

pub fn register_class() -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            hbrBackground: CreateSolidBrush(BG_PRESENT),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
    Ok(())
}

pub fn create_date_banner(parent: HWND) -> HWND {
    unsafe {
        register_class().expect("register date banner");
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(BannerState { hwnd: HWND::default(), font: create_banner_font() });
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            32,
            parent,
            HMENU(null_mut()),
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create date banner")
    }
}

struct BannerState {
    hwnd: HWND,
    font: HFONT,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let ptr = cs.lpCreateParams as *mut BannerState;
            if !ptr.is_null() {
                (*ptr).hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
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
            if let Some(ptr) = take_state(hwnd) {
                if !(*ptr).font.is_invalid() {
                    let _ = DeleteObject((*ptr).font);
                }
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn get_state(hwnd: HWND) -> Option<&'static mut BannerState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut BannerState;
    ptr.as_mut()
}

unsafe fn take_state(hwnd: HWND) -> Option<*mut BannerState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut BannerState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

unsafe fn paint(state: &mut BannerState) {
    let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
    let dc = windows::Win32::Graphics::Gdi::BeginPaint(state.hwnd, &mut ps);
    if dc.0.is_null() {
        return;
    }
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);

    let bg = pick_bg();
    SetDCBrushColor(dc, bg);
    SetDCPenColor(dc, bg);
    let _ = Rectangle(dc, 0, 0, rc.right, rc.bottom);

    // Left/right pseudo-buttons as outlined squares with text.
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, COLORREF(0));
    let mut btn_rc = RECT { left: 0, top: 0, right: BUTTON_WIDTH, bottom: rc.bottom };
    draw_button(dc, &btn_rc, "<");
    btn_rc.left = rc.right - BUTTON_WIDTH;
    btn_rc.right = rc.right;
    draw_button(dc, &btn_rc, ">");

    let old_font = SelectObject(dc, state.font);
    let date_str = current_date_string();
    let mut w = to_wstring(&date_str);
    if !w.is_empty() {
        w.pop();
    }
    let text_x = BUTTON_WIDTH + 8;
    let text_y = (rc.bottom - rc.top) / 2 - 10;
    let _ = TextOutW(dc, text_x, text_y, &w);
    let _ = SelectObject(dc, old_font);

    let _ = windows::Win32::Graphics::Gdi::EndPaint(state.hwnd, &mut ps);
}

unsafe fn draw_button(dc: HDC, rc: &RECT, text: &str) {
    let old_brush = SelectObject(dc, GetStockObject(WHITE_BRUSH));
    let _ = Rectangle(dc, rc.left, rc.top, rc.right, rc.bottom);
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, COLORREF(0x00303030));
    let mut w = to_wstring(text);
    if !w.is_empty() {
        w.pop();
    }
    let x = rc.left + 12;
    let y = (rc.bottom - rc.top) / 2 - 10;
    let _ = TextOutW(dc, x, y, &w);
    let _ = SelectObject(dc, old_brush);
}

fn current_date_string() -> String {
    Local::now().format("%A, %B %d, %Y").to_string()
}

unsafe fn pick_bg() -> COLORREF {
    // Simple present-day color; a more faithful port would compare service active date.
    BG_PRESENT
}

unsafe fn create_banner_font() -> HFONT {
    let mut lf: LOGFONTW = std::mem::zeroed();
    lf.lfCharSet = windows::Win32::Graphics::Gdi::DEFAULT_CHARSET;
    lf.lfClipPrecision = windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS;
    lf.lfOutPrecision = windows::Win32::Graphics::Gdi::OUT_TT_ONLY_PRECIS;
    lf.lfQuality = windows::Win32::Graphics::Gdi::ANTIALIASED_QUALITY;
    lf.lfPitchAndFamily = (windows::Win32::Graphics::Gdi::DEFAULT_PITCH.0
        | windows::Win32::Graphics::Gdi::FF_DONTCARE.0) as u8;
    lf.lfHeight = -28;
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
