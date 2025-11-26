use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DrawFocusRect, Rectangle, SetBkMode, SetDCBrushColor, SetDCPenColor,
    ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE,
    FW_NORMAL, HDC, HFONT, LOGFONTW, OUT_TT_ONLY_PRECIS, TRANSPARENT,
};

use data::types::Id;

use crate::project_planner::base_tree_item::{draw_wrapped_text, BaseTreeItem, TreeItem};
use crate::winutil::to_wstring;

const DEFAULT_SIZE: SIZE = SIZE { cx: 110, cy: 70 };

pub struct EventTreeItem {
    base: BaseTreeItem,
    event_id: Id,
    title: String,
}

impl EventTreeItem {
    pub fn new(event_id: Id, title: String) -> Self {
        Self {
            base: BaseTreeItem::new(),
            event_id,
            title,
        }
    }

    fn font() -> HFONT {
        static INIT: Once = Once::new();
        static mut FONT: HFONT = HFONT(std::ptr::null_mut());
        unsafe {
            INIT.call_once(|| {
                let mut lf: LOGFONTW = std::mem::zeroed();
                lf.lfCharSet = DEFAULT_CHARSET;
                lf.lfClipPrecision = CLIP_DEFAULT_PRECIS;
                lf.lfOutPrecision = OUT_TT_ONLY_PRECIS;
                lf.lfQuality = ANTIALIASED_QUALITY;
                lf.lfPitchAndFamily = (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u8;
                lf.lfHeight = -12;
                lf.lfWeight = FW_NORMAL.0 as i32;
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
}

impl TreeItem for EventTreeItem {
    fn base(&self) -> &BaseTreeItem {
        &self.base
    }

    fn base_mut(&mut self) -> &mut BaseTreeItem {
        &mut self.base
    }

    fn id(&self) -> Id {
        self.event_id
    }

    fn draw_item(&self, dc: HDC) {
        if self.base().selected {
            self.draw_focused_item(dc);
        }
        unsafe {
            SetDCPenColor(dc, COLORREF(0x00000000));
            SetDCBrushColor(dc, COLORREF(0x00c7d4f0));
            Rectangle(
                dc,
                self.base().rect.left,
                self.base().rect.top,
                self.base().rect.right,
                self.base().rect.bottom,
            );
            SetBkMode(dc, TRANSPARENT);
        }
        let mut text_rect = RECT {
            left: self.base().rect.left + 4,
            top: self.base().rect.top + 4,
            right: self.base().rect.right - 4,
            bottom: self.base().rect.bottom - 4,
        };
        draw_wrapped_text(dc, &mut text_rect, Self::font(), &self.title);
    }

    fn draw_focused_item(&self, dc: HDC) {
        let mut rc = RECT {
            left: self.base().rect.left - 4,
            top: self.base().rect.top - 3,
            right: self.base().rect.right + 4,
            bottom: self.base().rect.bottom + 3,
        };
        unsafe {
            DrawFocusRect(dc, &mut rc);
        }
    }

    fn preferred_size(&self) -> SIZE {
        DEFAULT_SIZE
    }

    fn report_name(&self) -> &'static str {
        "event"
    }

    fn to_report_xml(&self) -> String {
        format!("<event><title>{}</title></event>", self.title)
    }

    fn get_estimated_duration(&self) -> f64 {
        1.0
    }

    fn get_performance_percent(&self) -> f64 {
        1.0
    }

    fn get_target_percent(&self) -> f64 {
        1.0
    }

    fn get_user_percent(&self) -> f64 {
        1.0
    }

    fn get_total_time(&self, total: &mut u64) -> u64 {
        *total
    }
}
