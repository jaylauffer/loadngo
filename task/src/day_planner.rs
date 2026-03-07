use std::ptr::null_mut;
use std::sync::Once;

use anyhow::Result;
use data::entity::Entity;
use data::model_utils::{generate_id, now_timestamp, UNITS_PER_HOUR};
use data::service::Service;
use data::task::{EntryKind, TimeEntry};
use gui::buffered::BufferedWnd;
use gui::component::Component;
use gui::container::Container;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, FILETIME, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawEdge, DrawFocusRect, FillRect, GetObjectW, GetPixel,
    GetStockObject, GradientFill, LineTo, MoveToEx, ScreenToClient, SelectObject, SetBkMode,
    SetTextColor, StretchDIBits, TextOutW, AC_SRC_OVER, BDR_RAISEDOUTER, BF_RECT, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIBSECTION, DIB_RGB_COLORS, GRADIENT_FILL_RECT_H,
    GRADIENT_RECT, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ, LF_FACESIZE, LOGFONTW, SRCCOPY,
    TRANSPARENT, TRIVERTEX, WHITE_BRUSH,
};
use windows::Win32::Storage::FileSystem::{FileTimeToLocalFileTime, LocalFileTimeToFileTime};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN,
    VK_SHIFT,
};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, FindWindowExW, GetClientRect, GetCursorPos,
    GetScrollInfo, GetWindowLongPtrW, GetWindowTextW, LoadCursorW, LoadImageW, MoveWindow,
    RegisterClassW, SendMessageW, SetCursor, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
    ShowWindow, CBN_SELENDOK, CBS_DROPDOWN, CB_ADDSTRING, CB_RESETCONTENT, CREATESTRUCTW,
    CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, GWLP_WNDPROC, GWL_USERDATA, HMENU, HWND_TOP, IDC_ARROW,
    IDC_SIZEWE, IMAGE_BITMAP, LR_CREATEDIBSECTION, LR_SHARED, SCROLLBAR_COMMAND, SCROLLINFO,
    SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS, SWP_SHOWWINDOW, SW_HIDE, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CAPTURECHANGED, WM_CHAR, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_ERASEBKGND,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY,
    WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_VSCROLL, WNDCLASSW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_CLIENTEDGE, WS_VISIBLE, WS_VSCROLL,
};

use crate::winutil::to_wstring;
use windows::core::implement;

const CLASS_NAME: &str = "DayPlanWnd";
const HOST_CLASS: &str = "DayPlanHostWnd";
const HEADER_WIDTH: i32 = 70;
const SPLITTER_BAR_WIDTH: i32 = 5;
const SPLITTER_QUICKTAB_WIDTH: i32 = 8;
const DEFAULT_SPLIT: f64 = 0.55;
const HOUR_FRACTION: f64 = 0.25; // 15-minute increments
const HOUR_FRACTION_PX: i32 = 18; // pixels per 15-minute increment (matches legacy spacing)
const MIN_PANE_WIDTH: i32 = 80;
const WM_MOUSELEAVE: u32 = 0x02A3;
const UNITS_PER_FRACTION: u64 = UNITS_PER_HOUR / 4;
const IDB_SPEC_TOP_RIGHT: u16 = 115;
const IDB_SPEC_BOTTOM_LEFT: u16 = 116;
const IDB_SPEC_BOTTOM_RIGHT: u16 = 117;
const IDB_SPEC_LEFT: u16 = 118;
const IDB_SPEC_RIGHT: u16 = 119;
const IDB_SPEC_TOP: u16 = 120;
const IDB_SPEC_TOP_LEFT: u16 = 121;
const IDB_SPEC_BOTTOM: u16 = 122;
const IDB_ACTUAL_TOP_RIGHT: u16 = 123;
const IDB_ACTUAL_BOTTOM_LEFT: u16 = 124;
const IDB_ACTUAL_BOTTOM_RIGHT: u16 = 125;
const IDB_ACTUAL_LEFT: u16 = 126;
const IDB_ACTUAL_RIGHT: u16 = 127;
const IDB_ACTUAL_TOP: u16 = 128;
const IDB_ACTUAL_TOP_LEFT: u16 = 129;
const IDB_ACTUAL_BOTTOM: u16 = 130;

fn format_task() -> u16 {
    static INIT: Once = Once::new();
    static mut CF: u16 = 0;
    unsafe {
        INIT.call_once(|| {
            CF =
                RegisterClipboardFormatW(PCWSTR(to_wstring("loadngo::data::task").as_ptr())) as u16;
        });
        CF
    }
}

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
    entries: Vec<TimeEntry>,
    selected_id: Option<u64>,
    drag: Option<EntryDragState>,
}

impl DayPlanPane {
    fn new(host_hwnd: HWND, kind: PaneKind) -> Self {
        Self {
            host_hwnd,
            rect: RECT::default(),
            kind,
            entries: Vec::new(),
            selected_id: None,
            drag: None,
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
        if let Some(state) = get_state(self.host_hwnd) {
            for entry in &self.entries {
                if let Some(rc) = entry_rect(entry, state, self.rect) {
                    if !rects_overlap(rc, self.rect) {
                        continue;
                    }
                    draw_entry(dc, entry, rc, self.kind, self.selected_id, state.font);
                }
            }
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

    fn handle_message(&mut self, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let pt = point_from_lparam(lparam);
        match msg {
            WM_LBUTTONDOWN => {
                if let Some(state) = unsafe { get_state(self.host_hwnd) } {
                    if let Some((idx, rc, resizing)) = hit_test_entry(self, state, pt) {
                        let entry_id = self.entries[idx].entity.id;
                        let offset_y = pt.y - rc.top;
                        self.selected_id = Some(entry_id);
                        self.drag = Some(EntryDragState {
                            entry_id,
                            offset_y,
                            mode: if resizing {
                                DragMode::Resize
                            } else {
                                DragMode::Move
                            },
                        });
                        return LRESULT(1);
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(state) = unsafe { get_state(self.host_hwnd) } {
                    if let Some(drag) = self.drag {
                        update_entry_drag(self, state, pt, &drag);
                        refresh(self.host_hwnd);
                        return LRESULT(1);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(state) = unsafe { get_state(self.host_hwnd) } {
                    if self.drag.take().is_some() {
                        refresh(self.host_hwnd);
                        return LRESULT(1);
                    }
                    begin_editor(state, self.kind, pt);
                    refresh(self.host_hwnd);
                    return LRESULT(1);
                }
                LRESULT(0)
            }
            _ => LRESULT(0),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[derive(Clone, Copy)]
enum DragMode {
    Move,
    Resize,
}

#[derive(Clone, Copy)]
struct EntryDragState {
    entry_id: u64,
    offset_y: i32,
    mode: DragMode,
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

struct DetailEditor {
    hwnd: HWND,
    last_rect: RECT,
    edit_hwnd: HWND,
}

impl DetailEditor {
    unsafe fn create(parent: HWND) -> Result<Self> {
        let cls = to_wstring("COMBOBOX");
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_CLIENTEDGE.0),
            PCWSTR(cls.as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | (CBS_DROPDOWN as u32) | WS_VSCROLL.0),
            0,
            0,
            160,
            24,
            parent,
            HMENU(null_mut()),
            None,
            None,
        )?;
        ShowWindow(hwnd, SW_HIDE);
        let edit_cls = to_wstring("Edit");
        let edit_hwnd = FindWindowExW(
            hwnd,
            HWND::default(),
            PCWSTR(edit_cls.as_ptr()),
            PCWSTR::null(),
        )
        .unwrap_or(HWND::default());
        Ok(Self {
            hwnd,
            last_rect: RECT::default(),
            edit_hwnd,
        })
    }

    unsafe fn attach_subclass(&mut self, host_hwnd: HWND, kind: PaneKind) {
        if self.edit_hwnd.0.is_null() {
            return;
        }
        let prev = GetWindowLongPtrW(self.edit_hwnd, GWLP_WNDPROC);
        let prev_proc: WNDPROC = std::mem::transmute(prev);
        let data = Box::new(EditorSubclassData {
            host_hwnd,
            kind,
            prev_proc,
        });
        SetWindowLongPtrW(self.edit_hwnd, GWLP_USERDATA, Box::into_raw(data) as isize);
        SetWindowLongPtrW(self.edit_hwnd, GWLP_WNDPROC, editor_subclass_proc as isize);
    }
}

struct EditorSubclassData {
    host_hwnd: HWND,
    kind: PaneKind,
    prev_proc: WNDPROC,
}

unsafe extern "system" fn editor_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EditorSubclassData;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let data = &mut *ptr;
    match msg {
        WM_KEYDOWN => match wparam.0 as u32 {
            k if k == VK_RETURN.0 as u32 => {
                if let Some(state) = get_state(data.host_hwnd) {
                    let suppress = if data.kind == PaneKind::Actual {
                        is_suppress_key()
                    } else {
                        true
                    };
                    commit_editor(state, data.kind, suppress);
                }
                return LRESULT(0);
            }
            k if k == VK_ESCAPE.0 as u32 => {
                if let Some(state) = get_state(data.host_hwnd) {
                    cancel_editor(state, data.kind);
                }
                return LRESULT(0);
            }
            _ => {}
        },
        WM_CHAR | WM_KEYUP => match wparam.0 as u32 {
            k if k == VK_RETURN.0 as u32 || k == VK_ESCAPE.0 as u32 => return LRESULT(0),
            _ => {}
        },
        WM_NCDESTROY => {
            let prev_proc = data.prev_proc;
            SetWindowLongPtrW(
                hwnd,
                GWLP_WNDPROC,
                prev_proc.map(|p| p as isize).unwrap_or(0),
            );
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(ptr));
            if let Some(proc) = prev_proc {
                return CallWindowProcW(Some(proc), hwnd, msg, wparam, lparam);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        _ => {}
    }
    if let Some(proc) = data.prev_proc {
        CallWindowProcW(Some(proc), hwnd, msg, wparam, lparam)
    } else {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}
struct SpecDetailEditor {
    editor: DetailEditor,
}

struct ActualDetailEditor {
    editor: DetailEditor,
}

struct DayPlannerState {
    hwnd: HWND,
    container: Container,
    service: *mut Service,
    split_percent: f64,
    splitter_dragging: bool,
    active_date: u64,
    start_hour_pos: f64,
    font: HFONT,
    header_font: HFONT,
    buffer: BufferedWnd,
    drop_target: Option<windows::Win32::System::Ole::IDropTarget>,
    spec_editor: Option<SpecDetailEditor>,
    actual_editor: Option<ActualDetailEditor>,
}

impl DayPlannerState {
    fn new(service: *mut Service) -> Self {
        Self {
            hwnd: HWND::default(),
            container: Container::new(HWND::default()),
            service,
            split_percent: DEFAULT_SPLIT,
            splitter_dragging: false,
            active_date: 0,
            start_hour_pos: 8.0, // default 8 AM
            font: HFONT::default(),
            header_font: HFONT::default(),
            buffer: BufferedWnd::new(),
            drop_target: None,
            spec_editor: None,
            actual_editor: None,
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
    lf.lfOutPrecision = windows::Win32::Graphics::Gdi::OUT_TT_PRECIS;
    lf.lfQuality = windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY;
    lf.lfPitchAndFamily = (windows::Win32::Graphics::Gdi::DEFAULT_PITCH.0
        | windows::Win32::Graphics::Gdi::FF_DONTCARE.0) as u8;
    lf.lfHeight = -12;
    lf.lfWeight = windows::Win32::Graphics::Gdi::FW_NORMAL.0 as i32;
    let face = to_wstring("Arial");
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

unsafe fn create_header_font() -> HFONT {
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
    for (i, ch) in face.iter().enumerate().take(LF_FACESIZE as usize) {
        lf.lfFaceName[i] = *ch;
        if *ch == 0 {
            break;
        }
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

unsafe fn create_editors(state: &mut DayPlannerState) {
    if state.spec_editor.is_none() {
        if let Ok(mut editor) = DetailEditor::create(state.hwnd) {
            editor.attach_subclass(state.hwnd, PaneKind::Spec);
            state.spec_editor = Some(SpecDetailEditor { editor });
        }
    }
    if state.actual_editor.is_none() {
        if let Ok(mut editor) = DetailEditor::create(state.hwnd) {
            editor.attach_subclass(state.hwnd, PaneKind::Actual);
            state.actual_editor = Some(ActualDetailEditor { editor });
        }
    }
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
    let _ = SetScrollInfo(
        state.hwnd,
        windows::Win32::UI::WindowsAndMessaging::SB_VERT,
        &si,
        true,
    );
}

unsafe fn update_page(state: &mut DayPlannerState, height: i32) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_PAGE,
        nPage: (height / HOUR_FRACTION_PX).max(1) as u32,
        ..Default::default()
    };
    let _ = SetScrollInfo(
        state.hwnd,
        windows::Win32::UI::WindowsAndMessaging::SB_VERT,
        &si,
        true,
    );
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
                state.header_font = create_header_font();
                SetWindowLongPtrW(hwnd, GWL_USERDATA, ptr as isize);
                init_scroll(state);
                create_children(state);
                create_editors(state);
                sync_entries_from_service(state);
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
        WM_COMMAND => {
            if let Some(state) = get_state(hwnd) {
                let code = ((wparam.0 >> 16) & 0xffff) as u16;
                if code == CBN_SELENDOK as u16 {
                    let src = HWND(lparam.0 as *mut _);
                    if let Some(editor) = state.spec_editor.as_ref() {
                        if editor.editor.hwnd == src {
                            commit_editor(state, PaneKind::Spec, true);
                            return LRESULT(0);
                        }
                    }
                    if let Some(editor) = state.actual_editor.as_ref() {
                        if editor.editor.hwnd == src {
                            let suppress = is_suppress_key();
                            commit_editor(state, PaneKind::Actual, suppress);
                            return LRESULT(0);
                        }
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN => {
            if let Some(state) = get_state(hwnd) {
                let focus = GetFocus();
                if let Some(editor) = state.spec_editor.as_ref() {
                    if editor.editor.hwnd == focus {
                        if wparam.0 as u32 == VK_RETURN.0 as u32 {
                            commit_editor(state, PaneKind::Spec, true);
                            return LRESULT(0);
                        }
                        if wparam.0 as u32 == VK_ESCAPE.0 as u32 {
                            cancel_editor(state, PaneKind::Spec);
                            return LRESULT(0);
                        }
                    }
                }
                if let Some(editor) = state.actual_editor.as_ref() {
                    if editor.editor.hwnd == focus {
                        if wparam.0 as u32 == VK_RETURN.0 as u32 {
                            let suppress = is_suppress_key();
                            commit_editor(state, PaneKind::Actual, suppress);
                            return LRESULT(0);
                        }
                        if wparam.0 as u32 == VK_ESCAPE.0 as u32 {
                            cancel_editor(state, PaneKind::Actual);
                            return LRESULT(0);
                        }
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
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
                if !(*ptr).header_font.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ((*ptr).header_font.0));
                }
                if (*ptr).drop_target.is_some() {
                    let _ = RevokeDragDrop(hwnd);
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

    let old_font = SelectObject(dc, state.header_font);
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

    let _ = SelectObject(dc, old_font);
    paint_components(dc, state);
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

fn entry_rect(entry: &TimeEntry, state: &DayPlannerState, pane: RECT) -> Option<RECT> {
    if entry.stop <= entry.start {
        return None;
    }
    let width = pane.right - pane.left;
    if width <= 4 {
        return None;
    }
    let initial_pos = (state.start_hour_pos / HOUR_FRACTION).floor() as i32;
    let start_offset = entry.start.saturating_sub(state.active_date);
    let pos = (start_offset / UNITS_PER_FRACTION) as i32;
    let y = (pos - initial_pos) * HOUR_FRACTION_PX;
    let duration = entry
        .stop
        .saturating_sub(entry.start)
        .max(UNITS_PER_FRACTION);
    let segments = (duration / UNITS_PER_FRACTION).max(1) as i32;
    let entry_height = segments * HOUR_FRACTION_PX;

    Some(RECT {
        left: pane.left + 2,
        top: pane.top + y,
        right: pane.right - 2,
        bottom: pane.top + y + entry_height,
    })
}

unsafe fn local_start_of_day() -> u64 {
    let utc_now = GetSystemTimeAsFileTime();
    let mut local_now = FILETIME::default();
    let _ = FileTimeToLocalFileTime(&utc_now, &mut local_now);

    let mut utc_from_local = FILETIME::default();
    let _ = LocalFileTimeToFileTime(&local_now, &mut utc_from_local);

    let mut sys = SYSTEMTIME::default();
    let _ = FileTimeToSystemTime(&utc_from_local, &mut sys);
    sys.wHour = 0;
    sys.wMinute = 0;
    sys.wSecond = 0;
    sys.wMilliseconds = 0;

    let mut utc_midnight = FILETIME::default();
    let _ = SystemTimeToFileTime(&sys, &mut utc_midnight);

    let utc_now_val = filetime_to_u64(utc_from_local);
    let utc_mid_val = filetime_to_u64(utc_midnight);
    let delta = utc_now_val.saturating_sub(utc_mid_val);
    let local_now_val = filetime_to_u64(local_now);
    local_now_val.saturating_sub(delta)
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

fn rects_overlap(a: RECT, b: RECT) -> bool {
    a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top
}

fn hit_test_entry(
    pane: &DayPlanPane,
    state: &DayPlannerState,
    pt: POINT,
) -> Option<(usize, RECT, bool)> {
    for (idx, entry) in pane.entries.iter().enumerate() {
        if let Some(rc) = entry_rect(entry, state, pane.rect) {
            if point_in_rect(pt, rc) {
                let resizing = pt.y >= rc.bottom - 7;
                return Some((idx, rc, resizing));
            }
        }
    }
    None
}

fn create_entry_at(pane: &mut DayPlanPane, state: &DayPlannerState, pt: POINT) {
    let start = time_from_point(state, pane.rect, pt.y);
    let stop = start + UNITS_PER_FRACTION;
    let (task_id, base_title) = default_entry_title(state);
    let title = match pane.kind {
        PaneKind::Actual => format!("Actual: {base_title}"),
        PaneKind::Spec => base_title,
    };
    let kind = match pane.kind {
        PaneKind::Spec => EntryKind::Spec,
        PaneKind::Actual => EntryKind::Actual,
    };
    add_time_entry(state, task_id, title, start, stop, kind);
}

fn update_entry_drag(
    pane: &mut DayPlanPane,
    state: &DayPlannerState,
    pt: POINT,
    drag: &EntryDragState,
) {
    let entry_id = drag.entry_id;
    let Some(entry) = entry_by_id(state, entry_id) else {
        return;
    };
    match drag.mode {
        DragMode::Move => {
            let y = pt.y - drag.offset_y;
            let new_start = time_from_point(state, pane.rect, y);
            let duration = entry
                .stop
                .saturating_sub(entry.start)
                .max(UNITS_PER_FRACTION);
            update_entry_time(state, entry_id, new_start, new_start + duration);
        }
        DragMode::Resize => {
            let mut new_stop = time_from_point(state, pane.rect, pt.y);
            if new_stop <= entry.start {
                new_stop = entry.start + UNITS_PER_FRACTION;
            } else {
                new_stop += UNITS_PER_FRACTION;
            }
            update_entry_time(state, entry_id, entry.start, new_stop);
        }
    }
}

fn time_from_point(state: &DayPlannerState, pane: RECT, y: i32) -> u64 {
    let y_in_pane = (y - pane.top).max(0);
    let screen_segment = if y_in_pane >= HOUR_FRACTION_PX {
        y_in_pane / HOUR_FRACTION_PX
    } else {
        0
    };
    let offset = (screen_segment as f64 * HOUR_FRACTION) + state.start_hour_pos;
    state.active_date + (offset * UNITS_PER_HOUR as f64) as u64
}

fn default_entry_title(state: &DayPlannerState) -> (u64, String) {
    if !state.service.is_null() {
        let service = unsafe { &*state.service };
        if let Some(task) = service.tasks.values().next() {
            return (task.entity.id, task.name.clone());
        }
    }
    (0, "New Entry".to_string())
}

fn begin_editor(state: &DayPlannerState, kind: PaneKind, pt: POINT) {
    let Some(editor) = editor_for_kind(state, kind) else {
        return;
    };
    let Some(pane_rect) = pane_rect_for_kind(state, kind) else {
        return;
    };
    let y_in_pane = (pt.y - pane_rect.top).max(0);
    let segment = if y_in_pane >= HOUR_FRACTION_PX {
        y_in_pane / HOUR_FRACTION_PX
    } else {
        0
    };
    let top = pane_rect.top + (segment * HOUR_FRACTION_PX) + 1;
    let rect = RECT {
        left: pane_rect.left,
        top,
        right: pane_rect.right,
        bottom: top + HOUR_FRACTION_PX - 1,
    };
    populate_editor(state, editor.hwnd);
    unsafe {
        SetWindowPos(
            editor.hwnd,
            HWND_TOP,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_SHOWWINDOW,
        );
        let _ = SetFocus(editor.hwnd);
        let _ = SetWindowTextW(editor.hwnd, PCWSTR::null());
    }
    if let Some(state_mut) = unsafe { get_state(state.hwnd) } {
        if let Some(editor_mut) = editor_for_kind_mut(state_mut, kind) {
            editor_mut.last_rect = rect;
        }
    }
}

fn populate_editor(state: &DayPlannerState, hwnd: HWND) {
    if state.service.is_null() {
        return;
    }
    let service = unsafe { &*state.service };
    let mut names: Vec<String> = service.tasks.values().map(|t| t.name.clone()).collect();
    names.sort();
    unsafe {
        let _ = SendMessageW(hwnd, CB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for name in names {
            let w = to_wstring(&name);
            let _ = SendMessageW(hwnd, CB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
        }
    }
}

fn editor_for_kind(state: &DayPlannerState, kind: PaneKind) -> Option<&DetailEditor> {
    match kind {
        PaneKind::Spec => state.spec_editor.as_ref().map(|e| &e.editor),
        PaneKind::Actual => state.actual_editor.as_ref().map(|e| &e.editor),
    }
}

fn editor_for_kind_mut(state: &mut DayPlannerState, kind: PaneKind) -> Option<&mut DetailEditor> {
    match kind {
        PaneKind::Spec => state.spec_editor.as_mut().map(|e| &mut e.editor),
        PaneKind::Actual => state.actual_editor.as_mut().map(|e| &mut e.editor),
    }
}

fn pane_rect_for_kind(state: &DayPlannerState, kind: PaneKind) -> Option<RECT> {
    state.container.children.iter().find_map(|child| {
        child
            .as_any()
            .downcast_ref::<DayPlanPane>()
            .filter(|pane| pane.kind == kind)
            .map(|pane| pane.rect)
    })
}

fn commit_editor(state: &DayPlannerState, kind: PaneKind, suppress: bool) {
    let Some(editor) = editor_for_kind(state, kind) else {
        return;
    };
    let text = editor_text(editor.hwnd);
    if text.is_empty() {
        cancel_editor(state, kind);
        return;
    }
    let task = find_or_create_task(state, &text);
    let (start, stop) = editor_time_range(state, kind);
    let entry_kind = match kind {
        PaneKind::Spec => EntryKind::Spec,
        PaneKind::Actual => EntryKind::Actual,
    };
    let title = if suppress {
        format!("(suppressed) {}", task.name)
    } else {
        task.name.clone()
    };
    add_time_entry(state, task.entity.id, title, start, stop, entry_kind);
    cancel_editor(state, kind);
}

fn editor_time_range(state: &DayPlannerState, kind: PaneKind) -> (u64, u64) {
    if let Some(editor) = editor_for_kind(state, kind) {
        let y = editor.last_rect.top;
        let rect = pane_rect_for_kind(state, kind).unwrap_or(editor.last_rect);
        let start = time_from_point(state, rect, y);
        return (start, start + UNITS_PER_FRACTION);
    }
    (state.active_date, state.active_date + UNITS_PER_FRACTION)
}

fn cancel_editor(state: &DayPlannerState, kind: PaneKind) {
    let Some(editor) = editor_for_kind(state, kind) else {
        return;
    };
    unsafe {
        ShowWindow(editor.hwnd, SW_HIDE);
    }
}

fn editor_text(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    buf.truncate(len);
    String::from_utf16_lossy(&buf)
}

fn find_or_create_task(state: &DayPlannerState, name: &str) -> data::task::Task {
    let service = unsafe { &mut *state.service };
    if let Some(task) = service.tasks.values().find(|t| t.name == name) {
        return task.clone();
    }
    let task = data::task::Task::spawn(name, "local-user", 1, 1, now_timestamp());
    service.add_task(task.clone());
    task
}

fn is_suppress_key() -> bool {
    unsafe {
        (GetKeyState(VK_CONTROL.0 as i32) & (0x8000u16 as i16)) != 0
            || (GetKeyState(VK_SHIFT.0 as i32) & (0x8000u16 as i16)) != 0
    }
}

fn entry_by_id(state: &DayPlannerState, entry_id: u64) -> Option<TimeEntry> {
    if state.service.is_null() {
        return None;
    }
    let service = unsafe { &*state.service };
    service.time_entries.get(&entry_id).cloned()
}

fn add_time_entry(
    state: &DayPlannerState,
    task_id: u64,
    title: String,
    start: u64,
    stop: u64,
    kind: EntryKind,
) {
    if state.service.is_null() {
        return;
    }
    let entry = TimeEntry {
        entity: Entity::new(generate_id(), generate_id(), "local", now_timestamp()),
        task_id,
        duration: stop.saturating_sub(start).max(UNITS_PER_FRACTION),
        start,
        stop,
        title,
        kind,
        notes: None,
    };
    let service = unsafe { &mut *state.service };
    service.time_entries.insert(entry.entity.id, entry);
    sync_entries_from_service(state);
}

fn update_entry_time(state: &DayPlannerState, entry_id: u64, start: u64, stop: u64) {
    if state.service.is_null() {
        return;
    }
    let service = unsafe { &mut *state.service };
    if let Some(entry) = service.time_entries.get_mut(&entry_id) {
        entry.start = start;
        entry.stop = stop.max(start + UNITS_PER_FRACTION);
        entry.duration = entry
            .stop
            .saturating_sub(entry.start)
            .max(UNITS_PER_FRACTION);
    }
    sync_entries_from_service(state);
}

fn sync_entries_from_service(state: &DayPlannerState) {
    if state.service.is_null() {
        return;
    }
    let active_date = unsafe { local_start_of_day() };
    let end_date = active_date.saturating_add(UNITS_PER_HOUR * 24);
    let entries: Vec<TimeEntry> = {
        let service = unsafe { &*state.service };
        service.time_entries.values().cloned().collect()
    };
    let mut spec_entries = Vec::new();
    let mut actual_entries = Vec::new();
    for entry in entries {
        if entry.stop <= active_date || entry.start >= end_date {
            continue;
        }
        match entry.kind {
            EntryKind::Spec => spec_entries.push(entry),
            EntryKind::Actual => actual_entries.push(entry),
        }
    }
    if let Some(state_mut) = unsafe { get_state(state.hwnd) } {
        state_mut.active_date = active_date;
        for child in state_mut.container.children.iter_mut() {
            if let Some(pane) = child.as_any_mut().downcast_mut::<DayPlanPane>() {
                pane.entries = if pane.kind == PaneKind::Spec {
                    spec_entries.clone()
                } else {
                    actual_entries.clone()
                };
            }
        }
    }
    refresh(state.hwnd);
}

fn draw_entry(
    dc: HDC,
    entry: &TimeEntry,
    rc: RECT,
    kind: PaneKind,
    selected: Option<u64>,
    font: HFONT,
) {
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;
    if width <= 0 || height <= 0 {
        return;
    }

    unsafe {
        let mem_dc = CreateCompatibleDC(dc);
        if mem_dc.is_invalid() {
            draw_entry_opaque(dc, entry, rc, kind, selected, font);
            return;
        }

        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0 as u32;
        info.bmiHeader.biSizeImage = (width * height * 4) as u32;

        let mut bits = std::ptr::null_mut();
        let dib = match CreateDIBSection(mem_dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) => dib,
            Err(_) => {
                let _ = DeleteDC(mem_dc);
                draw_entry_opaque(dc, entry, rc, kind, selected, font);
                return;
            }
        };

        let old = SelectObject(mem_dc, HGDIOBJ(dib.0));
        let local = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        draw_entry_opaque(mem_dc, entry, local, kind, selected, font);

        let bf = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 0xBF,
            AlphaFormat: 0,
        };
        let _ = AlphaBlend(
            dc, rc.left, rc.top, width, height, mem_dc, 0, 0, width, height, bf,
        );

        let _ = SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem_dc);
    }
}

fn draw_entry_opaque(
    dc: HDC,
    entry: &TimeEntry,
    rc: RECT,
    kind: PaneKind,
    selected: Option<u64>,
    font: HFONT,
) {
    let mut old_font = HGDIOBJ::default();
    unsafe {
        if !font.is_invalid() {
            old_font = SelectObject(dc, font);
        }
    }

    paint_border(dc, rc, kind, selected == Some(entry.entity.id));

    unsafe {
        let color = GetPixel(dc, rc.left + 5, rc.top + 5);
        let brush = CreateSolidBrush(color);
        let fill_rect = RECT {
            left: rc.left + 6,
            top: rc.top + 6,
            right: rc.right - 6,
            bottom: rc.bottom - 6,
        };
        let _ = FillRect(dc, &fill_rect, brush);
        let _ = DeleteObject(brush);
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = SetTextColor(dc, COLORREF(0x00000000));
    }

    let mut w = to_wstring(&entry.title);
    if !w.is_empty() {
        w.pop();
    }
    unsafe {
        let _ = TextOutW(dc, rc.left + 8, rc.top + 2, &w);
    }

    unsafe {
        if !old_font.0.is_null() {
            let _ = SelectObject(dc, old_font);
        }
    }
}

struct BorderBitmaps {
    top_left: HBITMAP,
    top_right: HBITMAP,
    bottom_left: HBITMAP,
    bottom_right: HBITMAP,
    left: HBITMAP,
    right: HBITMAP,
    top: HBITMAP,
    bottom: HBITMAP,
}

struct BorderSets {
    spec: BorderBitmaps,
    actual: BorderBitmaps,
}

fn border_bitmaps() -> Option<&'static BorderSets> {
    static INIT: Once = Once::new();
    static mut SETS: Option<BorderSets> = None;
    unsafe {
        INIT.call_once(|| {
            let sets = (|| -> Result<BorderSets> {
                let hinstance = GetModuleHandleW(None)?;
                let load = |id: u16| -> Result<HBITMAP> {
                    let handle = LoadImageW(
                        hinstance,
                        PCWSTR(id as usize as *const u16),
                        IMAGE_BITMAP,
                        6,
                        6,
                        LR_SHARED | LR_CREATEDIBSECTION,
                    )?;
                    Ok(HBITMAP(handle.0))
                };
                Ok(BorderSets {
                    spec: BorderBitmaps {
                        top_left: load(IDB_SPEC_TOP_LEFT)?,
                        top_right: load(IDB_SPEC_TOP_RIGHT)?,
                        bottom_left: load(IDB_SPEC_BOTTOM_LEFT)?,
                        bottom_right: load(IDB_SPEC_BOTTOM_RIGHT)?,
                        left: load(IDB_SPEC_LEFT)?,
                        right: load(IDB_SPEC_RIGHT)?,
                        top: load(IDB_SPEC_TOP)?,
                        bottom: load(IDB_SPEC_BOTTOM)?,
                    },
                    actual: BorderBitmaps {
                        top_left: load(IDB_ACTUAL_TOP_LEFT)?,
                        top_right: load(IDB_ACTUAL_TOP_RIGHT)?,
                        bottom_left: load(IDB_ACTUAL_BOTTOM_LEFT)?,
                        bottom_right: load(IDB_ACTUAL_BOTTOM_RIGHT)?,
                        left: load(IDB_ACTUAL_LEFT)?,
                        right: load(IDB_ACTUAL_RIGHT)?,
                        top: load(IDB_ACTUAL_TOP)?,
                        bottom: load(IDB_ACTUAL_BOTTOM)?,
                    },
                })
            })();
            if let Ok(sets) = sets {
                SETS = Some(sets);
            }
        });
        SETS.as_ref()
    }
}

fn paint_border(dc: HDC, rc: RECT, kind: PaneKind, focused: bool) {
    let Some(sets) = border_bitmaps() else {
        unsafe {
            let _ = DrawEdge(dc, &mut rc.clone(), BDR_RAISEDOUTER, BF_RECT);
        }
        return;
    };
    let bitmaps = match kind {
        PaneKind::Spec => &sets.spec,
        PaneKind::Actual => &sets.actual,
    };
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;
    if w <= 12 || h <= 12 {
        unsafe {
            let _ = DrawEdge(dc, &mut rc.clone(), BDR_RAISEDOUTER, BF_RECT);
        }
        return;
    }
    draw_bitmap(dc, bitmaps.top_left, rc.left, rc.top, 6, 6);
    draw_bitmap(dc, bitmaps.top_right, rc.right - 6, rc.top, 6, 6);
    draw_bitmap(dc, bitmaps.bottom_right, rc.right - 6, rc.bottom - 6, 6, 6);
    draw_bitmap(dc, bitmaps.bottom_left, rc.left, rc.bottom - 6, 6, 6);
    draw_bitmap(dc, bitmaps.top, rc.left + 6, rc.top, w - 12, 6);
    draw_bitmap(dc, bitmaps.right, rc.right - 6, rc.top + 6, 6, h - 12);
    draw_bitmap(dc, bitmaps.bottom, rc.left + 6, rc.bottom - 6, w - 12, 6);
    draw_bitmap(dc, bitmaps.left, rc.left, rc.top + 6, 6, h - 12);
    if focused {
        let mut focus = RECT {
            left: rc.left + 2,
            top: rc.top + 2,
            right: rc.right - 2,
            bottom: rc.bottom - 2,
        };
        unsafe {
            let _ = DrawFocusRect(dc, &focus);
        }
    }
}

fn draw_bitmap(dc: HDC, bmp: HBITMAP, x: i32, y: i32, w: i32, h: i32) {
    if bmp.0.is_null() {
        return;
    }
    let mut dib = DIBSECTION::default();
    let got = unsafe {
        GetObjectW(
            bmp,
            std::mem::size_of::<DIBSECTION>() as i32,
            Some(&mut dib as *mut _ as *mut _),
        )
    };
    if got == 0 {
        return;
    }
    let mut bmi = BITMAPINFO::default();
    unsafe {
        std::ptr::copy_nonoverlapping(
            &dib.dsBmih as *const BITMAPINFOHEADER,
            &mut bmi.bmiHeader as *mut BITMAPINFOHEADER,
            1,
        );
        let _ = StretchDIBits(
            dc,
            x,
            y,
            w,
            h,
            0,
            0,
            6,
            6,
            Some(dib.dsBm.bmBits as *const _),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

unsafe fn register_drop(state: &mut DayPlannerState) {
    let target: IDropTarget = DayPlannerDropTarget {
        hwnd: state.hwnd,
        state: state as *mut DayPlannerState,
    }
    .into();
    if RegisterDragDrop(state.hwnd, &target).is_ok() {
        state.drop_target = Some(target);
    }
}

#[implement(IDropTarget)]
struct DayPlannerDropTarget {
    hwnd: HWND,
    state: *mut DayPlannerState,
}

impl DayPlannerDropTarget {
    fn state(&self) -> Option<&mut DayPlannerState> {
        unsafe { self.state.as_mut() }
    }

    fn has_format(&self, data_obj: &IDataObject, cf: u16) -> bool {
        let mut format = FORMATETC::default();
        format.cfFormat = cf;
        format.ptd = std::ptr::null_mut();
        format.dwAspect = DVASPECT_CONTENT.0 as u32;
        format.lindex = -1;
        format.tymed = TYMED_HGLOBAL.0 as u32;
        unsafe { data_obj.QueryGetData(&format).is_ok() }
    }

    fn choose_effect(&self, data_obj: &IDataObject) -> DROPEFFECT {
        if self.has_format(data_obj, CF_HDROP.0 as u16)
            || self.has_format(data_obj, CF_UNICODETEXT.0 as u16)
            || self.has_format(data_obj, format_task())
        {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        }
    }

    fn extract_files(&self, data_obj: &IDataObject) -> Option<Vec<String>> {
        let mut format = FORMATETC::default();
        format.cfFormat = CF_HDROP.0 as u16;
        format.ptd = std::ptr::null_mut();
        format.dwAspect = DVASPECT_CONTENT.0 as u32;
        format.lindex = -1;
        format.tymed = TYMED_HGLOBAL.0 as u32;
        let mut medium = unsafe { data_obj.GetData(&format).ok()? };
        let hdrop = HDROP(unsafe { medium.u.hGlobal.0 });
        let count = unsafe { DragQueryFileW(hdrop, 0xFFFFFFFF, None) };
        let mut files = Vec::new();
        for i in 0..count {
            let len = unsafe { DragQueryFileW(hdrop, i, None) } + 1;
            let mut buf = vec![0u16; len as usize];
            let written = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
            buf.truncate(written as usize);
            files.push(String::from_utf16_lossy(&buf));
        }
        unsafe { ReleaseStgMedium(&mut medium) };
        Some(files)
    }

    fn extract_text(&self, data_obj: &IDataObject) -> Option<String> {
        let mut format = FORMATETC::default();
        format.cfFormat = CF_UNICODETEXT.0 as u16;
        format.ptd = std::ptr::null_mut();
        format.dwAspect = DVASPECT_CONTENT.0 as u32;
        format.lindex = -1;
        format.tymed = TYMED_HGLOBAL.0 as u32;
        let mut medium = unsafe { data_obj.GetData(&format).ok()? };
        let handle = unsafe { medium.u.hGlobal };
        if handle.0.is_null() {
            unsafe { ReleaseStgMedium(&mut medium) };
            return None;
        }
        let ptr = unsafe { GlobalLock(handle) } as *const u16;
        if ptr.is_null() {
            unsafe { ReleaseStgMedium(&mut medium) };
            return None;
        }
        let mut len = 0usize;
        loop {
            let ch = unsafe { *ptr.add(len) };
            if ch == 0 {
                break;
            }
            len += 1;
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        let text = String::from_utf16_lossy(slice);
        unsafe {
            let _ = GlobalUnlock(handle);
            ReleaseStgMedium(&mut medium);
        }
        Some(text)
    }

    fn extract_task_id(&self, data_obj: &IDataObject) -> Option<u64> {
        let mut format = FORMATETC::default();
        format.cfFormat = format_task();
        format.ptd = std::ptr::null_mut();
        format.dwAspect = DVASPECT_CONTENT.0 as u32;
        format.lindex = -1;
        format.tymed = TYMED_HGLOBAL.0 as u32;
        let mut medium = unsafe { data_obj.GetData(&format).ok()? };
        let handle = unsafe { medium.u.hGlobal };
        if handle.0.is_null() {
            unsafe { ReleaseStgMedium(&mut medium) };
            return None;
        }
        let ptr = unsafe { GlobalLock(handle) } as *const u64;
        if ptr.is_null() {
            unsafe { ReleaseStgMedium(&mut medium) };
            return None;
        }
        let id = unsafe { *ptr };
        unsafe {
            let _ = GlobalUnlock(handle);
            ReleaseStgMedium(&mut medium);
        }
        Some(id)
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for DayPlannerDropTarget_Impl {
    fn DragEnter(
        &self,
        pDataObj: Option<&IDataObject>,
        _grfKeyState: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        _pt: &windows::Win32::Foundation::POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe { *pdwEffect = DROPEFFECT_NONE };
        if let Some(obj) = pDataObj {
            unsafe { *pdwEffect = self.choose_effect(obj) };
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfKeyState: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        _pt: &windows::Win32::Foundation::POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe { *pdwEffect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        pDataObj: Option<&IDataObject>,
        _grfKeyState: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe { *pdwEffect = DROPEFFECT_NONE };
        let Some(obj) = pDataObj else {
            return Ok(());
        };
        let mut client = POINT { x: pt.x, y: pt.y };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut client);
        }
        if let Some(state) = self.state() {
            if let Some(task_id) = self.extract_task_id(obj) {
                let title = unsafe { state.service.as_ref() }
                    .and_then(|svc| svc.tasks.get(&task_id))
                    .map(|task| task.name.clone())
                    .unwrap_or_else(|| "Task".to_string());
                drop_entry_at_point(state, client, title, Some(task_id));
                unsafe { *pdwEffect = DROPEFFECT_COPY };
                return Ok(());
            }
            if let Some(files) = self.extract_files(obj) {
                let title = files
                    .get(0)
                    .map(|s| s.clone())
                    .unwrap_or_else(|| "Dropped File".to_string());
                drop_entry_at_point(state, client, title, None);
                unsafe { *pdwEffect = DROPEFFECT_COPY };
                return Ok(());
            }
            if let Some(text) = self.extract_text(obj) {
                drop_entry_at_point(state, client, text, None);
                unsafe { *pdwEffect = DROPEFFECT_COPY };
            }
        }
        Ok(())
    }
}

fn drop_entry_at_point(
    state: &mut DayPlannerState,
    pt: POINT,
    title: String,
    task_id: Option<u64>,
) {
    if let Some((idx, rect)) = pane_index_at_point(state, pt) {
        let start = time_from_point(state, rect, pt.y);
        let stop = start + UNITS_PER_FRACTION;
        if let Some(pane) = state
            .container
            .children
            .get_mut(idx)
            .and_then(|child| child.as_any_mut().downcast_mut::<DayPlanPane>())
        {
            let kind = match pane.kind {
                PaneKind::Spec => EntryKind::Spec,
                PaneKind::Actual => EntryKind::Actual,
            };
            let id = task_id.unwrap_or(0);
            let entry_title = if id != 0 {
                unsafe { state.service.as_ref() }
                    .and_then(|svc| svc.tasks.get(&id))
                    .map(|task| task.name.clone())
                    .unwrap_or(title)
            } else {
                title
            };
            add_time_entry(state, id, entry_title, start, stop, kind);
        }
    }
}

fn pane_index_at_point(state: &DayPlannerState, pt: POINT) -> Option<(usize, RECT)> {
    state
        .container
        .children
        .iter()
        .enumerate()
        .find_map(|(idx, child)| {
            child
                .as_any()
                .downcast_ref::<DayPlanPane>()
                .filter(|pane| point_in_rect(pt, pane.rect))
                .map(|pane| (idx, pane.rect))
        })
}

unsafe fn update_split_from_point(state: &mut DayPlannerState, x: i32) {
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = rc.right - rc.left;
    let height = rc.bottom - rc.top;
    let offset = SPLITTER_QUICKTAB_WIDTH + 2;
    if x - offset <= HEADER_WIDTH || x + offset >= width {
        return;
    }
    let plan_width = width - HEADER_WIDTH;
    if plan_width <= 0 {
        return;
    }
    let spec_width = x - 3 - HEADER_WIDTH;
    let raw = spec_width as f64 / plan_width as f64;
    state.split_percent = raw.clamp(0.0, 1.0);
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
    state.container.children.iter().find_map(|child| {
        child
            .as_any()
            .downcast_ref::<DayPlanSplitter>()
            .map(|s| s.bounds())
    })
}

fn point_in_rect(pt: POINT, rc: RECT) -> bool {
    pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
}

struct DayPlannerHostState {
    hwnd: HWND,
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
                let body = create_planner_body(hwnd, state.service).expect("create planner body");
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
    let _ = MoveWindow(state.body_hwnd, 0, 0, width, height.max(0), true);
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
