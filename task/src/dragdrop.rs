//! Minimal OLE drag-drop helpers inspired by the legacy Winbase implementation.
//! Provides a simple IDropTarget that reports CF_HDROP and CF_UNICODETEXT drops
//! to a user-supplied callback.

use std::{ffi::OsString, os::windows::ffi::OsStringExt, sync::Arc};

use anyhow::Result;
use windows::core::implement;
use windows::Win32::{
    Foundation::{HWND, POINTL},
    System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL},
    System::Ole::{
        IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
        CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
    },
    System::SystemServices::MODIFIERKEYS_FLAGS,
    UI::Shell::{DragFinish, DragQueryFileW, HDROP},
};

/// Helper to register a drop target for a window.
pub fn register_drop_target<F>(hwnd: HWND, handler: F) -> Result<IDropTarget>
where
    F: Fn(DropPayload) -> Result<()> + Send + Sync + 'static,
{
    let target = DropTarget::new(hwnd, handler);
    let com_target: IDropTarget = target.into();
    unsafe {
        RegisterDragDrop(hwnd, &com_target)?;
    }
    Ok(com_target)
}

/// Unregister the drop target (call on window teardown).
pub fn revoke_drop_target(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

/// Data extracted from a drop.
pub enum DropPayload {
    Files(Vec<String>),
    Text(String),
}

/// Simple IDropTarget implementation that forwards drops to a callback.
#[implement(IDropTarget)]
pub struct DropTarget {
    hwnd: HWND,
    handler: Arc<dyn Fn(DropPayload) -> Result<()> + Send + Sync>,
}

impl DropTarget {
    pub fn new<F>(hwnd: HWND, handler: F) -> Self
    where
        F: Fn(DropPayload) -> Result<()> + Send + Sync + 'static,
    {
        Self {
            hwnd,
            handler: Arc::new(handler),
        }
    }

    fn extract_files(&self, data: &IDataObject) -> Option<Vec<String>> {
        unsafe {
            let fmt = FORMATETC {
                cfFormat: CF_HDROP.0 as u16,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0 as u32,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            if let Ok(mut medium) = data.GetData(&fmt) {
                let hdrop = HDROP(unsafe { medium.u.hGlobal }.0);
                let file_count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
                let mut files = Vec::new();
                for i in 0..file_count {
                    let len = DragQueryFileW(hdrop, i, None) + 1;
                    let mut buf = vec![0u16; len as usize];
                    let written = DragQueryFileW(hdrop, i, Some(&mut buf));
                    buf.truncate(written as usize);
                    let s = OsString::from_wide(&buf).to_string_lossy().to_string();
                    files.push(s);
                }
                DragFinish(hdrop);
                ReleaseStgMedium(&mut medium);
                return Some(files);
            }
        }
        None
    }

    fn extract_text(&self, data: &IDataObject) -> Option<String> {
        unsafe {
            let fmt = FORMATETC {
                cfFormat: CF_UNICODETEXT.0 as u16,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0 as u32,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            if let Ok(mut medium) = data.GetData(&fmt) {
                let hglobal = unsafe { medium.u.hGlobal };
                let ptr = windows::Win32::System::Memory::GlobalLock(hglobal);
                if !ptr.is_null() {
                    // Treat as wide string
                    let mut len = 0;
                    let mut cur = ptr as *const u16;
                    while unsafe { *cur } != 0 {
                        len += 1;
                        cur = unsafe { cur.add(1) };
                    }
                    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, len) };
                    let text = OsString::from_wide(slice).to_string_lossy().to_string();
                    let _ = windows::Win32::System::Memory::GlobalUnlock(hglobal);
                    ReleaseStgMedium(&mut medium);
                    return Some(text);
                }
                ReleaseStgMedium(&mut medium);
            }
        }
        None
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        pDataObj: Option<&IDataObject>,
        _grfKeyState: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            *pdwEffect = DROPEFFECT_COPY;
        }
        // Just accept; real validation can happen on Drop.
        if pDataObj.is_none() {
            unsafe {
                *pdwEffect = DROPEFFECT_NONE;
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfKeyState: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            *pdwEffect = DROPEFFECT_COPY;
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        pDataObj: Option<&IDataObject>,
        _grfKeyState: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdwEffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        unsafe {
            *pdwEffect = DROPEFFECT_COPY;
        }
        if let Some(data) = pDataObj {
            if let Some(files) = self.extract_files(data) {
                let _ = (self.handler)(DropPayload::Files(files));
                return Ok(());
            }
            if let Some(text) = self.extract_text(data) {
                let _ = (self.handler)(DropPayload::Text(text));
                return Ok(());
            }
        }
        Ok(())
    }
}
