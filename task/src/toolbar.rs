use std::{ptr::null_mut, sync::OnceLock};

use windows::{
    core::{implement, PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            COLORREF, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, POINTL, RECT, WPARAM,
        },
        Graphics::Gdi::{
            AlphaBlend, BeginPaint, BeginPath, BitBlt, CloseFigure, CreateCompatibleDC, DeleteDC,
            DeleteObject, DrawEdge, EndPaint, EndPath, FillRect, GetDC, GetObjectW, GetRgnBox,
            GetStockObject, GradientFill, InvalidateRect, LineTo, MapWindowPoints, MoveToEx,
            OffsetWindowOrgEx, PathToRegion, PolyBezierTo, ReleaseDC, ScreenToClient,
            SelectClipRgn, SelectObject, SetDCBrushColor, SetDCPenColor, SetWindowOrgEx,
            AC_SRC_ALPHA, AC_SRC_OVER, BF_RECT, BITMAP, BLENDFUNCTION, DC_BRUSH, DC_PEN,
            EDGE_ETCHED, EDGE_SUNKEN, GRADIENT_FILL_RECT_H, GRADIENT_RECT, HBITMAP, HBRUSH, HDC,
            HRGN, PAINTSTRUCT, SRCCOPY, TRIVERTEX, WHITE_BRUSH,
        },
        System::{
            Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL},
            DataExchange::RegisterClipboardFormatW,
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalLock, GlobalUnlock},
            Ole::{
                IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
                CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
            },
            SystemServices::MODIFIERKEYS_FLAGS,
        },
        UI::{
            Controls::{TOOLTIPS_CLASSW, TTF_SUBCLASS, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW},
            Input::KeyboardAndMouse::{
                ReleaseCapture, SetCapture, SetFocus, VK_LEFT, VK_RIGHT, VK_SPACE,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, KillTimer,
                LoadCursorW, LoadImageW, PostMessageW, RegisterClassW, SendMessageW, SetTimer,
                SetWindowLongPtrW, ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
                CW_USEDEFAULT, GWL_USERDATA, HMENU, IDC_ARROW, IMAGE_BITMAP, LR_CREATEDIBSECTION,
                LR_DEFAULTCOLOR, LR_SHARED, PRF_CHILDREN, PRF_CLIENT, PRF_ERASEBKGND, SW_HIDE,
                WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_ERASEBKGND,
                WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
                WM_PRINTCLIENT, WM_SETFOCUS, WM_SIZE, WM_TIMER, WM_USER, WNDCLASSW, WS_CHILD,
                WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TRANSPARENT, WS_VISIBLE,
            },
        },
    },
};

use crate::winutil::{to_wstring, MAKELONG};
use gui::{BufferedWnd, Component, Container};

pub const TOOLBAR_CLASS: &str = "LNGToolbar";

pub const IDB_TBNEWTASK: u16 = 218;
pub const IDB_TBSAVEPLAN: u16 = 221;
pub const IDB_TBMAKEREPORT: u16 = 222;
pub const IDB_TBTASKSYNCH: u16 = 223;
pub const IDB_TBTRASHCAN: u16 = 224;
pub const IDB_TBPRINT: u16 = 235;

pub const TBCREATETASK: i32 = 1;
pub const TBSAVEPLAN: i32 = 3;
pub const TBMAKEREPORT: i32 = 4;
pub const TBSYNCHRONIZE: i32 = 5;
pub const TBPRINT: i32 = 6;

const WM_TOGGLE_KEYBOARD: u32 = WM_USER + 1;
pub const WM_DELETE_TASK: u32 = WM_USER + 0x200;

#[derive(Clone)]
struct ButtonDef {
    id: i32,
    bitmap_res: u16,
    tip: &'static str,
}

struct ToolbarState {
    hwnd: HWND,
    parent: HWND,
    defs: Vec<ButtonDef>,
    container: Container,
    tooltip: HWND,
    keyboard_mode: bool,
    trash_bmp: HBITMAP,
    trash_rect: RECT,
    drop_target: Option<IDropTarget>,
    buffer: BufferedWnd,
}

const WM_MOUSELEAVE: u32 = 0x02A3;
const TRASH_SIZE: i32 = 26;

struct ToolbarButton {
    host_hwnd: HWND,
    parent_cmd_hwnd: HWND,
    def: ButtonDef,
    bmp: HBITMAP,
    rect: RECT,
    hover: bool,
    pressed: bool,
    has_focus: bool,
    tip_w: Vec<u16>,
}

impl ToolbarButton {
    fn paint(&self, dc: HDC) {
        unsafe {
            let width = self.rect.right - self.rect.left;
            let height = self.rect.bottom - self.rect.top;
            let hdc_btn = CreateCompatibleDC(dc);
            let old = SelectObject(hdc_btn, self.bmp);
            let bf = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: if self.hover || self.has_focus || self.pressed {
                    0xff
                } else {
                    0xbf
                },
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = AlphaBlend(
                dc,
                self.rect.left + 2,
                self.rect.top + 2,
                width - 4,
                height - 4,
                hdc_btn,
                0,
                0,
                width - 4,
                height - 4,
                bf,
            );
            SelectObject(hdc_btn, old);
            DeleteDC(hdc_btn);

            if self.hover || self.has_focus || self.pressed {
                let mut rc = self.rect;
                let edge = if self.pressed {
                    EDGE_SUNKEN
                } else {
                    EDGE_ETCHED
                };
                let _ = DrawEdge(dc, &mut rc, edge, BF_RECT);
            }
        }
    }
}

impl Component for ToolbarButton {
    fn hwnd(&self) -> HWND {
        self.host_hwnd
    }

    fn bounds(&self) -> RECT {
        self.rect
    }

    fn set_bounds(&mut self, rect: RECT) {
        self.rect = rect;
    }

    fn handle_message(&mut self, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let x = GET_X_LPARAM(lparam);
        let y = GET_Y_LPARAM(lparam);
        match msg {
            WM_MOUSEMOVE => LRESULT(0),
            WM_LBUTTONDOWN => {
                self.pressed = true;
                unsafe {
                    let _ = InvalidateRect(self.host_hwnd, Some(&self.rect), false);
                    let _ = SetCapture(self.host_hwnd);
                }
                LRESULT(1)
            }
            WM_LBUTTONUP => {
                let fire = self.pressed && point_in_rect(x, y, &self.rect);
                self.pressed = false;
                unsafe {
                    let _ = ReleaseCapture();
                    let _ = InvalidateRect(self.host_hwnd, Some(&self.rect), false);
                }
                if fire {
                    unsafe {
                        let wp = WPARAM(MAKELONG(2, 3) as usize);
                        let lp = LPARAM(self.def.id as isize);
                        let _ = PostMessageW(self.parent_cmd_hwnd, WM_COMMAND, wp, lp);
                    }
                }
                LRESULT(1)
            }
            _ => LRESULT(0),
        }
    }

    fn mouse_entered(&mut self) {
        self.hover = true;
        unsafe {
            let _ = InvalidateRect(self.host_hwnd, Some(&self.rect), false);
        }
    }

    fn mouse_exited(&mut self) {
        self.hover = false;
        self.pressed = false;
        unsafe {
            let _ = InvalidateRect(self.host_hwnd, Some(&self.rect), false);
        }
    }

    fn focus_changed(&mut self, gained: bool) {
        self.has_focus = gained;
        unsafe {
            let _ = InvalidateRect(self.host_hwnd, Some(&self.rect), false);
        }
    }

    fn id(&self) -> i32 {
        self.def.id
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub fn register_class() {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(toolbar_wndproc),
            hInstance: HINSTANCE(hinstance.0),
            lpszClassName: PCWSTR(to_wstring(TOOLBAR_CLASS).as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(null_mut()),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
}

pub fn create_toolbar(parent: HWND, enable_multicast: bool) -> HWND {
    unsafe {
        register_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(ToolbarState {
            hwnd: HWND(null_mut()),
            parent,
            defs: build_buttons(enable_multicast),
            container: Container::new(HWND(null_mut())),
            tooltip: HWND(null_mut()),
            keyboard_mode: false,
            trash_bmp: HBITMAP(null_mut()),
            trash_rect: RECT::default(),
            drop_target: None,
            buffer: BufferedWnd::new(),
        });
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TRANSPARENT.0),
            PCWSTR(to_wstring(TOOLBAR_CLASS).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
            0,
            0,
            200,
            30,
            parent,
            HMENU(null_mut()),
            HINSTANCE(hinstance.0),
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create toolbar")
    }
}

pub fn toggle_keyboard_mode(hwnd: HWND) {
    unsafe {
        PostMessageW(hwnd, WM_TOGGLE_KEYBOARD, WPARAM(0), LPARAM(0));
    }
}

fn build_buttons(enable_multicast: bool) -> Vec<ButtonDef> {
    let mut defs = vec![
        ButtonDef {
            id: TBCREATETASK,
            bitmap_res: IDB_TBNEWTASK,
            tip: "New Task",
        },
        ButtonDef {
            id: TBSAVEPLAN,
            bitmap_res: IDB_TBSAVEPLAN,
            tip: "Save All",
        },
        ButtonDef {
            id: TBMAKEREPORT,
            bitmap_res: IDB_TBMAKEREPORT,
            tip: "Generate Report",
        },
    ];
    if enable_multicast {
        defs.push(ButtonDef {
            id: TBSYNCHRONIZE,
            bitmap_res: IDB_TBTASKSYNCH,
            tip: "Network Synchronization",
        });
    }
    defs.push(ButtonDef {
        id: TBPRINT,
        bitmap_res: IDB_TBPRINT,
        tip: "Print",
    });
    defs
}

unsafe extern "system" fn toolbar_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut ToolbarState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);
                init_toolbar(state);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state(hwnd) {
                layout(state);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(state) = state(hwnd) {
                paint(state);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if let Some(state) = state(hwnd) {
                let _ = state.container.handle_message(msg, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = state(hwnd) {
                let _ = state.container.handle_message(msg, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = state(hwnd) {
                let _ = state.container.handle_message(msg, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if let Some(state) = state(hwnd) {
                if handle_key(state, wparam) {
                    return LRESULT(0);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETFOCUS => {
            if let Some(state) = state(hwnd) {
                state.keyboard_mode = true;
                set_focus_button(state, 0);
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            if let Some(state) = state(hwnd) {
                state.keyboard_mode = false;
                for child in &mut state.container.children {
                    if let Some(btn) = child.as_any_mut().downcast_mut::<ToolbarButton>() {
                        btn.has_focus = false;
                    }
                }
                InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        // Paint the parent background so the transparent toolbar blends in.
        WM_ERASEBKGND => {
            if let Some(state) = state(hwnd) {
                let dc = HDC(wparam.0 as *mut _);
                let mut pt = POINT { x: 0, y: 0 };
                let mut pts = [pt];
                MapWindowPoints(hwnd, state.parent, &mut pts);
                pt = pts[0];
                let mut old = POINT::default();
                OffsetWindowOrgEx(dc, pt.x, pt.y, Some(&mut old));
                let _ = SendMessageW(
                    state.parent,
                    WM_ERASEBKGND,
                    WPARAM(dc.0 as usize),
                    LPARAM(0),
                );
                SetWindowOrgEx(dc, old.x, old.y, None);
                return LRESULT(1);
            }
            LRESULT(1)
        }
        WM_DESTROY => {
            if let Some(ptr) = detach_state(hwnd) {
                cleanup(ptr);
            }
            LRESULT(0)
        }
        WM_TOGGLE_KEYBOARD => {
            if let Some(state) = state(hwnd) {
                state.keyboard_mode = !state.keyboard_mode;
                if state.keyboard_mode {
                    set_focus_button(state, 0);
                } else {
                    for child in &mut state.container.children {
                        if let Some(btn) = child.as_any_mut().downcast_mut::<ToolbarButton>() {
                            btn.has_focus = false;
                        }
                    }
                    InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_NCHITTEST => {
            // Ensure we still receive mouse messages even with WS_EX_TRANSPARENT.
            LRESULT(windows::Win32::UI::WindowsAndMessaging::HTCLIENT as isize)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn init_toolbar(state: &mut ToolbarState) {
    let hinstance = GetModuleHandleW(None).unwrap();
    let mut x = 2;
    for def in state.defs.clone() {
        let bmp = LoadImageW(
            HINSTANCE(hinstance.0),
            PCWSTR(def.bitmap_res as usize as *const u16),
            IMAGE_BITMAP,
            0,
            0,
            LR_CREATEDIBSECTION | LR_DEFAULTCOLOR | LR_SHARED,
        )
        .unwrap();
        premultiply_bitmap(HBITMAP(bmp.0));
        let mut rect = RECT::default();
        rect.left = x;
        rect.top = 2;
        rect.right = x + 26;
        rect.bottom = rect.top + 26;
        let btn = ToolbarButton {
            host_hwnd: state.hwnd,
            parent_cmd_hwnd: state.parent,
            def: def.clone(),
            bmp: HBITMAP(bmp.0),
            rect,
            hover: false,
            pressed: false,
            has_focus: false,
            tip_w: to_wstring(def.tip),
        };
        state.container.add(Box::new(btn));
        x += 28;
    }
    let trash_bmp = LoadImageW(
        HINSTANCE(hinstance.0),
        PCWSTR(IDB_TBTRASHCAN as usize as *const u16),
        IMAGE_BITMAP,
        0,
        0,
        LR_CREATEDIBSECTION | LR_DEFAULTCOLOR | LR_SHARED,
    )
    .unwrap_or_default();
    let hbmp = HBITMAP(trash_bmp.0);
    premultiply_bitmap(hbmp);
    state.trash_bmp = hbmp;

    let tooltip = CreateWindowExW(
        WINDOW_EX_STYLE(WS_EX_TRANSPARENT.0),
        TOOLTIPS_CLASSW,
        PCWSTR::null(),
        WINDOW_STYLE((TTS_NOPREFIX | TTS_ALWAYSTIP) as u32),
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        state.hwnd,
        HMENU(null_mut()),
        HINSTANCE(hinstance.0),
        None,
    )
    .unwrap();
    state.tooltip = tooltip;
    SendMessageW(tooltip, 0x0418, WPARAM(state.hwnd.0 as usize), LPARAM(0));
    add_tools(state);

    // Enable OLE drop for the toolbar (used by the trash can).
    let target: IDropTarget = ToolbarDropTarget {
        hwnd: state.hwnd,
        state: state as *mut ToolbarState,
    }
    .into();
    if RegisterDragDrop(state.hwnd, &target).is_ok() {
        state.drop_target = Some(target);
    }
}

unsafe fn add_tools(state: &ToolbarState) {
    for (i, child) in state.container.children.iter().enumerate() {
        let Some(btn) = child.as_any().downcast_ref::<ToolbarButton>() else {
            continue;
        };
        let mut ti = TTTOOLINFOW::default();
        ti.cbSize = std::mem::size_of::<TTTOOLINFOW>() as u32;
        ti.uFlags = TTF_SUBCLASS;
        ti.hwnd = state.hwnd;
        ti.uId = (i + 1) as usize;
        let mut pts = [
            POINT {
                x: btn.rect.left,
                y: btn.rect.top,
            },
            POINT {
                x: btn.rect.right,
                y: btn.rect.bottom,
            },
        ];
        MapWindowPoints(state.hwnd, HWND(null_mut()), &mut pts);
        ti.rect.left = pts[0].x;
        ti.rect.top = pts[0].y;
        ti.rect.right = pts[1].x;
        ti.rect.bottom = pts[1].y;
        ti.lpszText = PWSTR(btn.tip_w.as_ptr() as *mut _);
        SendMessageW(
            state.tooltip,
            0x0432,
            WPARAM(0),
            LPARAM(&ti as *const _ as isize),
        );
    }
}

unsafe fn layout(state: &mut ToolbarState) {
    let mut rc = RECT::default();
    GetClientRect(state.hwnd, &mut rc);
    let mut x = 2;
    for child in &mut state.container.children {
        if let Some(btn) = child.as_any_mut().downcast_mut::<ToolbarButton>() {
            btn.rect.left = x;
            btn.rect.top = 2;
            btn.rect.right = x + 26;
            btn.rect.bottom = btn.rect.top + 26;
        }
        x += 28;
    }
    let toolbar_width = rc.right - rc.left;
    let trash_left = (toolbar_width - 2 - TRASH_SIZE).max(x);
    state.trash_rect.left = trash_left;
    state.trash_rect.top = 2;
    state.trash_rect.right = trash_left + TRASH_SIZE;
    state.trash_rect.bottom = state.trash_rect.top + TRASH_SIZE;
    InvalidateRect(state.hwnd, None, false);
}

unsafe fn paint(state: &mut ToolbarState) {
    let state_ptr = state as *mut ToolbarState;
    let buffer = &mut state.buffer;
    let _ = buffer.paint(state.hwnd, move |_, dc, width, height| {
        let state = unsafe { &mut *state_ptr };
        // Let the parent paint show through (transparent background).
        let mut pt = POINT { x: 0, y: 0 };
        let mut pts = [pt];
        MapWindowPoints(state.hwnd, state.parent, &mut pts);
        pt = pts[0];
        // Preferred: grab pixels directly from parent DC to avoid WM_PRINTCLIENT
        // returning nothing for some parents.
        let parent_dc = GetDC(state.parent);
        if !parent_dc.0.is_null() {
            let _ = BitBlt(dc, 0, 0, width, height, parent_dc, pt.x, pt.y, SRCCOPY);
            let _ = ReleaseDC(state.parent, parent_dc);
        } else {
            // Fallback: ask parent to render its client.
            let mut old = POINT { x: 0, y: 0 };
            let _ = OffsetWindowOrgEx(dc, pt.x, pt.y, Some(&mut old));
            let flags = (PRF_CLIENT | PRF_ERASEBKGND | PRF_CHILDREN) as isize;
            let _ = SendMessageW(
                state.parent,
                WM_PRINTCLIENT,
                WPARAM(dc.0 as usize),
                LPARAM(flags),
            );
            let _ = SetWindowOrgEx(dc, old.x, old.y, None);
        }

        // Draw toolbar content.
        for child in &state.container.children {
            if let Some(btn) = child.as_any().downcast_ref::<ToolbarButton>() {
                paint_button(dc, btn);
            }
        }
        paint_trash(dc, state);
        // width/height currently unused but kept for parity with legacy.
        let _ = (width, height);
        Ok(())
    });
}

unsafe fn paint_button(dc: HDC, btn: &ToolbarButton) {
    btn.paint(dc);
}

unsafe fn paint_trash(dc: HDC, state: &ToolbarState) {
    if state.trash_bmp.0.is_null() {
        return;
    }
    let width = state.trash_rect.right - state.trash_rect.left;
    let height = state.trash_rect.bottom - state.trash_rect.top;
    let hdc_trash = CreateCompatibleDC(dc);
    let old = SelectObject(hdc_trash, state.trash_bmp);
    let bf = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 0xbf,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _ = AlphaBlend(
        dc,
        state.trash_rect.left + 2,
        state.trash_rect.top + 2,
        width - 4,
        height - 4,
        hdc_trash,
        0,
        0,
        width - 4,
        height - 4,
        bf,
    );
    SelectObject(hdc_trash, old);
    DeleteDC(hdc_trash);
}

unsafe fn handle_key(state: &mut ToolbarState, wparam: WPARAM) -> bool {
    if !state.keyboard_mode {
        return false;
    }
    let key = wparam.0 as i32;
    if key == VK_LEFT.0 as i32 {
        focus_prev(state);
        true
    } else if key == VK_RIGHT.0 as i32 {
        focus_next(state);
        true
    } else if key == VK_SPACE.0 as i32 {
        if let Some(idx) = current_focus(state) {
            if let Some(btn) = state.container.children[idx]
                .as_any()
                .downcast_ref::<ToolbarButton>()
            {
                let wp = WPARAM(MAKELONG(2, 3) as usize);
                let lp = LPARAM(btn.def.id as isize);
                PostMessageW(state.parent, WM_COMMAND, wp, lp);
            }
        }
        true
    } else {
        false
    }
}

unsafe fn focus_prev(state: &mut ToolbarState) {
    if let Some(idx) = current_focus(state) {
        if state.container.children.is_empty() {
            return;
        }
        let new_idx = if idx == 0 {
            state.container.children.len() - 1
        } else {
            idx - 1
        };
        set_focus_button(state, new_idx);
    } else {
        set_focus_button(state, 0);
    }
}

unsafe fn focus_next(state: &mut ToolbarState) {
    if let Some(idx) = current_focus(state) {
        if state.container.children.is_empty() {
            return;
        }
        let new_idx = (idx + 1) % state.container.children.len();
        set_focus_button(state, new_idx);
    } else {
        set_focus_button(state, 0);
    }
}

unsafe fn current_focus(state: &ToolbarState) -> Option<usize> {
    state.container.children.iter().position(|b| {
        b.as_any()
            .downcast_ref::<ToolbarButton>()
            .map(|b| b.has_focus)
            .unwrap_or(false)
    })
}

unsafe fn set_focus_button(state: &mut ToolbarState, idx: usize) {
    for (i, child) in state.container.children.iter_mut().enumerate() {
        if let Some(btn) = child.as_any_mut().downcast_mut::<ToolbarButton>() {
            btn.has_focus = i == idx;
        }
    }
    let _ = InvalidateRect(state.hwnd, None, false);
    let _ = SetFocus(state.hwnd);
}

fn point_in_rect(x: i32, y: i32, rc: &RECT) -> bool {
    x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
}

unsafe fn premultiply_bitmap(bmp: HBITMAP) {
    if bmp.0.is_null() {
        return;
    }
    let mut info = BITMAP::default();
    let got = GetObjectW(
        bmp,
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut info as *mut _ as *mut _),
    );
    if got == 0 || info.bmBits.is_null() || info.bmBitsPixel != 32 {
        return;
    }
    let width = info.bmWidth as usize;
    let height = info.bmHeight as usize;
    let stride = info.bmWidthBytes as usize;
    let buf = std::slice::from_raw_parts_mut(info.bmBits as *mut u8, stride * height);
    for y in 0..height {
        let row = y * stride;
        for x in 0..width {
            let p = row + x * 4;
            let b = buf[p];
            let g = buf[p + 1];
            let r = buf[p + 2];
            let a = buf[p + 3];
            if a == 0xff {
                continue;
            }
            buf[p] = ((b as u16 * a as u16) / 255) as u8;
            buf[p + 1] = ((g as u16 * a as u16) / 255) as u8;
            buf[p + 2] = ((r as u16 * a as u16) / 255) as u8;
            // alpha unchanged
        }
    }
}

unsafe fn state(hwnd: HWND) -> Option<&'static mut ToolbarState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut ToolbarState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut ToolbarState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut ToolbarState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

// --- Drag/drop + trash can support ---

fn format_task() -> u16 {
    static CF: OnceLock<u16> = OnceLock::new();
    *CF.get_or_init(|| unsafe {
        RegisterClipboardFormatW(PCWSTR(to_wstring("loadngo::data::task").as_ptr())) as u16
    })
}

fn format_spec_entry() -> u16 {
    static CF: OnceLock<u16> = OnceLock::new();
    *CF.get_or_init(|| unsafe {
        RegisterClipboardFormatW(PCWSTR(to_wstring("SpecTimeEntry ID Format").as_ptr())) as u16
    })
}

fn format_actual_entry() -> u16 {
    static CF: OnceLock<u16> = OnceLock::new();
    *CF.get_or_init(|| unsafe {
        RegisterClipboardFormatW(PCWSTR(to_wstring("ActualTimeEntry ID Format").as_ptr())) as u16
    })
}

fn format_annotation() -> u16 {
    static CF: OnceLock<u16> = OnceLock::new();
    *CF.get_or_init(|| unsafe {
        RegisterClipboardFormatW(PCWSTR(to_wstring("Annotation ID Format").as_ptr())) as u16
    })
}

fn supported_formats() -> &'static [u16; 5] {
    // Include text as a fallback so manual drops still work during development.
    static FORMATS: OnceLock<[u16; 5]> = OnceLock::new();
    FORMATS.get_or_init(|| {
        [
            format_task(),
            format_spec_entry(),
            format_actual_entry(),
            format_annotation(),
            CF_UNICODETEXT.0 as u16,
        ]
    })
}

#[implement(windows::Win32::System::Ole::IDropTarget)]
struct ToolbarDropTarget {
    hwnd: HWND,
    state: *mut ToolbarState,
}

impl ToolbarDropTarget_Impl {
    fn state(&self) -> Option<&mut ToolbarState> {
        unsafe { self.state.as_mut() }
    }

    fn point_in_trash(&self, pt: &POINTL) -> bool {
        if let Some(state) = self.state() {
            let mut p = windows::Win32::Foundation::POINT { x: pt.x, y: pt.y };
            unsafe {
                ScreenToClient(self.hwnd, &mut p);
            }
            unsafe { point_in_rect(p.x, p.y, &state.trash_rect) }
        } else {
            false
        }
    }

    fn choose_effect(&self, data_obj: &IDataObject, pt: &POINTL) -> DROPEFFECT {
        if self.point_in_trash(pt) && has_supported_format(data_obj) {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        }
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for ToolbarDropTarget_Impl {
    fn DragEnter(
        &self,
        p_data_obj: Option<&IDataObject>,
        _grf_key_state: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdw_effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if let Some(eff) = unsafe { pdw_effect.as_mut() } {
            *eff = DROPEFFECT_NONE;
            if let Some(data_obj) = p_data_obj {
                *eff = self.choose_effect(data_obj, pt);
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grf_key_state: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdw_effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if let Some(eff) = unsafe { pdw_effect.as_mut() } {
            *eff = if self.point_in_trash(pt) {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            };
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        p_data_obj: Option<&IDataObject>,
        _grf_key_state: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdw_effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if let Some(eff) = unsafe { pdw_effect.as_mut() } {
            *eff = DROPEFFECT_NONE;
        }
        if !self.point_in_trash(pt) {
            return Ok(());
        }
        if let Some(data_obj) = p_data_obj {
            if let Some(id) = extract_drop_id(data_obj) {
                if let Some(state) = self.state() {
                    unsafe {
                        let _ = PostMessageW(
                            state.parent,
                            WM_DELETE_TASK,
                            WPARAM(id as usize),
                            LPARAM(0),
                        );
                    }
                    if let Some(eff) = unsafe { pdw_effect.as_mut() } {
                        *eff = DROPEFFECT_COPY;
                    }
                }
            }
        }
        Ok(())
    }
}

fn has_supported_format(data_obj: &IDataObject) -> bool {
    supported_formats().iter().any(|cf| can_get(data_obj, *cf))
}

fn can_get(data_obj: &IDataObject, cf: u16) -> bool {
    let mut format = FORMATETC::default();
    format.cfFormat = cf;
    format.ptd = std::ptr::null_mut();
    format.dwAspect = DVASPECT_CONTENT.0 as u32;
    format.lindex = -1;
    format.tymed = TYMED_HGLOBAL.0 as u32;
    unsafe { data_obj.QueryGetData(&format).is_ok() }
}

fn extract_drop_id(data_obj: &IDataObject) -> Option<u64> {
    for cf in supported_formats() {
        if let Some(id) = extract_id_with_format(data_obj, *cf) {
            return Some(id);
        }
    }
    None
}

fn extract_id_with_format(data_obj: &IDataObject, cf: u16) -> Option<u64> {
    let mut format = FORMATETC::default();
    format.cfFormat = cf;
    format.ptd = std::ptr::null_mut();
    format.dwAspect = DVASPECT_CONTENT.0 as u32;
    format.lindex = -1;
    format.tymed = TYMED_HGLOBAL.0 as u32;
    let mut medium = unsafe { data_obj.GetData(&format).ok()? };
    let handle: HGLOBAL = unsafe { medium.u.hGlobal };
    if handle.0.is_null() {
        return None;
    }
    let id = unsafe {
        let locked = GlobalLock(handle);
        if locked.is_null() {
            return None;
        }
        let value = *(locked as *const u64);
        GlobalUnlock(handle);
        value
    };
    unsafe { ReleaseStgMedium(&mut medium) };
    Some(id)
}

unsafe fn cleanup(ptr: *mut ToolbarState) {
    let boxed = Box::from_raw(ptr);
    for child in boxed.container.children {
        if let Some(btn) = child.as_any().downcast_ref::<ToolbarButton>() {
            if !btn.bmp.0.is_null() {
                let _ = DeleteObject(btn.bmp);
            }
        }
    }
    if !boxed.tooltip.0.is_null() {
        ShowWindow(boxed.tooltip, SW_HIDE);
    }
    if boxed.drop_target.is_some() {
        let _ = RevokeDragDrop(boxed.hwnd);
    }
    if !boxed.trash_bmp.0.is_null() {
        let _ = DeleteObject(boxed.trash_bmp);
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

// Helper to draw the toolbar background swoosh.
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
        windows::Win32::Foundation::POINT { x: last_x, y: 12 },
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

// Developer note:
// Hover still sticks on some systems despite WM_MOUSELEAVE handling. We removed
// WS_EX_TRANSPARENT and added timer-based hover refresh plus explicit
// invalidations, but OS-level capture interference seems to keep hover lit.
// When resuming, consider instrumenting WM_MOUSEMOVE/LEAVE arrival or falling
// back to focus/press-only highlighting.
