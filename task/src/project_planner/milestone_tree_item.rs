use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DrawFocusRect, FrameRect, GetStockObject, Rectangle, SetBkMode,
    SetDCBrushColor, SetDCPenColor, ANTIALIASED_QUALITY, CLIP_DEFAULT_PRECIS, DC_BRUSH,
    DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_BOLD, HBRUSH, HDC, HFONT, LOGFONTW,
    OUT_TT_ONLY_PRECIS, TRANSPARENT,
};

use data::types::Id;

use crate::project_planner::base_task_item::BaseTaskItem;
use crate::project_planner::base_tree_item::{draw_wrapped_text, BaseTreeItem, TreeItem};
use crate::winutil::to_wstring;

const DEFAULT_SIZE: SIZE = SIZE { cx: 100, cy: 86 };

pub struct MilestoneTreeItem {
    base_task: BaseTaskItem,
}

impl MilestoneTreeItem {
    pub fn new(task_id: Id, service: *mut data::service::Service) -> Self {
        Self {
            base_task: BaseTaskItem::new(task_id, service),
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
                lf.lfHeight = -13;
                lf.lfWeight = FW_BOLD.0 as i32;
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

impl TreeItem for MilestoneTreeItem {
    fn base(&self) -> &BaseTreeItem {
        &self.base_task.base
    }

    fn base_mut(&mut self) -> &mut BaseTreeItem {
        &mut self.base_task.base
    }

    fn id(&self) -> Id {
        self.base_task.task_id
    }

    fn draw_item(&self, dc: HDC) {
        if self.base().selected {
            self.draw_focused_item(dc);
        }
        unsafe {
            SetDCPenColor(dc, COLORREF(0x00000000));
            SetDCBrushColor(dc, self.base_task.background_color());
            Rectangle(
                dc,
                self.base().rect.left,
                self.base().rect.top,
                self.base().rect.right,
                self.base().rect.bottom - 12,
            );
            SetBkMode(dc, TRANSPARENT);
        }
        let mut text_rect = RECT {
            left: self.base().rect.left + 3,
            top: self.base().rect.top + 3,
            right: self.base().rect.right - 3,
            bottom: self.base().rect.bottom - 12,
        };
        draw_wrapped_text(
            dc,
            &mut text_rect,
            Self::font(),
            &self.base_task.task_name(),
        );

        let status_rect = RECT {
            left: self.base().rect.left,
            top: self.base().rect.bottom - 12,
            right: self.base().rect.right,
            bottom: self.base().rect.bottom,
        };
        self.base_task.paint_status_graph(dc, status_rect);
        unsafe {
            SetDCBrushColor(dc, COLORREF(0x00000000));
            let _ = FrameRect(dc, &self.base().rect, HBRUSH(GetStockObject(DC_BRUSH).0));
        }
        self.base_task
            .base
            .paint_expand_switch(dc, COLORREF(0x00ffffff), COLORREF(0x00101010));
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
        "milestone"
    }

    fn to_report_xml(&self) -> String {
        let task = self.base_task.task();
        let title = task.map(|t| t.name.clone()).unwrap_or_default();
        format!("<milestone><title>{}</title></milestone>", title)
    }

    fn get_estimated_duration(&self) -> f64 {
        self.base_task.estimated_duration() as f64
    }

    fn get_performance_percent(&self) -> f64 {
        self.base_task.performance_percent()
    }

    fn get_target_percent(&self) -> f64 {
        self.base_task.target_percent()
    }

    fn get_user_percent(&self) -> f64 {
        self.base_task.user_percent()
    }

    fn get_total_time(&self, total: &mut u64) -> u64 {
        *total += self.base_task.actual_time();
        for child in self.base().children.iter() {
            child.get_total_time(total);
        }
        *total
    }

    fn set_expanded(&mut self, expanded: bool) {
        self.base_task.set_expanded(expanded);
    }

    fn default_point_width(&self) -> f64 {
        1.2
    }
}
