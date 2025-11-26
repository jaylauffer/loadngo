use std::mem::size_of;

use anyhow::Result;
use windows::Win32::Foundation::{BOOL, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject,
    EndPaint, SelectObject, AC_SRC_ALPHA, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HBITMAP, HDC, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, WM_USER};

/// Legacy constant used by CBufferedWnd to request a repaint.
pub const WM_INVALIDATE: u32 = WM_USER + 515;

/// Simple DIB-section backing store (mirrors loadngoGUI ImgBuffer).
pub struct ImgBuffer {
    pub width: i32,
    pub height: i32,
    pub hbitmap: HBITMAP,
    bits: *mut std::ffi::c_void,
}

impl ImgBuffer {
    pub fn new(dc: HDC, width: i32, height: i32) -> Result<Self> {
        unsafe {
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                biSizeImage: (width * height * 4) as u32,
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let hbitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            // Clear to transparent to avoid garbage/flicker before we paint.
            if !bits.is_null() {
                let len = (width * height * 4) as usize;
                std::ptr::write_bytes(bits, 0, len);
            }
            Ok(Self {
                width,
                height,
                hbitmap,
                bits,
            })
        }
    }
}

impl Drop for ImgBuffer {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.hbitmap);
        }
    }
}

/// Double-buffered painter helper (faithful to loadngoGUI CBufferedWnd).
pub struct BufferedWnd {
    buffer: Option<ImgBuffer>,
}

impl Default for BufferedWnd {
    fn default() -> Self {
        Self { buffer: None }
    }
}

impl BufferedWnd {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle WM_PAINT by rendering into an off-screen buffer then blitting.
    /// The `render` closure should draw the full client area.
    pub fn paint<F>(&mut self, hwnd: HWND, mut render: F) -> Result<()>
    where
        F: FnMut(HWND, HDC, i32, i32) -> Result<()>,
    {
        unsafe {
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            let width = rc.right - rc.left;
            let height = rc.bottom - rc.top;

            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            if dc.0.is_null() {
                return Ok(());
            }

            if let Some(buf) = &self.buffer {
                if buf.width != width || buf.height != height {
                    self.buffer = None;
                }
            }
            if self.buffer.is_none() {
                self.buffer = Some(ImgBuffer::new(dc, width, height)?);
            }
            let buf = self.buffer.as_ref().unwrap();

            let mem_dc = CreateCompatibleDC(dc);
            let old = SelectObject(mem_dc, buf.hbitmap);

            render(hwnd, mem_dc, width, height)?;

            let _ = BitBlt(dc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

            let _ = SelectObject(mem_dc, old);
            let _ = DeleteDC(mem_dc);
            EndPaint(hwnd, &ps);
        }
        Ok(())
    }

    /// Utility to alpha-blend the buffer back to a DC (matches ImgBuffer::Render).
    pub fn render_alpha(&self, dc: HDC, x: i32, y: i32, alpha: u8) {
        if let Some(buf) = &self.buffer {
            unsafe {
                let mem_dc = CreateCompatibleDC(dc);
                let old = SelectObject(mem_dc, buf.hbitmap);
                let bf = BLENDFUNCTION {
                    BlendOp: 0,
                    BlendFlags: 0,
                    SourceConstantAlpha: alpha,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = AlphaBlend(
                    dc, x, y, buf.width, buf.height, mem_dc, 0, 0, buf.width, buf.height, bf,
                );
                let _ = SelectObject(mem_dc, old);
                let _ = DeleteDC(mem_dc);
            }
        }
    }
}
