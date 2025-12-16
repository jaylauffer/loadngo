//! Windows clipboard helpers for copying task data as JSON.

use crate::persistence;
use crate::task::Task;
use anyhow::Result;
use std::ffi::c_void;
use std::ptr::copy_nonoverlapping;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_UNICODETEXT: u32 = 13;

/// Copy tasks to the clipboard as pretty JSON.
pub fn copy_tasks(tasks: &[Task]) -> Result<()> {
    let json = persistence::tasks_to_json_string(tasks)?;
    set_clipboard_text(&json)
}

/// Attempt to read tasks from the clipboard (expects JSON produced by `copy_tasks`).
pub fn paste_tasks() -> Result<Vec<Task>> {
    if let Some(text) = get_clipboard_text()? {
        return persistence::tasks_from_json_str(&text);
    }
    Ok(Vec::new())
}

fn set_clipboard_text(text: &str) -> Result<()> {
    unsafe {
        OpenClipboard(HWND(std::ptr::null_mut()))?;
        EmptyClipboard()?;

        // Convert to UTF-16 with terminating NUL.
        let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = utf16.len() * std::mem::size_of::<u16>();

        let hmem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
        if hmem.0.is_null() {
            let _ = CloseClipboard();
            anyhow::bail!("GlobalAlloc failed");
        }

        let locked = GlobalLock(hmem) as *mut c_void;
        if locked.is_null() {
            let _ = CloseClipboard();
            anyhow::bail!("GlobalLock failed");
        }
        copy_nonoverlapping(utf16.as_ptr() as *const c_void, locked, bytes);
        let _ = GlobalUnlock(hmem);

        SetClipboardData(CF_UNICODETEXT, HANDLE(hmem.0))?;
        let _ = CloseClipboard();
    }
    Ok(())
}

fn get_clipboard_text() -> Result<Option<String>> {
    unsafe {
        OpenClipboard(HWND(std::ptr::null_mut()))?;
        let handle = match GetClipboardData(CF_UNICODETEXT) {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return Ok(None);
            }
        };
        if handle.0.is_null() {
            let _ = CloseClipboard();
            return Ok(None);
        }
        let ptr = GlobalLock(HGLOBAL(handle.0)) as *const u16;
        if ptr.is_null() {
            let _ = CloseClipboard();
            anyhow::bail!("GlobalLock failed");
        }

        // Read until NUL terminator.
        let mut len = 0usize;
        loop {
            let ch = *ptr.add(len);
            if ch == 0 {
                break;
            }
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        let s = String::from_utf16(slice).unwrap_or_default();
        let _ = GlobalUnlock(HGLOBAL(handle.0));
        let _ = CloseClipboard();
        Ok(Some(s))
    }
}
