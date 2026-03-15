use std::os::windows::ffi::OsStrExt;

use ui_core::{
    geometry::{Color, Point, Rect},
    input::{Key, Modifiers, PointerButton, PointerSource, PointerState},
    paint::{PaintOp, TextStyle},
};
use windows::Win32::Foundation::{COLORREF, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetStockObject, Rectangle,
    SelectObject, SetTextColor, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HBRUSH,
    HDC, PS_SOLID,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};

/// Convert a Rust string into a null-terminated UTF-16 buffer.
pub fn to_wstring(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn point_from_lparam(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam.0 & 0xffff) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
    }
}

pub fn rect_to_core(rect: RECT) -> Rect {
    Rect {
        x: rect.left,
        y: rect.top,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    }
}

pub fn rect_from_core(rect: Rect) -> RECT {
    RECT {
        left: rect.x,
        top: rect.y,
        right: rect.x + rect.width,
        bottom: rect.y + rect.height,
    }
}

pub fn point_to_core(point: POINT) -> Point {
    Point {
        x: point.x,
        y: point.y,
    }
}

pub fn primary_pointer(point: POINT) -> PointerState {
    PointerState::mouse(point_to_core(point), Modifiers::default())
}

pub fn rgb(color: Color) -> COLORREF {
    COLORREF((color.r as u32) | ((color.g as u32) << 8) | ((color.b as u32) << 16))
}

pub fn render_paint_ops(dc: HDC, ops: &[PaintOp]) {
    unsafe {
        for op in ops {
            match op {
                PaintOp::FillRect { rect, color } => {
                    let win_rect = rect_from_core(*rect);
                    let brush = CreateSolidBrush(rgb(*color));
                    let _ = FillRect(dc, &win_rect, brush);
                    let _ = DeleteObject(brush);
                }
                PaintOp::StrokeRect { rect, color } => {
                    let pen = CreatePen(PS_SOLID, 1, rgb(*color));
                    let old_pen = SelectObject(dc, pen);
                    let _ = Rectangle(dc, rect.x, rect.y, rect.right(), rect.bottom());
                    let _ = SelectObject(dc, old_pen);
                    let _ = DeleteObject(pen);
                }
                PaintOp::Text { rect, text, style } => render_text(dc, *rect, text, style),
                PaintOp::Line { from, to, color } => {
                    let pen = CreatePen(PS_SOLID, 1, rgb(*color));
                    let old_pen = SelectObject(dc, pen);
                    let _ = windows::Win32::Graphics::Gdi::MoveToEx(dc, from.x, from.y, None);
                    let _ = windows::Win32::Graphics::Gdi::LineTo(dc, to.x, to.y);
                    let _ = SelectObject(dc, old_pen);
                    let _ = DeleteObject(pen);
                }
                PaintOp::BlitImage { .. } => {}
            }
        }
    }
}

fn render_text(dc: HDC, rect: Rect, text: &str, style: &TextStyle) {
    unsafe {
        let _ = SetTextColor(dc, rgb(style.color));
        let old_font = SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        let mut rect = rect_from_core(rect);
        let mut flags = DT_VCENTER | DT_SINGLELINE;
        if style.centered {
            flags |= DT_CENTER;
        }
        let mut buf = to_wstring(text);
        if !buf.is_empty() {
            buf.pop();
        }
        let _ = DrawTextW(dc, &mut buf, &mut rect, flags);
        let _ = SelectObject(dc, old_font);
    }
}

pub fn pointer_pressed_event(point: POINT) -> ui_core::input::UiEvent {
    ui_core::input::UiEvent::PointerPressed {
        button: PointerButton::Primary,
        state: primary_pointer(point),
    }
}

pub fn pointer_released_event(point: POINT) -> ui_core::input::UiEvent {
    ui_core::input::UiEvent::PointerReleased {
        button: PointerButton::Primary,
        state: primary_pointer(point),
    }
}

pub fn key_from_wparam(wparam: usize) -> Option<Key> {
    match wparam as u32 {
        code if code == VK_LEFT.0 as u32 => Some(Key::Left),
        code if code == VK_RIGHT.0 as u32 => Some(Key::Right),
        code if code == VK_UP.0 as u32 => Some(Key::Up),
        code if code == VK_DOWN.0 as u32 => Some(Key::Down),
        code if code == VK_RETURN.0 as u32 => Some(Key::Enter),
        code if code == VK_SPACE.0 as u32 => Some(Key::Space),
        _ => None,
    }
}
