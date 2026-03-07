use crate::component::Component;
use std::ptr::NonNull;
use tracing::debug;
use windows::core::implement;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    MoveWindow, WM_CAPTURECHANGED, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE,
};
const WM_MOUSELEAVE_CONST: u32 = 0x02A3;

/// Simple container that owns child Components and forwards messages.
pub struct Container {
    pub hwnd: HWND,
    pub children: Vec<Box<dyn Component>>,
    focus_idx: Option<usize>,
    hover_idx: Option<usize>,
    capturing_idx: Option<usize>,
    tracking_mouse: bool,
    drop_target: Option<IDropTarget>,
}

impl Container {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            children: Vec::new(),
            focus_idx: None,
            hover_idx: None,
            capturing_idx: None,
            tracking_mouse: false,
            drop_target: None,
        }
    }

    pub fn set_hwnd(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
    }

    pub fn add(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    pub fn remove_by_hwnd(&mut self, hwnd: HWND) {
        self.children.retain(|c| c.hwnd() != hwnd);
    }

    /// Enable a simple file-drop target for this container. Drops are hit-tested
    /// to children and dispatched to `drop_files`; DragOver is accepted when any
    /// child returns true from `drag_over`.
    pub fn enable_file_drop(&mut self) {
        if self.drop_target.is_some() || self.hwnd.0.is_null() {
            return;
        }
        let target = ContainerDropTarget::new(self.hwnd, NonNull::from(&mut *self));
        let com_target: IDropTarget = target.into();
        unsafe {
            if RegisterDragDrop(self.hwnd, &com_target).is_ok() {
                self.drop_target = Some(com_target);
            }
        }
    }

    pub fn revoke_file_drop(&mut self) {
        if self.drop_target.is_some() {
            unsafe {
                let _ = RevokeDragDrop(self.hwnd);
            }
            self.drop_target = None;
        }
    }

    /// Layout children horizontally with a small gap (placeholder layout).
    pub fn layout_horizontal(&mut self, start: POINT, gap: i32) {
        let mut x = start.x;
        let y = start.y;
        for child in self.children.iter_mut() {
            let mut rc = child.bounds();
            let w = rc.right - rc.left;
            let h = rc.bottom - rc.top;
            rc.left = x;
            rc.top = y;
            rc.right = x + w;
            rc.bottom = y + h;
            child.set_bounds(rc);
            unsafe {
                let _ = MoveWindow(child.hwnd(), rc.left, rc.top, w, h, true);
            }
            x += w + gap;
        }
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
    }

    /// Dispatch a message to children until handled.
    pub fn handle_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_MOUSEMOVE => self.handle_mouse_move(wparam, lparam),
            WM_LBUTTONDOWN | WM_LBUTTONUP => self.handle_mouse_button(msg, wparam, lparam),
            WM_KEYDOWN | WM_KEYUP | WM_CHAR => self.forward_to_focus(msg, wparam, lparam),
            WM_MOUSELEAVE_CONST => self.handle_mouse_leave(),
            WM_CAPTURECHANGED => {
                if let Some(idx) = self.capturing_idx.take() {
                    if let Some(child) = self.children.get_mut(idx) {
                        child.mouse_exited();
                    }
                    self.hover_idx = None;
                }
                self.capturing_idx = None;
                LRESULT(0)
            }
            _ => self.forward_first(msg, wparam, lparam),
        }
    }

    pub fn focus_child(&mut self, idx: usize) {
        if let Some(prev) = self.focus_idx {
            if let Some(c) = self.children.get_mut(prev) {
                c.focus_changed(false);
            }
        }
        if let Some(c) = self.children.get_mut(idx) {
            c.focus_changed(true);
            self.focus_idx = Some(idx);
            unsafe {
                let _ = SetFocus(c.hwnd());
            }
        }
    }

    fn forward_first(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        for (idx, child) in self.children.iter_mut().enumerate() {
            let res = child.handle_message(msg, wparam, lparam);
            if res.0 != 0 {
                self.focus_idx = Some(idx);
                return res;
            }
        }
        LRESULT(0)
    }

    fn forward_to_focus(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if let Some(idx) = self.focus_idx {
            if let Some(child) = self.children.get_mut(idx) {
                return child.handle_message(msg, wparam, lparam);
            }
        }
        LRESULT(0)
    }

    fn handle_mouse_move(&mut self, _wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            match unsafe { TrackMouseEvent(&mut tme) } {
                Ok(_) => {
                    self.tracking_mouse = true;
                }
                Err(err) => {
                    self.tracking_mouse = false;
                    debug!(
                        target: "gui::container",
                        "TrackMouseEvent failed for hwnd={:?}: {err:?}",
                        self.hwnd
                    );
                }
            }
        }
        let pt = self.to_client_point(lparam);
        if let Some(idx) = self.capturing_idx {
            debug!(
                target: "gui::container",
                "mouse_move capturing idx={} pt=({}, {})",
                idx,
                pt.x,
                pt.y
            );
            let lp = self.repack_lparam(pt);
            if let Some(child) = self.children.get_mut(idx) {
                return child.handle_message(WM_MOUSEMOVE, WPARAM(0), lp);
            }
        }
        let hit_idx = self.hit_test(pt);
        debug!(
            target: "gui::container",
            "mouse_move pt=({}, {}) hit_idx={:?} hover_idx={:?} tracking={}",
            pt.x,
            pt.y,
            hit_idx,
            self.hover_idx,
            self.tracking_mouse
        );
        if hit_idx != self.hover_idx {
            if let Some(prev) = self.hover_idx {
                if let Some(c) = self.children.get_mut(prev) {
                    c.mouse_exited();
                }
            }
            if let Some(new_idx) = hit_idx {
                if let Some(c) = self.children.get_mut(new_idx) {
                    c.mouse_entered();
                }
            }
            self.hover_idx = hit_idx;
            // Force a repaint whenever hover target changes to ensure stale visuals clear.
            unsafe {
                let _ = InvalidateRect(self.hwnd, None, false);
            }
        }
        if let Some(idx) = hit_idx {
            let lp = self.repack_lparam(pt);
            if let Some(child) = self.children.get_mut(idx) {
                return child.handle_message(WM_MOUSEMOVE, WPARAM(0), lp);
            }
        }
        LRESULT(0)
    }

    fn handle_mouse_button(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let pt = self.to_client_point(lparam);
        // If a child previously captured the mouse, always forward button
        // messages to it (even when the cursor is outside its bounds) so it can
        // release its pressed/hover state. This mirrors the legacy CContainerWnd
        // behaviour and prevents buttons from getting stuck when the mouse is
        // released off the control.
        if let Some(idx) = self.capturing_idx {
            let lp = self.repack_lparam(pt);
            if let Some(child) = self.children.get_mut(idx) {
                let res = child.handle_message(msg, wparam, lp);
                if msg == WM_LBUTTONUP {
                    self.capturing_idx = None;
                    // Update hover state based on the release point.
                    if !child.hit_test(pt) {
                        child.mouse_exited();
                        self.hover_idx = None;
                    } else {
                        self.hover_idx = Some(idx);
                    }
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                return res;
            } else {
                self.capturing_idx = None;
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.hover_idx = None;
            }
        }
        if let Some(idx) = self.hit_test(pt) {
            if msg == WM_LBUTTONDOWN {
                self.capturing_idx = Some(idx);
                unsafe {
                    let _ = SetCapture(self.hwnd);
                }
            }
            let lp = self.repack_lparam(pt);
            if let Some(child) = self.children.get_mut(idx) {
                let res = child.handle_message(msg, wparam, lp);
                if msg == WM_LBUTTONUP {
                    self.capturing_idx = None;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                return res;
            }
        }
        LRESULT(0)
    }

    fn handle_mouse_leave(&mut self) -> LRESULT {
        debug!(
            target: "gui::container",
            "mouse_leave clearing hover_idx={:?} capturing_idx={:?}",
            self.hover_idx,
            self.capturing_idx
        );
        // Clear hover state on any component that may have retained visual hover.
        for c in self.children.iter_mut() {
            c.mouse_exited();
        }
        self.hover_idx = None;
        // Force a repaint so any hovered visuals are cleared.
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, false);
        }
        self.tracking_mouse = false;
        LRESULT(0)
    }

    fn hit_test(&self, pt: POINT) -> Option<usize> {
        self.children.iter().position(|c| c.hit_test(pt))
    }

    fn to_client_point(&self, lparam: LPARAM) -> POINT {
        POINT {
            x: (lparam.0 & 0xffff) as i32 as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i32 as i16 as i32,
        }
    }

    fn repack_lparam(&self, pt: POINT) -> LPARAM {
        let packed = ((pt.y as u32) << 16) | (pt.x as u32 & 0xffff);
        LPARAM(packed as isize)
    }

    fn handle_drop(&mut self, files: Vec<String>, pt: POINT) -> bool {
        if let Some(idx) = self.hit_test(pt) {
            if let Some(child) = self.children.get_mut(idx) {
                return child.drop_files(&files, pt);
            }
        }
        false
    }

    fn handle_drag_over(&mut self, pt: POINT) -> bool {
        if let Some(idx) = self.hit_test(pt) {
            if let Some(child) = self.children.get_mut(idx) {
                return child.drag_over(pt);
            }
        }
        false
    }
}

#[implement(windows::Win32::System::Ole::IDropTarget)]
struct ContainerDropTarget {
    hwnd: HWND,
    container_ptr: std::ptr::NonNull<Container>,
}

impl ContainerDropTarget {
    fn new(hwnd: HWND, container_ptr: std::ptr::NonNull<Container>) -> Self {
        Self {
            hwnd,
            container_ptr,
        }
    }

    fn extract_files(&self, data: &IDataObject) -> Option<Vec<String>> {
        unsafe {
            let fmt = windows::Win32::System::Com::FORMATETC {
                cfFormat: CF_HDROP.0 as u16,
                ptd: std::ptr::null_mut(),
                dwAspect: windows::Win32::System::Com::DVASPECT_CONTENT.0 as u32,
                lindex: -1,
                tymed: windows::Win32::System::Com::TYMED_HGLOBAL.0 as u32,
            };
            if let Ok(mut medium) = data.GetData(&fmt) {
                let hdrop = HDROP(medium.u.hGlobal.0);
                let file_count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
                let mut files = Vec::new();
                for i in 0..file_count {
                    let len = DragQueryFileW(hdrop, i, None) + 1;
                    let mut buf = vec![0u16; len as usize];
                    let written = DragQueryFileW(hdrop, i, Some(&mut buf));
                    buf.truncate(written as usize);
                    let s = String::from_utf16_lossy(&buf);
                    files.push(s);
                }
                ReleaseStgMedium(&mut medium);
                return Some(files);
            }
        }
        None
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for ContainerDropTarget_Impl {
    fn DragEnter(
        &self,
        pDataObj: Option<&IDataObject>,
        _grfKeyState: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            *pdwEffect = DROPEFFECT_NONE;
        }
        if pDataObj.is_none() {
            return Ok(());
        }
        // Accept drag if any child wants it.
        let pt_client = POINT { x: pt.x, y: pt.y };
        let accept = unsafe { self.container_ptr.as_ptr().as_mut() }
            .map(|c| c.handle_drag_over(pt_client))
            .unwrap_or(false);
        if accept {
            unsafe {
                *pdwEffect = DROPEFFECT_COPY;
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfKeyState: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let pt_client = POINT { x: pt.x, y: pt.y };
        let accept = unsafe { self.container_ptr.as_ptr().as_mut() }
            .map(|c| c.handle_drag_over(pt_client))
            .unwrap_or(false);
        unsafe {
            *pdwEffect = if accept {
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
        pDataObj: Option<&IDataObject>,
        _grfKeyState: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        pt: &windows::Win32::Foundation::POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            *pdwEffect = DROPEFFECT_NONE;
        }
        if let Some(data) = pDataObj {
            if let Some(files) = self.extract_files(data) {
                let pt_client = POINT { x: pt.x, y: pt.y };
                let handled = unsafe { self.container_ptr.as_ptr().as_mut() }
                    .map(|c| c.handle_drop(files.clone(), pt_client))
                    .unwrap_or(false);
                if handled {
                    unsafe {
                        *pdwEffect = DROPEFFECT_COPY;
                    }
                }
            }
        }
        Ok(())
    }
}
