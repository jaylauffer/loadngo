use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DrawFocusRect, FrameRect, GetDC, GetStockObject, Rectangle, ReleaseDC,
    SelectObject, SetBkMode, SetDCBrushColor, SetDCPenColor, TextOutW, ANTIALIASED_QUALITY,
    CLIP_DEFAULT_PRECIS, DC_BRUSH, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE, FW_BOLD, HBRUSH,
    HDC, HFONT, HGDIOBJ, LOGFONTW, OUT_TT_ONLY_PRECIS, TRANSPARENT,
};

use data::types::Id;

use crate::project_planner::base_task_item::BaseTaskItem;
use crate::project_planner::base_tree_item::{measure_text, BaseTreeItem, TreeItem};
use crate::winutil::to_wstring;

const DEFAULT_SIZE: SIZE = SIZE { cx: 250, cy: 92 };

pub struct ProjectTreeItem {
    base_task: BaseTaskItem,
}

impl ProjectTreeItem {
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
                lf.lfHeight = -17;
                lf.lfWeight = FW_BOLD.0 as i32;
                let face = to_wstring("Verdana");
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

impl TreeItem for ProjectTreeItem {
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
            SetDCBrushColor(dc, self.base_task.background_color());
            SetDCPenColor(dc, COLORREF(0x00000000));
            Rectangle(
                dc,
                self.base().rect.left,
                self.base().rect.top,
                self.base().rect.right,
                self.base().rect.bottom - 13,
            );
            SetBkMode(dc, TRANSPARENT);
            let old = SelectObject(dc, HGDIOBJ(Self::font().0));
            let title = self.base_task.task_name();
            let mut w = to_wstring(&title);
            if !w.is_empty() {
                w.pop();
            }
            let _ = TextOutW(dc, self.base().rect.left + 10, self.base().rect.top + 3, &w);
            let _ = SelectObject(dc, old);

            let status_rect = RECT {
                left: self.base().rect.left,
                top: self.base().rect.bottom - 12,
                right: self.base().rect.right,
                bottom: self.base().rect.bottom,
            };
            self.base_task.paint_status_graph(dc, status_rect);

            SetDCBrushColor(dc, COLORREF(0x00000000));
            let _ = FrameRect(dc, &self.base().rect, HBRUSH(GetStockObject(DC_BRUSH).0));
        }
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
        unsafe {
            let dc = GetDC(windows::Win32::Foundation::HWND::default());
            if dc.0.is_null() {
                return DEFAULT_SIZE;
            }
            let title = self.base_task.task_name();
            let text_size = measure_text(dc, Self::font(), &title);
            let _ = ReleaseDC(windows::Win32::Foundation::HWND::default(), dc);
            let mut size = DEFAULT_SIZE;
            size.cx = (text_size.cx + 20).max(DEFAULT_SIZE.cx);
            size
        }
    }

    fn report_name(&self) -> &'static str {
        "project"
    }

    fn to_report_xml(&self) -> String {
        let task = self.base_task.task();
        let title = task.map(|t| t.name.clone()).unwrap_or_default();
        format!("<project><title>{}</title></project>", title)
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

    fn set_expanded(&mut self, _expanded: bool) {
        self.base_task.base.expanded = true;
    }

    fn hit_test_expand(&self, _pt: POINT) -> bool {
        false
    }
}
