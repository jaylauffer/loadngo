use windows::Win32::Foundation::{POINT, RECT, SIZE};

pub struct BasePlanItem {
    pub rect: RECT,
    pub children: Vec<BasePlanItem>,
    pub selected: bool,
    pub hilited: bool,
    pub expanded: bool,
}

impl BasePlanItem {
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
}
