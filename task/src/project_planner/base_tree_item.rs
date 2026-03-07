use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    DrawTextW, Ellipse, LineTo, MoveToEx, SelectObject, SetBkMode, SetDCBrushColor, SetDCPenColor,
    HFONT, HGDIOBJ, TRANSPARENT,
};

use data::types::Id;

use crate::winutil::to_wstring;

pub struct BaseTreeItem {
    pub rect: RECT,
    pub children: Vec<Box<dyn TreeItem>>,
    pub selected: bool,
    pub hilited: bool,
    pub expanded: bool,
}

impl BaseTreeItem {
    pub fn new() -> Self {
        Self {
            rect: RECT::default(),
            children: Vec::new(),
            selected: false,
            hilited: false,
            expanded: true,
        }
    }

    pub fn place_item(&mut self, x: i32, y: i32, size: SIZE) {
        self.rect.left = x - size.cx / 2;
        self.rect.top = y;
        self.rect.right = self.rect.left + size.cx;
        self.rect.bottom = self.rect.top + size.cy;
    }

    pub fn center(&self) -> POINT {
        POINT {
            x: self.rect.left + (self.rect.right - self.rect.left) / 2,
            y: self.rect.top + (self.rect.bottom - self.rect.top) / 2,
        }
    }

    pub fn connector_start(&self) -> POINT {
        POINT {
            x: self.rect.left + (self.rect.right - self.rect.left) / 2,
            y: self.rect.bottom + 5,
        }
    }

    pub fn right_center(&self) -> POINT {
        POINT {
            x: self.rect.right,
            y: self.rect.top + (self.rect.bottom - self.rect.top) / 2,
        }
    }

    pub fn left_center(&self) -> POINT {
        POINT {
            x: self.rect.left,
            y: self.rect.top + (self.rect.bottom - self.rect.top) / 2,
        }
    }

    pub fn hit_test_expand(&self, pt: POINT) -> bool {
        let cx = self.rect.left + (self.rect.right - self.rect.left) / 2;
        let toggle = RECT {
            left: cx - 5,
            top: self.rect.bottom,
            right: cx + 5,
            bottom: self.rect.bottom + 10,
        };
        point_in_rect(pt, toggle)
    }

    pub fn paint_expand_switch(
        &self,
        dc: windows::Win32::Graphics::Gdi::HDC,
        fill: COLORREF,
        pen: COLORREF,
    ) {
        if self.children.is_empty() {
            return;
        }
        let pt = self.connector_start();
        unsafe {
            SetDCBrushColor(dc, fill);
            SetDCPenColor(dc, pen);
            Ellipse(dc, pt.x - 5, pt.y - 5, pt.x + 5, pt.y + 5);
            let _ = MoveToEx(dc, pt.x - 3, pt.y, None);
            let _ = LineTo(dc, pt.x + 3, pt.y);
            if !self.expanded {
                let _ = MoveToEx(dc, pt.x, pt.y - 3, None);
                let _ = LineTo(dc, pt.x, pt.y + 3);
            }
        }
    }
}

pub trait TreeItem {
    fn base(&self) -> &BaseTreeItem;
    fn base_mut(&mut self) -> &mut BaseTreeItem;
    fn id(&self) -> Id;
    fn draw_item(&self, dc: windows::Win32::Graphics::Gdi::HDC);
    fn draw_focused_item(&self, dc: windows::Win32::Graphics::Gdi::HDC);
    fn preferred_size(&self) -> SIZE;
    fn report_name(&self) -> &'static str;
    fn to_report_xml(&self) -> String;
    fn get_estimated_duration(&self) -> f64;
    fn get_performance_percent(&self) -> f64;
    fn get_target_percent(&self) -> f64;
    fn get_user_percent(&self) -> f64;
    fn get_total_time(&self, total: &mut u64) -> u64;

    fn scale_modifier(&self) -> f64 {
        1.0
    }

    fn default_point_width(&self) -> f64 {
        1.0
    }

    fn place_item(&mut self, x: i32, y: i32) {
        let size = self.preferred_size();
        self.base_mut().place_item(x, y, size);
    }

    fn height(&self) -> i32 {
        self.preferred_size().cy
    }

    fn is_expanded(&self) -> bool {
        self.base().expanded
    }

    fn set_expanded(&mut self, expanded: bool) {
        self.base_mut().expanded = expanded;
    }

    fn toggle_expanded(&mut self) -> bool {
        let new_value = !self.is_expanded();
        self.set_expanded(new_value);
        new_value
    }

    fn get_point_width(&self) -> f64 {
        let mut width = self.preferred_size().cx as f64 + 10.0;
        if self.is_expanded() && !self.base().children.is_empty() {
            let mut sum = 0.0;
            for child in self.base().children.iter() {
                sum += child.get_point_width();
            }
            if sum > width {
                width = sum;
            }
        }
        width
    }

    fn get_item_locations(&self, offset: f64, list: &mut Vec<f64>, add_self: bool) {
        let value = (self.get_point_width() / 2.0) + offset;
        if add_self {
            list.push(value);
        }
        if self.is_expanded() {
            let mut temp_offset = offset;
            let kids_len = self.base().children.len();
            if kids_len == 1 {
                let mut child_width = self.base().children[0].get_point_width();
                let min = self.preferred_size().cx as f64 + 10.0;
                if child_width < min {
                    child_width = min;
                }
                list.push((child_width / 2.0) + temp_offset);
            } else {
                for child in self.base().children.iter() {
                    let child_width = child.get_point_width();
                    list.push((child_width / 2.0) + temp_offset);
                    temp_offset += child_width;
                }
            }
            temp_offset = offset;
            for child in self.base().children.iter() {
                child.get_item_locations(temp_offset, list, false);
                temp_offset += child.get_point_width();
            }
        }
    }

    fn hit_test_expand(&self, pt: POINT) -> bool {
        self.base().hit_test_expand(pt)
    }
}

pub fn point_in_rect(pt: POINT, rc: RECT) -> bool {
    pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
}

pub fn hit_test_tree(item: &dyn TreeItem, pt: POINT) -> Option<(Id, bool)> {
    if point_in_rect(pt, item.base().rect) {
        return Some((item.id(), false));
    }
    if item.hit_test_expand(pt) {
        return Some((item.id(), true));
    }
    if item.is_expanded() {
        for child in item.base().children.iter() {
            if let Some(hit) = hit_test_tree(child.as_ref(), pt) {
                return Some(hit);
            }
        }
    }
    None
}

pub fn set_selected(item: &mut dyn TreeItem, selected_id: Option<Id>) {
    let is_selected = Some(item.id()) == selected_id;
    item.base_mut().selected = is_selected;
    for child in item.base_mut().children.iter_mut() {
        set_selected(child.as_mut(), selected_id);
    }
}

pub fn toggle_expanded_by_id(item: &mut dyn TreeItem, id: Id) -> bool {
    if item.id() == id {
        item.toggle_expanded();
        return true;
    }
    for child in item.base_mut().children.iter_mut() {
        if toggle_expanded_by_id(child.as_mut(), id) {
            return true;
        }
    }
    false
}

pub fn draw_wrapped_text(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: &mut RECT,
    font: HFONT,
    text: &str,
) -> SIZE {
    let mut w = to_wstring(text);
    if !w.is_empty() {
        w.pop();
    }
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let _ = SetBkMode(dc, TRANSPARENT);
        let _ = DrawTextW(
            dc,
            &mut w,
            rect,
            windows::Win32::Graphics::Gdi::DT_WORDBREAK
                | windows::Win32::Graphics::Gdi::DT_NOPREFIX,
        );
        let _ = SelectObject(dc, old);
    }
    SIZE {
        cx: rect.right - rect.left,
        cy: rect.bottom - rect.top,
    }
}

pub fn measure_text(dc: windows::Win32::Graphics::Gdi::HDC, font: HFONT, text: &str) -> SIZE {
    let mut rect = RECT::default();
    let mut w = to_wstring(text);
    if !w.is_empty() {
        w.pop();
    }
    unsafe {
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let _ = DrawTextW(
            dc,
            &mut w,
            &mut rect,
            windows::Win32::Graphics::Gdi::DT_CALCRECT | windows::Win32::Graphics::Gdi::DT_NOPREFIX,
        );
        let _ = SelectObject(dc, old);
    }
    SIZE {
        cx: rect.right - rect.left,
        cy: rect.bottom - rect.top,
    }
}
