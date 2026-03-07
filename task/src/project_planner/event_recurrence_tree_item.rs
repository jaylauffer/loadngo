use windows::Win32::Foundation::SIZE;

use data::types::Id;

use crate::project_planner::base_tree_item::{BaseTreeItem, TreeItem};

const DEFAULT_SIZE: SIZE = SIZE { cx: 110, cy: 70 };

pub struct EventRecurrenceTreeItem {
    base: BaseTreeItem,
    recurrence_id: Id,
    title: String,
}

impl EventRecurrenceTreeItem {
    pub fn new(recurrence_id: Id, title: String) -> Self {
        Self {
            base: BaseTreeItem::new(),
            recurrence_id,
            title,
        }
    }
}

impl TreeItem for EventRecurrenceTreeItem {
    fn base(&self) -> &BaseTreeItem {
        &self.base
    }

    fn base_mut(&mut self) -> &mut BaseTreeItem {
        &mut self.base
    }

    fn id(&self) -> Id {
        self.recurrence_id
    }

    fn draw_item(&self, dc: windows::Win32::Graphics::Gdi::HDC) {
        use windows::Win32::Foundation::{COLORREF, RECT};
        use windows::Win32::Graphics::Gdi::{
            Rectangle, SetBkMode, SetDCBrushColor, SetDCPenColor, TRANSPARENT,
        };
        unsafe {
            SetDCPenColor(dc, COLORREF(0x00000000));
            SetDCBrushColor(dc, COLORREF(0x00f0e2c7));
            Rectangle(
                dc,
                self.base.rect.left,
                self.base.rect.top,
                self.base.rect.right,
                self.base.rect.bottom,
            );
            SetBkMode(dc, TRANSPARENT);
        }
        let mut rc = RECT {
            left: self.base.rect.left + 4,
            top: self.base.rect.top + 4,
            right: self.base.rect.right - 4,
            bottom: self.base.rect.bottom - 4,
        };
        crate::project_planner::base_tree_item::draw_wrapped_text(
            dc,
            &mut rc,
            windows::Win32::Graphics::Gdi::HFONT::default(),
            &self.title,
        );
    }

    fn draw_focused_item(&self, _dc: windows::Win32::Graphics::Gdi::HDC) {}

    fn preferred_size(&self) -> SIZE {
        DEFAULT_SIZE
    }

    fn report_name(&self) -> &'static str {
        "eventrecurrence"
    }

    fn to_report_xml(&self) -> String {
        format!(
            "<eventrecurrence><title>{}</title></eventrecurrence>",
            self.title
        )
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
