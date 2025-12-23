use std::cell::Cell;
use std::sync::{Arc, Once};

use data::{service::Service, task::Task, task_compare::TaskComparator};
use gui::{
    bitmap::Bitmap,
    component::Component,
    list::{ListBox, ListBoxItem},
};
use windows::{
    core::{implement, Error, HRESULT, PCWSTR},
    Win32::{
        Foundation::{
            BOOL, COLORREF, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
            DV_E_DVASPECT, DV_E_FORMATETC, DV_E_TYMED, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY,
            HGLOBAL, HWND, LPARAM, LRESULT, RECT, S_FALSE, S_OK, WPARAM,
        },
        Graphics::Gdi::{
            AlphaBlend, CreateCompatibleDC, CreateFontIndirectW, DeleteDC, DrawTextW,
            GetStockObject, GetTextMetricsW, SelectObject, SetDCBrushColor, SetDCPenColor, AC_SRC_OVER,
            BLENDFUNCTION, DC_BRUSH, DC_PEN, DRAW_TEXT_FORMAT, DT_NOPREFIX, HBRUSH, HDC, HFONT,
            HGDIOBJ, LOGFONTW, TEXTMETRICW,
        },
        System::{
            Com::{
                IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumFORMATETC_Impl, FORMATETC,
                STGMEDIUM, TYMED_HGLOBAL, DVASPECT_CONTENT, DATADIR_GET,
            },
            DataExchange::RegisterClipboardFormatW,
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
            Ole::{
                DoDragDrop, IDropSource, IDropSource_Impl, CF_TEXT, CF_UNICODETEXT, DROPEFFECT,
                DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
            },
            SystemServices::MODIFIERKEYS_FLAGS,
        },
        UI::{
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, MoveWindow,
                RegisterClassW, SendMessageW, SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW,
                CS_VREDRAW, CW_USEDEFAULT, GWL_USERDATA, HMENU, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_COMMAND, WM_CREATE, WM_DESTROY, WM_SIZE, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN,
                WS_CLIPSIBLINGS, WS_VISIBLE, CB_GETCURSEL, CBN_SELCHANGE,
            },
        },
    },
};

use crate::winutil::to_wstring;

const TASK_LIST_CLASS: &str = "LNGTaskListWnd";
const DP_TASK_LIST_CLASS: &str = "LNGDPTaskListWnd";

const IDB_PRIORITY1: u16 = 227;
const IDB_PRIORITY2: u16 = 229;
const IDB_PRIORITY3: u16 = 228;
const IDB_PRIORITY4: u16 = 230;
const IDB_PRIORITY5: u16 = 231;
const IDB_XHATCHBR: u16 = 131;

fn format_task() -> u16 {
    static INIT: Once = Once::new();
    static mut CF: u16 = 0;
    unsafe {
        INIT.call_once(|| {
            CF = RegisterClipboardFormatW(PCWSTR(to_wstring("loadngo::data::task").as_ptr())) as u16;
        });
        CF
    }
}

#[derive(Clone)]
struct TaskListItem {
    task_id: u64,
    title: String,
    priority: i32,
    bounds: RECT,
}

#[derive(Clone)]
struct TaskDragInfo {
    id: u64,
    title: String,
}

impl TaskListItem {
    fn from_task(task: &Task) -> Self {
        Self {
            task_id: task.entity.id,
            title: task.name.clone(),
            priority: task.priority,
            bounds: RECT::default(),
        }
    }
}

impl ListBoxItem for TaskListItem {
    fn draw(&self, dc: HDC, width: i32, _height: i32, highlighted: bool) -> i32 {
        unsafe {
            let font = task_list_font();
            let mut old_font = HGDIOBJ::default();
            if !font.is_invalid() {
                old_font = SelectObject(dc, font);
            }
            let mut tm = TEXTMETRICW::default();
            let _ = GetTextMetricsW(dc, &mut tm);
            let mut line_h = tm.tmHeight.max(1);
            let mut text_y = 0;

            let (bmp_w, bmp_h) = priority_bitmap(self.priority)
                .map(|b| (b.width, b.height))
                .unwrap_or((0, 0));
            let mut bmp_y = 0;
            if line_h > bmp_h && bmp_h > 0 {
                bmp_y = (line_h / 2) - (bmp_h / 2);
            } else if line_h < bmp_h {
                text_y = (bmp_h / 2) - (line_h / 2);
                line_h = bmp_h;
            }

            if highlighted {
                SelectObject(dc, HGDIOBJ(GetStockObject(DC_BRUSH).0));
                SelectObject(dc, HGDIOBJ(GetStockObject(DC_PEN).0));
                SetDCBrushColor(dc, COLORREF(0x00e5f5c3));
                SetDCPenColor(dc, COLORREF(0x00e5f5c3));
                let _ = windows::Win32::Graphics::Gdi::Rectangle(dc, -4, 0, width - 2, line_h);
            }

            if let Some(bmp) = priority_bitmap(self.priority) {
                draw_bitmap_alpha(dc, bmp, 0, bmp_y);
            }

            let mut rect = RECT {
                left: bmp_w + 2,
                top: text_y,
                right: width - 2,
                bottom: line_h,
            };
            let mut w = to_wstring(&self.title);
            if !w.is_empty() {
                w.pop();
            }
            let _ = DrawTextW(
                dc,
                &mut w,
                &mut rect,
                DRAW_TEXT_FORMAT(DT_NOPREFIX.0 as u32),
            );

            if !old_font.0.is_null() {
                let _ = SelectObject(dc, old_font);
            }
            line_h
        }
    }

    fn set_bounds(&mut self, rect: RECT) {
        self.bounds = rect;
    }

    fn bounds(&self) -> RECT {
        self.bounds
    }
}

fn task_list_font() -> HFONT {
    static INIT: Once = Once::new();
    static mut FONT: HFONT = HFONT(std::ptr::null_mut());
    unsafe {
        INIT.call_once(|| {
            let mut lf: LOGFONTW = std::mem::zeroed();
            lf.lfCharSet = windows::Win32::Graphics::Gdi::DEFAULT_CHARSET;
            lf.lfClipPrecision = windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS;
            lf.lfOutPrecision = windows::Win32::Graphics::Gdi::OUT_TT_ONLY_PRECIS;
            lf.lfQuality = windows::Win32::Graphics::Gdi::ANTIALIASED_QUALITY;
            lf.lfPitchAndFamily = (windows::Win32::Graphics::Gdi::DEFAULT_PITCH.0
                | windows::Win32::Graphics::Gdi::FF_DONTCARE.0) as u8;
            lf.lfHeight = -13;
            lf.lfWeight = windows::Win32::Graphics::Gdi::FW_NORMAL.0 as i32;
            let face = to_wstring("Arial");
            for (i, ch) in face.iter().enumerate() {
                if i >= windows::Win32::Graphics::Gdi::LF_FACESIZE as usize - 1 {
                    break;
                }
                if *ch == 0 {
                    break;
                }
                lf.lfFaceName[i] = *ch;
            }
            FONT = CreateFontIndirectW(&lf);
        });
        FONT
    }
}

fn priority_bitmap(priority: i32) -> Option<&'static Bitmap> {
    let list = priority_bitmaps()?;
    let p = if priority <= 0 { 1 } else { priority };
    let idx = (p.min(5) - 1) as usize;
    list.get(idx)
}

fn priority_bitmaps() -> Option<&'static [Bitmap]> {
    static INIT: Once = Once::new();
    static mut BITMAPS: Option<Vec<Bitmap>> = None;
    unsafe {
        INIT.call_once(|| {
            let ids = [
                IDB_PRIORITY1,
                IDB_PRIORITY2,
                IDB_PRIORITY3,
                IDB_PRIORITY4,
                IDB_PRIORITY5,
            ];
            let mut list = Vec::new();
            for id in ids {
                if let Ok(bmp) = Bitmap::load_resource(id) {
                    list.push(bmp);
                }
            }
            if list.len() == ids.len() {
                BITMAPS = Some(list);
            }
        });
        BITMAPS.as_ref().map(|v| v.as_slice())
    }
}

fn draw_bitmap_alpha(dc: HDC, bmp: &Bitmap, x: i32, y: i32) {
    unsafe {
        let mem_dc = CreateCompatibleDC(dc);
        if mem_dc.is_invalid() {
            return;
        }
        let old = SelectObject(mem_dc, HGDIOBJ(bmp.handle.0));
        let bf = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 0xFF,
            AlphaFormat: windows::Win32::Graphics::Gdi::AC_SRC_ALPHA as u8,
        };
        let _ = AlphaBlend(
            dc,
            x,
            y,
            bmp.width,
            bmp.height,
            mem_dc,
            0,
            0,
            bmp.width,
            bmp.height,
            bf,
        );
        let _ = SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
    }
}

#[implement(IEnumFORMATETC)]
struct FormatEtcEnum {
    formats: Vec<FORMATETC>,
    index: Cell<usize>,
}

impl FormatEtcEnum {
    fn new(formats: Vec<FORMATETC>) -> Self {
        Self {
            formats,
            index: Cell::new(0),
        }
    }
}

#[allow(non_snake_case)]
impl IEnumFORMATETC_Impl for FormatEtcEnum_Impl {
    fn Next(
        &self,
        celt: u32,
        rgelt: *mut FORMATETC,
        pcelt_fetched: *mut u32,
    ) -> HRESULT {
        if rgelt.is_null() {
            return E_INVALIDARG;
        }
        let mut fetched = 0u32;
        let mut idx = self.index.get();
        unsafe {
            while fetched < celt && idx < self.formats.len() {
                rgelt.add(fetched as usize).write(self.formats[idx]);
                fetched += 1;
                idx += 1;
            }
            if !pcelt_fetched.is_null() {
                *pcelt_fetched = fetched;
            }
        }
        self.index.set(idx);
        if fetched == celt { S_OK } else { S_FALSE }
    }

    fn Skip(&self, celt: u32) -> windows::core::Result<()> {
        let idx = self.index.get().saturating_add(celt as usize);
        self.index.set(idx);
        if idx <= self.formats.len() {
            Ok(())
        } else {
            Err(Error::from(S_FALSE))
        }
    }

    fn Reset(&self) -> windows::core::Result<()> {
        self.index.set(0);
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumFORMATETC> {
        Ok(FormatEtcEnum {
            formats: self.formats.clone(),
            index: Cell::new(self.index.get()),
        }
        .into())
    }
}

#[implement(IDataObject)]
struct TaskDataObject {
    task_id: u64,
    title: String,
}

impl TaskDataObject {
    fn formats() -> Vec<FORMATETC> {
        let mut formats = Vec::new();
        formats.push(FORMATETC {
            cfFormat: format_task(),
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0 as u32,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        });
        formats.push(FORMATETC {
            cfFormat: CF_UNICODETEXT.0 as u16,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0 as u32,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        });
        formats.push(FORMATETC {
            cfFormat: CF_TEXT.0 as u16,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0 as u32,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        });
        formats
    }

    fn alloc_hglobal(bytes: &[u8]) -> windows::core::Result<HGLOBAL> {
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
            let ptr = GlobalLock(handle) as *mut u8;
            if ptr.is_null() {
                return Err(Error::from(E_OUTOFMEMORY));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = GlobalUnlock(handle);
            Ok(handle)
        }
    }

    fn medium_from_hglobal(handle: HGLOBAL) -> STGMEDIUM {
        let mut medium = STGMEDIUM::default();
        medium.tymed = TYMED_HGLOBAL.0 as u32;
        medium.u.hGlobal = handle;
        medium
    }

    fn write_task_id(&self) -> windows::core::Result<STGMEDIUM> {
        let bytes = self.task_id.to_le_bytes();
        let handle = Self::alloc_hglobal(&bytes)?;
        Ok(Self::medium_from_hglobal(handle))
    }

    fn write_text(&self, unicode: bool) -> windows::core::Result<STGMEDIUM> {
        if unicode {
            let mut wide: Vec<u16> = self.title.encode_utf16().collect();
            wide.push(0);
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    wide.as_ptr() as *const u8,
                    wide.len() * std::mem::size_of::<u16>(),
                )
            };
            let handle = Self::alloc_hglobal(bytes)?;
            Ok(Self::medium_from_hglobal(handle))
        } else {
            let mut bytes = self.title.clone().into_bytes();
            bytes.push(0);
            let handle = Self::alloc_hglobal(&bytes)?;
            Ok(Self::medium_from_hglobal(handle))
        }
    }
}

#[allow(non_snake_case)]
impl IDataObject_Impl for TaskDataObject_Impl {
    fn GetData(&self, pformatetc: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        if pformatetc.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        let fmt = unsafe { *pformatetc };
        if fmt.cfFormat == format_task() {
            return self.write_task_id();
        }
        if fmt.cfFormat == CF_UNICODETEXT.0 as u16 {
            return self.write_text(true);
        }
        if fmt.cfFormat == CF_TEXT.0 as u16 {
            return self.write_text(false);
        }
        Err(Error::from(DV_E_FORMATETC))
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(Error::from(E_NOTIMPL))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if pformatetc.is_null() {
            return E_INVALIDARG;
        }
        let fmt = unsafe { *pformatetc };
        let supported = fmt.cfFormat == format_task()
            || fmt.cfFormat == CF_TEXT.0 as u16
            || fmt.cfFormat == CF_UNICODETEXT.0 as u16;
        if !supported {
            return DV_E_FORMATETC;
        }
        if fmt.dwAspect != DVASPECT_CONTENT.0 as u32 {
            return DV_E_DVASPECT;
        }
        if (fmt.tymed & TYMED_HGLOBAL.0 as u32) == 0 {
            return DV_E_TYMED;
        }
        S_OK
    }

    fn GetCanonicalFormatEtc(&self, _pformatectin: *const FORMATETC, _pformatetcout: *mut FORMATETC) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(&self, _pformatetc: *const FORMATETC, _pmedium: *const STGMEDIUM, _frelease: BOOL) -> windows::core::Result<()> {
        Err(Error::from(E_NOTIMPL))
    }

    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        if direction == DATADIR_GET.0 as u32 {
            Ok(FormatEtcEnum::new(TaskDataObject::formats()).into())
        } else {
            Err(Error::from(E_NOTIMPL))
        }
    }

    fn DAdvise(&self, _pformatetc: *const FORMATETC, _advf: u32, _padvsink: Option<&windows::Win32::System::Com::IAdviseSink>) -> windows::core::Result<u32> {
        Err(Error::from(E_NOTIMPL))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(Error::from(E_NOTIMPL))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<windows::Win32::System::Com::IEnumSTATDATA> {
        Err(Error::from(E_NOTIMPL))
    }
}

#[implement(IDropSource)]
struct TaskDropSource;

#[allow(non_snake_case)]
impl IDropSource_Impl for TaskDropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fEscapePressed: BOOL,
        grfKeyState: MODIFIERKEYS_FLAGS,
    ) -> HRESULT {
        if fEscapePressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if grfKeyState.contains(windows::Win32::System::SystemServices::MK_LBUTTON) {
            S_OK
        } else {
            DRAGDROP_S_DROP
        }
    }

    fn GiveFeedback(&self, _dwEffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn begin_task_drag(info: &TaskDragInfo) {
    unsafe {
        let data_obj: IDataObject = TaskDataObject {
            task_id: info.id,
            title: info.title.clone(),
        }
        .into();
        let source: IDropSource = TaskDropSource.into();
        let mut effect = DROPEFFECT(0);
        let _ = DoDragDrop(
            &data_obj,
            &source,
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
            &mut effect,
        );
    }
}

#[derive(Clone, Copy)]
enum TaskListQuery {
    TopLevel,
    Leaves,
}

struct TaskListAdapter {
    comparator: TaskComparator,
    context_id: Option<u64>,
    query: TaskListQuery,
}

impl TaskListAdapter {
    fn new_top_level() -> Self {
        Self {
            comparator: TaskComparator::default(),
            context_id: None,
            query: TaskListQuery::TopLevel,
        }
    }

    fn new_leaves() -> Self {
        Self {
            comparator: TaskComparator::default(),
            context_id: None,
            query: TaskListQuery::Leaves,
        }
    }

    fn set_context(&mut self, context_id: Option<u64>) {
        self.context_id = context_id;
    }

    fn collect_tasks<'a>(&self, service: &'a Service) -> Vec<&'a Task> {
        let mut has_children = std::collections::HashSet::new();
        for task in service.tasks.values() {
            if let Some(parent) = task.parent {
                has_children.insert(parent);
            }
        }

        let mut tasks: Vec<&Task> = service.tasks.values().collect();
        tasks.retain(|task| match self.query {
            TaskListQuery::TopLevel => {
                self.context_id.map_or(task.parent.is_none(), |ctx| task.parent == Some(ctx))
            }
            TaskListQuery::Leaves => {
                let is_leaf = !has_children.contains(&task.entity.id);
                if let Some(ctx) = self.context_id {
                    task.parent == Some(ctx) && is_leaf
                } else {
                    is_leaf
                }
            }
        });

        tasks.sort_by(|a, b| self.comparator.compare(a, b));
        tasks
    }

    fn items(&self, service: &Service) -> Vec<Box<dyn ListBoxItem>> {
        self.collect_tasks(service)
            .into_iter()
            .map(|task| Box::new(TaskListItem::from_task(task)) as Box<dyn ListBoxItem>)
            .collect()
    }
}

struct TaskListState {
    hwnd: HWND,
    list: Option<Box<ListBox>>,
    service: *mut Service,
    adapter: TaskListAdapter,
    drag_items: Vec<TaskDragInfo>,
}

pub fn create_task_list_wnd(parent: HWND, service: *mut Service) -> HWND {
    unsafe {
        register_task_list_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(TaskListState {
            hwnd: HWND::default(),
            list: None,
            service,
            adapter: TaskListAdapter::new_top_level(),
            drag_items: Vec::new(),
        });
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(to_wstring(TASK_LIST_CLASS).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            200,
            200,
            parent,
            HMENU(std::ptr::null_mut()),
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create task list window")
    }
}

pub fn refresh_task_list(hwnd: HWND) {
    unsafe {
        if let Some(state) = task_list_state(hwnd) {
            rebuild_task_list(state);
        }
    }
}

unsafe fn register_task_list_class() {
    let hinstance = GetModuleHandleW(None).unwrap();
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(task_list_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: PCWSTR(to_wstring(TASK_LIST_CLASS).as_ptr()),
        hbrBackground: HBRUSH::default(),
        ..Default::default()
    };
    let _ = RegisterClassW(&class);
}

unsafe extern "system" fn task_list_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut TaskListState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);
                if let Ok(mut list) = ListBox::create(hwnd) {
                    let handler = Arc::new(move |idx| unsafe {
                        if let Some(state) = (state_ptr as *mut TaskListState).as_mut() {
                            if let Some(info) = state.drag_items.get(idx).cloned() {
                                begin_task_drag(&info);
                            }
                        }
                    });
                    list.set_drag_handler(Some(handler));
                    state.list = Some(list);
                    rebuild_task_list(state);
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = task_list_state(hwnd) {
                layout_task_list(state);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ptr) = detach_task_list_state(hwnd) {
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn layout_task_list(state: &mut TaskListState) {
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    if let Some(list) = state.list.as_mut() {
        let width = (rc.right - rc.left - 10).max(0);
        let height = (rc.bottom - rc.top - 10).max(0);
        let _ = MoveWindow(list.hwnd(), 5, 5, width, height, true);
    }
}

unsafe fn rebuild_task_list(state: &mut TaskListState) {
    let service = match state.service.as_ref() {
        Some(service) => service,
        None => return,
    };
    if let Some(list) = state.list.as_mut() {
        let tasks = state.adapter.collect_tasks(service);
        state.drag_items = tasks
            .iter()
            .map(|task| TaskDragInfo {
                id: task.entity.id,
                title: task.name.clone(),
            })
            .collect();
        let items = tasks
            .iter()
            .map(|task| Box::new(TaskListItem::from_task(task)) as Box<dyn ListBoxItem>)
            .collect();
        list.set_items(items);
    }
}

unsafe fn task_list_state(hwnd: HWND) -> Option<&'static mut TaskListState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut TaskListState;
    ptr.as_mut()
}

unsafe fn detach_task_list_state(hwnd: HWND) -> Option<*mut TaskListState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut TaskListState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

struct DpTaskListState {
    hwnd: HWND,
    list: Option<Box<ListBox>>,
    context_combo: HWND,
    context_ids: Vec<Option<u64>>,
    detail_hwnd: HWND,
    service: *mut Service,
    adapter: TaskListAdapter,
    drag_items: Vec<TaskDragInfo>,
}

pub fn create_dp_task_list_wnd(parent: HWND, service: *mut Service) -> HWND {
    unsafe {
        register_dp_task_list_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(DpTaskListState {
            hwnd: HWND::default(),
            list: None,
            context_combo: HWND::default(),
            context_ids: Vec::new(),
            detail_hwnd: HWND::default(),
            service,
            adapter: TaskListAdapter::new_leaves(),
            drag_items: Vec::new(),
        });
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(to_wstring(DP_TASK_LIST_CLASS).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPCHILDREN.0 | WS_CLIPSIBLINGS.0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            200,
            200,
            parent,
            HMENU(std::ptr::null_mut()),
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create dp task list window")
    }
}

pub fn refresh_dp_task_list(hwnd: HWND) {
    unsafe {
        if let Some(state) = dp_task_list_state(hwnd) {
            rebuild_dp_task_list(state);
        }
    }
}

unsafe fn register_dp_task_list_class() {
    let hinstance = GetModuleHandleW(None).unwrap();
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(dp_task_list_wndproc),
        hInstance: hinstance.into(),
        lpszClassName: PCWSTR(to_wstring(DP_TASK_LIST_CLASS).as_ptr()),
        hbrBackground: HBRUSH::default(),
        ..Default::default()
    };
    let _ = RegisterClassW(&class);
}

unsafe extern "system" fn dp_task_list_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut DpTaskListState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);

                if let Ok(mut list) = ListBox::create(hwnd) {
                    let handler = Arc::new(move |idx| unsafe {
                        if let Some(state) = (state_ptr as *mut DpTaskListState).as_mut() {
                            if let Some(info) = state.drag_items.get(idx).cloned() {
                                begin_task_drag(&info);
                            }
                        }
                    });
                    list.set_drag_handler(Some(handler));
                    state.list = Some(list);
                }

                let combo_class = to_wstring("COMBOBOX");
                state.context_combo = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(combo_class.as_ptr()),
                    PCWSTR::null(),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
                    0,
                    0,
                    120,
                    24,
                    hwnd,
                    HMENU(std::ptr::null_mut()),
                    None,
                    None,
                )
                .unwrap_or(HWND::default());

                state.detail_hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(to_wstring("STATIC").as_ptr()),
                    PCWSTR(to_wstring("Task Details").as_ptr()),
                    WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0),
                    0,
                    0,
                    120,
                    40,
                    hwnd,
                    HMENU(std::ptr::null_mut()),
                    None,
                    None,
                )
                .unwrap_or(HWND::default());

                populate_context_combo(state);
                rebuild_dp_task_list(state);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            if let Some(state) = dp_task_list_state(hwnd) {
                let code = ((wparam.0 >> 16) & 0xffff) as u16;
                let src = HWND(lparam.0 as *mut _);
                if src == state.context_combo && code == CBN_SELCHANGE as u16 {
                    apply_context_selection(state);
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = dp_task_list_state(hwnd) {
                layout_dp_task_list(state);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ptr) = detach_dp_task_list_state(hwnd) {
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn layout_dp_task_list(state: &mut DpTaskListState) {
    let mut rc = RECT::default();
    let _ = GetClientRect(state.hwnd, &mut rc);
    let width = (rc.right - rc.left).max(0);
    let height = (rc.bottom - rc.top).max(0);
    let context_h = 24;
    let detail_h = 84;

    let _ = MoveWindow(state.context_combo, 10, 0, width - 20, context_h, true);
    let list_h = (height - context_h - detail_h - 10).max(0);
    if let Some(list) = state.list.as_mut() {
        let _ = MoveWindow(list.hwnd(), 5, context_h + 5, width - 10, list_h, true);
    }
    let _ = MoveWindow(
        state.detail_hwnd,
        5,
        height - detail_h,
        width - 10,
        detail_h,
        true,
    );
}

unsafe fn populate_context_combo(state: &mut DpTaskListState) {
    state.context_ids.clear();
    let _ = SendMessageW(state.context_combo, windows::Win32::UI::WindowsAndMessaging::CB_RESETCONTENT, WPARAM(0), LPARAM(0));

    let add_item = |hwnd: HWND, text: &str| {
        let w = to_wstring(text);
        let _ = SendMessageW(hwnd, windows::Win32::UI::WindowsAndMessaging::CB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
    };

    add_item(state.context_combo, "All Tasks");
    state.context_ids.push(None);

    if let Some(service) = state.service.as_ref() {
        let mut roots: Vec<&Task> = service.tasks.values().filter(|t| t.parent.is_none()).collect();
        roots.sort_by(|a, b| a.name.cmp(&b.name));
        for task in roots {
            add_item(state.context_combo, &task.name);
            state.context_ids.push(Some(task.entity.id));
        }
    }
    let _ = SendMessageW(state.context_combo, windows::Win32::UI::WindowsAndMessaging::CB_SETCURSEL, WPARAM(0), LPARAM(0));
}

unsafe fn apply_context_selection(state: &mut DpTaskListState) {
    let idx = SendMessageW(
        state.context_combo,
        CB_GETCURSEL,
        WPARAM(0),
        LPARAM(0),
    )
    .0 as i32;
    let selected = if idx >= 0 {
        state.context_ids.get(idx as usize).cloned().unwrap_or(None)
    } else {
        None
    };
    state.adapter.set_context(selected);
    rebuild_dp_task_list(state);
}

unsafe fn rebuild_dp_task_list(state: &mut DpTaskListState) {
    let service = match state.service.as_ref() {
        Some(service) => service,
        None => return,
    };
    if let Some(list) = state.list.as_mut() {
        let tasks = state.adapter.collect_tasks(service);
        state.drag_items = tasks
            .iter()
            .map(|task| TaskDragInfo {
                id: task.entity.id,
                title: task.name.clone(),
            })
            .collect();
        let items = tasks
            .iter()
            .map(|task| Box::new(TaskListItem::from_task(task)) as Box<dyn ListBoxItem>)
            .collect();
        list.set_items(items);
    }
}

unsafe fn dp_task_list_state(hwnd: HWND) -> Option<&'static mut DpTaskListState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DpTaskListState;
    ptr.as_mut()
}

unsafe fn detach_dp_task_list_state(hwnd: HWND) -> Option<*mut DpTaskListState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut DpTaskListState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}
