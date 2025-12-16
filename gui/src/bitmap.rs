use anyhow::Result;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, BITMAP, HBITMAP};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    LoadImageW, IMAGE_BITMAP, LR_CREATEDIBSECTION, LR_DEFAULTCOLOR, LR_SHARED,
};

/// Simple bitmap loader with drop-based cleanup.
pub struct Bitmap {
    pub handle: HBITMAP,
    pub width: i32,
    pub height: i32,
}

impl Bitmap {
    pub fn load_resource(res_id: u16) -> Result<Self> {
        unsafe {
            let hinst = HINSTANCE(GetModuleHandleW(None)?.0);
            let hbm_raw = LoadImageW(
                hinst,
                windows::core::PCWSTR(res_id as usize as *const u16),
                IMAGE_BITMAP,
                0,
                0,
                LR_CREATEDIBSECTION | LR_DEFAULTCOLOR | LR_SHARED,
            )
            .map_err(|e| anyhow::anyhow!("LoadImageW failed: {e:?}"))?;
            let hbm = HBITMAP(hbm_raw.0);

            let mut bmp = BITMAP::default();
            GetObjectW(
                hbm,
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bmp as *mut _ as *mut _),
            );
            Ok(Self {
                handle: hbm,
                width: bmp.bmWidth,
                height: bmp.bmHeight,
            })
        }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.handle);
        }
    }
}
