use anyhow::Result;
use data::{service::Service, types::Id};
use gui::buffered::BufferedWnd;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    InvalidateRect, LineTo, MoveToEx, SetDCBrushColor, SetDCPenColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetScrollInfo, GetWindowLongPtrW,
    RegisterClassW, SetWindowLongPtrW, CREATESTRUCTW, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW,
    GWL_USERDATA, HMENU, SCROLLBAR_COMMAND, SCROLLINFO, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CREATE, WM_DESTROY, WM_HSCROLL, WM_LBUTTONUP, WM_PAINT,
    WM_SIZE, WM_VSCROLL, WNDCLASSW, WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_HSCROLL,
    WS_VISIBLE, WS_VSCROLL,
};

use crate::project_planner::base_tree_item::{hit_test_tree, set_selected};
use crate::project_planner::new_entity_widget::NewEntityWidget;
use crate::project_planner::project_tree_adapter::ProjectTreeAdapter;
use crate::winutil::to_wstring;

const CLASS_NAME: &str = "LNGProjectHierarchyWnd";

pub struct ProjectHierarchyState {
    hwnd: HWND,
    service: *mut Service,
    adapter: ProjectTreeAdapter,
    buffer: BufferedWnd,
    dirty: bool,
    selected_id: Option<Id>,
    new_entity: Option<NewEntityWidget>,
    calculated_width: i32,
    calculated_height: i32,
    h_scroll_pos: i32,
    v_scroll_pos: i32,
    x_offset: i32,
    y_offset: i32,
}

pub fn register_class() -> Result<()> {
    unsafe {
        static mut DONE: bool = false;
        if DONE {
            return Ok(());
        }
        let hinstance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
        DONE = true;
    }
    Ok(())
}

pub fn create_project_hierarchy(parent: HWND, service: *mut Service) -> HWND {
    unsafe {
        let _ = register_class();
        let hinstance = GetModuleHandleW(None).unwrap();
        let state = Box::new(ProjectHierarchyState {
            hwnd: HWND::default(),
            service,
            adapter: ProjectTreeAdapter::new(service),
            buffer: BufferedWnd::new(),
            dirty: true,
            selected_id: None,
            new_entity: None,
            calculated_width: 0,
            calculated_height: 0,
            h_scroll_pos: 500,
            v_scroll_pos: 0,
            x_offset: 0,
            y_offset: 0,
        });
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(to_wstring(CLASS_NAME).as_ptr()),
            PCWSTR::null(),
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_CLIPCHILDREN.0
                    | WS_CLIPSIBLINGS.0
                    | WS_VSCROLL.0
                    | WS_HSCROLL.0,
            ),
            0,
            0,
            200,
            200,
            parent,
            HMENU(std::ptr::null_mut()),
            hinstance,
            Some(Box::into_raw(state) as *mut _),
        )
        .expect("create project hierarchy")
    }
}

pub fn refresh_project_hierarchy(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
    }
}

pub fn set_project_root(hwnd: HWND, task_id: Option<Id>) {
    unsafe {
        if let Some(state) = state(hwnd) {
            state.adapter.set_root(task_id);
            state.dirty = true;
            refresh_project_hierarchy(hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let state_ptr = cs.lpCreateParams as *mut ProjectHierarchyState;
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                state.hwnd = hwnd;
                SetWindowLongPtrW(hwnd, GWL_USERDATA, state_ptr as isize);
                state.adapter.set_root(None);
                state.dirty = true;
                state.new_entity = Some(NewEntityWidget::create(hwnd));
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                update_scrollbars(state, rc.right - rc.left, rc.bottom - rc.top);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state(hwnd) {
                let width = (lparam.0 & 0xffff) as i16 as i32;
                let height = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                update_scrollbars(state, width, height);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            if let Some(state) = state(hwnd) {
                paint(state);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = state(hwnd) {
                let pt = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                handle_click(state, pt);
            }
            LRESULT(0)
        }
        WM_VSCROLL => {
            if let Some(state) = state(hwnd) {
                handle_vscroll(state, wparam);
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            if let Some(state) = state(hwnd) {
                handle_hscroll(state, wparam);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ptr) = detach_state(hwnd) {
                drop(Box::from_raw(ptr));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn state(hwnd: HWND) -> Option<&'static mut ProjectHierarchyState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut ProjectHierarchyState;
    ptr.as_mut()
}

unsafe fn detach_state(hwnd: HWND) -> Option<*mut ProjectHierarchyState> {
    let ptr = GetWindowLongPtrW(hwnd, GWL_USERDATA) as *mut ProjectHierarchyState;
    if !ptr.is_null() {
        SetWindowLongPtrW(hwnd, GWL_USERDATA, 0);
        Some(ptr)
    } else {
        None
    }
}

fn paint(state: &mut ProjectHierarchyState) {
    let hwnd = state.hwnd;
    let mut buffer = std::mem::replace(&mut state.buffer, BufferedWnd::new());
    let _ = buffer.paint(hwnd, |_, dc, width, height| {
        unsafe {
            SetDCBrushColor(dc, windows::Win32::Foundation::COLORREF(0x00ffffff));
            SetDCPenColor(dc, windows::Win32::Foundation::COLORREF(0x00ffffff));
            let _ = windows::Win32::Graphics::Gdi::Rectangle(dc, 0, 0, width, height);
        }
        if state.adapter.root().is_none() {
            return Ok(());
        }

        if state.dirty {
            layout_hierarchy(state);
            update_scrollbars(state, width, height);
        }

        state.x_offset = (width / 2) - (state.calculated_width * state.h_scroll_pos / 1000);
        state.y_offset = -(state.calculated_height * state.v_scroll_pos / 1000);

        unsafe {
            let _ = windows::Win32::Graphics::Gdi::OffsetViewportOrgEx(
                dc,
                state.x_offset,
                state.y_offset,
                None,
            );
        }

        if let Some(root) = state.adapter.root() {
            draw_connecting_lines(dc, root.as_ref());
            draw_tree(dc, root.as_ref());
        }

        state.dirty = false;
        Ok(())
    });
    state.buffer = buffer;
}

fn layout_hierarchy(state: &mut ProjectHierarchyState) {
    if let Some(root) = state.adapter.root_mut() {
        let mut locations = Vec::new();
        root.get_item_locations(0.0, &mut locations, true);
        if locations.is_empty() {
            return;
        }
        let mut pos = 0usize;
        let x = locations[pos] as i32;
        pos += 1;
        root.place_item(x, 15);
        let next_y = root.height() + 35 + 15;
        let children = root.base_mut().children.as_mut_slice();
        layout_next_tier(next_y, children, &locations, &mut pos);

        state.calculated_width = calc_width(state);
        state.calculated_height = calc_height(state);
    }
}

fn layout_next_tier(
    y: i32,
    kids: &mut [Box<dyn crate::project_planner::base_tree_item::TreeItem>],
    locations: &[f64],
    pos: &mut usize,
) {
    if kids.is_empty() || *pos >= locations.len() {
        return;
    }
    for child in kids.iter_mut() {
        if *pos >= locations.len() {
            break;
        }
        let x = locations[*pos] as i32;
        *pos += 1;
        child.place_item(x, y);
    }
    let next_y = y + kids[0].height() + 35;
    for child in kids.iter_mut() {
        if child.is_expanded() {
            let child_kids = child.base_mut().children.as_mut_slice();
            layout_next_tier(next_y, child_kids, locations, pos);
        }
    }
}

fn draw_tree(
    dc: windows::Win32::Graphics::Gdi::HDC,
    item: &dyn crate::project_planner::base_tree_item::TreeItem,
) {
    item.draw_item(dc);
    if item.is_expanded() {
        for child in item.base().children.iter() {
            draw_tree(dc, child.as_ref());
        }
    }
}

fn draw_connecting_lines(
    dc: windows::Win32::Graphics::Gdi::HDC,
    item: &dyn crate::project_planner::base_tree_item::TreeItem,
) {
    if !item.is_expanded() {
        return;
    }
    let parent = item.base().connector_start();
    unsafe {
        SetDCPenColor(dc, windows::Win32::Foundation::COLORREF(0x00776767));
    }
    for child in item.base().children.iter() {
        let center = child.base().center();
        unsafe {
            let _ = MoveToEx(dc, parent.x, parent.y, None);
            let _ = LineTo(dc, center.x, center.y);
        }
    }
    for child in item.base().children.iter() {
        draw_connecting_lines(dc, child.as_ref());
    }
}

fn handle_click(state: &mut ProjectHierarchyState, pt: POINT) {
    let mut tree_pt = pt;
    tree_pt.x -= state.x_offset;
    tree_pt.y -= state.y_offset;
    if let Some(root) = state.adapter.root() {
        if let Some((id, hit_expand)) = hit_test_tree(root.as_ref(), tree_pt) {
            if hit_expand {
                state.adapter.toggle_expanded(id);
            } else {
                state.selected_id = Some(id);
                state.adapter.update_selection(state.selected_id);
            }
            state.dirty = true;
            refresh_project_hierarchy(state.hwnd);
            return;
        }
    }
    state.selected_id = None;
    if let Some(root) = state.adapter.root_mut() {
        set_selected(root.as_mut(), None);
    }
    if let Some(widget) = state.new_entity.as_ref() {
        widget.begin_edit(pt);
    }
    state.dirty = true;
    refresh_project_hierarchy(state.hwnd);
}

fn calc_width(state: &ProjectHierarchyState) -> i32 {
    state
        .adapter
        .root()
        .map(|root| root.get_point_width() as i32 + 20)
        .unwrap_or(0)
}

fn calc_height(state: &ProjectHierarchyState) -> i32 {
    let tiers = state.adapter.tier_count().saturating_sub(2).max(0);
    tiers * 99 + 256
}

fn update_scrollbars(state: &mut ProjectHierarchyState, width: i32, height: i32) {
    let calc_w = calc_width(state);
    let calc_h = calc_height(state);

    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS | SIF_RANGE | SIF_PAGE,
        nMin: 0,
        nMax: 1000,
        nPage: 100,
        nPos: state.h_scroll_pos,
        ..Default::default()
    };

    if width >= calc_w || calc_w == 0 {
        si.nMin = 0;
        si.nMax = 0;
        si.nPos = 0;
        si.nPage = 0;
    } else {
        let page = ((width as f64 / calc_w as f64) * 1000.0) as u32;
        si.nPage = page.max(1);
    }
    unsafe {
        let _ = SetScrollInfo(
            state.hwnd,
            windows::Win32::UI::WindowsAndMessaging::SB_HORZ,
            &si,
            true,
        );
    }

    let mut si_v = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS | SIF_RANGE | SIF_PAGE,
        nMin: 0,
        nMax: 1000,
        nPage: 100,
        nPos: state.v_scroll_pos,
        ..Default::default()
    };
    if height >= calc_h || calc_h == 0 {
        si_v.nMin = 0;
        si_v.nMax = 0;
        si_v.nPos = 0;
        si_v.nPage = 0;
    } else {
        let page = ((height as f64 / calc_h as f64) * 1000.0) as u32;
        si_v.nPage = page.max(1);
    }
    unsafe {
        let _ = SetScrollInfo(
            state.hwnd,
            windows::Win32::UI::WindowsAndMessaging::SB_VERT,
            &si_v,
            true,
        );
    }

    state.calculated_width = calc_w;
    state.calculated_height = calc_h;
}

fn handle_vscroll(state: &mut ProjectHierarchyState, wparam: WPARAM) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS | SIF_TRACKPOS | SIF_RANGE | SIF_PAGE,
        ..Default::default()
    };
    unsafe {
        let _ = GetScrollInfo(
            state.hwnd,
            windows::Win32::UI::WindowsAndMessaging::SB_VERT,
            &mut si,
        );
    }
    match SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32) {
        windows::Win32::UI::WindowsAndMessaging::SB_LINEUP => {
            if si.nPos > si.nMin {
                si.nPos -= 1;
            }
        }
        windows::Win32::UI::WindowsAndMessaging::SB_LINEDOWN => {
            if si.nPos < si.nMax {
                si.nPos += 1;
            }
        }
        windows::Win32::UI::WindowsAndMessaging::SB_PAGEUP => {
            si.nPos = (si.nPos - si.nPage as i32).max(si.nMin);
        }
        windows::Win32::UI::WindowsAndMessaging::SB_PAGEDOWN => {
            si.nPos = (si.nPos + si.nPage as i32).min(si.nMax);
        }
        windows::Win32::UI::WindowsAndMessaging::SB_THUMBTRACK
        | windows::Win32::UI::WindowsAndMessaging::SB_THUMBPOSITION => {
            si.nPos = si.nTrackPos;
        }
        _ => {}
    }
    state.v_scroll_pos = si.nPos;
    si.fMask = SIF_POS;
    unsafe {
        let _ = SetScrollInfo(
            state.hwnd,
            windows::Win32::UI::WindowsAndMessaging::SB_VERT,
            &si,
            true,
        );
    }
    state.dirty = true;
    refresh_project_hierarchy(state.hwnd);
}

fn handle_hscroll(state: &mut ProjectHierarchyState, wparam: WPARAM) {
    let mut si = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_POS | SIF_TRACKPOS | SIF_RANGE | SIF_PAGE,
        ..Default::default()
    };
    unsafe {
        let _ = GetScrollInfo(
            state.hwnd,
            windows::Win32::UI::WindowsAndMessaging::SB_HORZ,
            &mut si,
        );
    }
    match SCROLLBAR_COMMAND((wparam.0 & 0xffff) as i32) {
        windows::Win32::UI::WindowsAndMessaging::SB_LINEUP => {
            if si.nPos > si.nMin {
                si.nPos -= 1;
            }
        }
        windows::Win32::UI::WindowsAndMessaging::SB_LINEDOWN => {
            if si.nPos < si.nMax {
                si.nPos += 1;
            }
        }
        windows::Win32::UI::WindowsAndMessaging::SB_PAGEUP => {
            si.nPos = (si.nPos - si.nPage as i32).max(si.nMin);
        }
        windows::Win32::UI::WindowsAndMessaging::SB_PAGEDOWN => {
            si.nPos = (si.nPos + si.nPage as i32).min(si.nMax);
        }
        windows::Win32::UI::WindowsAndMessaging::SB_THUMBTRACK
        | windows::Win32::UI::WindowsAndMessaging::SB_THUMBPOSITION => {
            si.nPos = si.nTrackPos;
        }
        _ => {}
    }
    state.h_scroll_pos = si.nPos;
    si.fMask = SIF_POS;
    unsafe {
        let _ = SetScrollInfo(
            state.hwnd,
            windows::Win32::UI::WindowsAndMessaging::SB_HORZ,
            &si,
            true,
        );
    }
    state.dirty = true;
    refresh_project_hierarchy(state.hwnd);
}
