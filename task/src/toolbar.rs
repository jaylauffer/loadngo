use std::{ptr::null_mut, sync::OnceLock};

use windows::{
    core::{implement, PCWSTR, PWSTR},
    Win32::{
        Foundation::{HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, POINTL, RECT, WPARAM},
        Graphics::Gdi::{
            AlphaBlend, BeginPaint, BeginPath, CloseFigure, CreateCompatibleDC, DeleteDC,
            DeleteObject, EndPaint, EndPath, FillRect, GetRgnBox, GetStockObject, GradientFill,
            InvalidateRect, MapWindowPoints, PathToRegion, PolyBezierTo, ScreenToClient,
            SelectClipRgn, SelectObject, SetDCBrushColor, SetDCPenColor, MoveToEx, LineTo,
            BLENDFUNCTION, GRADIENT_FILL_RECT_H, GRADIENT_RECT, HBITMAP, HBRUSH, HDC, HRGN,
            PAINTSTRUCT, TRIVERTEX, WHITE_BRUSH, DC_BRUSH, DC_PEN, AC_SRC_ALPHA, AC_SRC_OVER,
        },
        System::{
            Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL},
            DataExchange::RegisterClipboardFormatW,
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalLock, GlobalUnlock},
            Ole::{
                RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_UNICODETEXT, IDropTarget,
                IDropTarget_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
            },
            SystemServices::MODIFIERKEYS_FLAGS,
        },
        UI::{
            Controls::{TTTOOLINFOW, TOOLTIPS_CLASSW, TTF_SUBCLASS, TTS_ALWAYSTIP, TTS_NOPREFIX},
            Input::KeyboardAndMouse::{
                TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_LEFT, VK_RIGHT, VK_SPACE,
                ReleaseCapture, SetCapture, SetFocus,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, LoadCursorW,
                LoadImageW, PostMessageW, RegisterClassW, SendMessageW, SetWindowLongPtrW,
                ShowWindow, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWL_USERDATA,
                HMENU, IMAGE_BITMAP, LR_CREATEDIBSECTION, LR_DEFAULTCOLOR, LR_SHARED, SW_HIDE,
                WNDCLASSW, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
                WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETFOCUS,
                WM_SIZE, WM_USER, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_TRANSPARENT,
                WS_VISIBLE, WINDOW_EX_STYLE, WINDOW_STYLE, IDC_ARROW,
            },
        },
    },
};

use crate::winutil::{to_wstring, MAKELONG};

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

struct ButtonState {
    def: ButtonDef,
    bmp: HBITMAP,
    rect: RECT,
    hover: bool,
    pressed: bool,
    has_focus: bool,
    tip_w: Vec<u16>,
}

struct ToolbarState {
    hwnd: HWND,
    parent: HWND,
    defs: Vec<ButtonDef>,
    buttons: Vec<ButtonState>,
    tooltip: HWND,
    keyboard_mode: bool,
    trash_bmp: HBITMAP,
    trash_rect: RECT,
    drop_target: Option<IDropTarget>,
}

const WM_MOUSELEAVE: u32 = 0x02A3;
const TRASH_SIZE: i32 = 26;

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
            buttons: Vec::new(),
        tooltip: HWND(null_mut()),
        keyboard_mode: false,
        trash_bmp: HBITMAP(null_mut()),
        trash_rect: RECT::default(),
        drop_target: None,
    });
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TRANSPARENT.0),
            PCWSTR(to_wstring(TOOLBAR_CLASS).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(
                WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0,
            ),
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
                track_mouse(hwnd);
                handle_hover(state, GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam));
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(state) = state(hwnd) {
                clear_hover(state);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = state(hwnd) {
                SetCapture(hwnd);
                press(state, GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam));
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = state(hwnd) {
                release(state, GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam));
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
                for b in &mut state.buttons {
                    b.has_focus = false;
                }
                InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
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
                    for b in &mut state.buttons {
                        b.has_focus = false;
                    }
                    InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
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
        let mut rect = RECT::default();
        rect.left = x;
        rect.top = 2;
        rect.right = x + 26;
        rect.bottom = rect.top + 26;
        state.buttons.push(ButtonState {
            def: def.clone(),
            bmp: HBITMAP(bmp.0),
            rect,
            hover: false,
            pressed: false,
            has_focus: false,
            tip_w: to_wstring(def.tip),
        });
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
    state.trash_bmp = HBITMAP(trash_bmp.0);

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
    for (i, btn) in state.buttons.iter().enumerate() {
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
    for btn in &mut state.buttons {
        btn.rect.left = x;
        btn.rect.top = 2;
        btn.rect.right = x + 26;
        btn.rect.bottom = btn.rect.top + 26;
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

unsafe fn paint(state: &ToolbarState) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(state.hwnd, &mut ps);
    // Clear the toolbar area so alpha-blended icons don't accumulate on top of
    // previous frames (otherwise every repaint makes all buttons look hovered).
    let mut rc = RECT::default();
    GetClientRect(state.hwnd, &mut rc);
    FillRect(dc, &rc, HBRUSH(GetStockObject(WHITE_BRUSH).0));
    for btn in &state.buttons {
        paint_button(dc, btn);
    }
    paint_trash(dc, state);
    EndPaint(state.hwnd, &ps);
}

unsafe fn paint_button(dc: HDC, btn: &ButtonState) {
    let width = btn.rect.right - btn.rect.left;
    let height = btn.rect.bottom - btn.rect.top;
    let hdc_btn = CreateCompatibleDC(dc);
    let old = SelectObject(hdc_btn, btn.bmp);
    let bf = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: if btn.hover || btn.has_focus { 0xff } else { 0xbf },
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    AlphaBlend(
        dc,
        btn.rect.left + 2,
        btn.rect.top + 2,
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
    AlphaBlend(
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

unsafe fn handle_hover(state: &mut ToolbarState, x: i32, y: i32) {
    let mut changed = false;
    for btn in &mut state.buttons {
        let inside = point_in_rect(x, y, &btn.rect);
        if btn.hover != inside {
            btn.hover = inside;
            changed = true;
        }
    }
    if changed {
        InvalidateRect(state.hwnd, None, false);
    }
}

unsafe fn clear_hover(state: &mut ToolbarState) {
    let mut changed = false;
    for btn in &mut state.buttons {
        if btn.hover {
            btn.hover = false;
            changed = true;
        }
    }
    if changed {
        InvalidateRect(state.hwnd, None, false);
    }
}

unsafe fn press(state: &mut ToolbarState, x: i32, y: i32) {
    for btn in &mut state.buttons {
        btn.pressed = point_in_rect(x, y, &btn.rect);
    }
    InvalidateRect(state.hwnd, None, false);
}

unsafe fn release(state: &mut ToolbarState, x: i32, y: i32) {
    let mut fire: Option<i32> = None;
    for btn in &mut state.buttons {
        if btn.pressed && point_in_rect(x, y, &btn.rect) {
            fire = Some(btn.def.id);
        }
        btn.pressed = false;
    }
    InvalidateRect(state.hwnd, None, false);
    let _ = ReleaseCapture();
    if let Some(id) = fire {
        let wp = WPARAM(MAKELONG(2, 3) as usize);
        let lp = LPARAM(id as isize);
        PostMessageW(state.parent, WM_COMMAND, wp, lp);
    }
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
            let btn = &state.buttons[idx];
            let wp = WPARAM(MAKELONG(2, 3) as usize);
            let lp = LPARAM(btn.def.id as isize);
            PostMessageW(state.parent, WM_COMMAND, wp, lp);
        }
        true
    } else {
        false
    }
}

unsafe fn focus_prev(state: &mut ToolbarState) {
    if let Some(idx) = current_focus(state) {
        let new_idx = if idx == 0 {
            state.buttons.len() - 1
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
        let new_idx = (idx + 1) % state.buttons.len();
        set_focus_button(state, new_idx);
    } else {
        set_focus_button(state, 0);
    }
}

unsafe fn current_focus(state: &ToolbarState) -> Option<usize> {
    state.buttons.iter().position(|b| b.has_focus)
}

unsafe fn set_focus_button(state: &mut ToolbarState, idx: usize) {
    for (i, b) in state.buttons.iter_mut().enumerate() {
        b.has_focus = i == idx;
    }
    InvalidateRect(state.hwnd, None, false);
    SetFocus(state.hwnd);
}

unsafe fn point_in_rect(x: i32, y: i32, rc: &RECT) -> bool {
    x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
}

unsafe fn track_mouse(hwnd: HWND) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = TrackMouseEvent(&mut tme);
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
        RegisterClipboardFormatW(PCWSTR(
            to_wstring("SpecTimeEntry ID Format").as_ptr(),
        )) as u16
    })
}

fn format_actual_entry() -> u16 {
    static CF: OnceLock<u16> = OnceLock::new();
    *CF.get_or_init(|| unsafe {
        RegisterClipboardFormatW(PCWSTR(
            to_wstring("ActualTimeEntry ID Format").as_ptr(),
        )) as u16
    })
}

fn format_annotation() -> u16 {
    static CF: OnceLock<u16> = OnceLock::new();
    *CF.get_or_init(|| unsafe {
        RegisterClipboardFormatW(PCWSTR(
            to_wstring("Annotation ID Format").as_ptr(),
        )) as u16
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
            let mut p = windows::Win32::Foundation::POINT {
                x: pt.x,
                y: pt.y,
            };
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
    for btn in boxed.buttons {
        if !btn.bmp.0.is_null() {
            let _ = DeleteObject(btn.bmp);
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
