#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl TouchRect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchInsets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchOverlayKind {
    GlobalMenu,
    SaveLoad,
    SoundSettings,
    InputSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbaColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RgbaColor {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchButtonStyle {
    pub shadow: RgbaColor,
    pub halo: RgbaColor,
    pub fill: RgbaColor,
    pub border: RgbaColor,
    pub text: RgbaColor,
    pub font_size: u16,
    pub border_thickness: u16,
    pub label_baseline_offset: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchButtonVisualState {
    Normal,
    Hovered,
    Pressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchOverlayStyle {
    pub scrim: RgbaColor,
    pub panel_fill: RgbaColor,
    pub panel_border: RgbaColor,
    pub panel_border_thickness: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TouchGlobalMenuLayout {
    pub panel: TouchRect,
    pub title_x: f32,
    pub title_y: f32,
    pub button_rects: Vec<TouchRect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TouchMenuButtonPresentation<'a> {
    pub rect: TouchRect,
    pub label: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TouchGlobalMenuPresentation<'a> {
    pub panel: TouchRect,
    pub title: &'a str,
    pub title_x: f32,
    pub title_y: f32,
    pub buttons: Vec<TouchMenuButtonPresentation<'a>>,
}

#[derive(Debug, Default)]
pub struct TouchLayoutCache {
    cached_global_menu: Option<CachedGlobalMenuLayout>,
}

#[derive(Debug, Clone, PartialEq)]
struct CachedGlobalMenuLayout {
    screen_w: u32,
    screen_h: u32,
    button_count: usize,
    layout: TouchGlobalMenuLayout,
}

pub fn safe_insets(screen_w: f32, screen_h: f32) -> TouchInsets {
    let edge = (screen_w.min(screen_h) * 0.035).clamp(20.0, 34.0);
    let top = (screen_h * 0.045).clamp(28.0, 48.0);
    let bottom = (screen_h * 0.04).clamp(22.0, 44.0);
    TouchInsets {
        left: edge,
        top,
        right: edge,
        bottom,
    }
}

pub fn menu_button_rect(screen_w: f32, screen_h: f32) -> TouchRect {
    let insets = safe_insets(screen_w, screen_h);
    let button_w = (screen_w * 0.5).clamp(340.0, 500.0);
    let button_h = 136.0;
    let x = ((screen_w - button_w) * 0.5)
        .clamp(insets.left, (screen_w - insets.right - button_w).max(insets.left));
    let y = insets.top + (screen_h * 0.11).clamp(220.0, 320.0);
    TouchRect::new(x, y, button_w, button_h)
}

pub fn overlay_panel_rect(screen_w: f32, screen_h: f32, kind: TouchOverlayKind) -> TouchRect {
    let insets = safe_insets(screen_w, screen_h);
    let horizontal_margin = if screen_w < 520.0 { 12.0 } else { 24.0 };
    let vertical_margin = if screen_h < 520.0 { 12.0 } else { 24.0 };
    let left = insets.left + horizontal_margin;
    let top = insets.top + vertical_margin;
    let right = insets.right + horizontal_margin;
    let bottom = insets.bottom + vertical_margin;
    let safe_w = (screen_w - left - right).max(1.0);
    let safe_h = (screen_h - top - bottom).max(1.0);
    let panel_w = (safe_w * 0.94).clamp(380.0, 900.0).min(safe_w);
    let panel_h = match kind {
        TouchOverlayKind::GlobalMenu => (safe_h * 0.8).clamp(560.0, 860.0),
        TouchOverlayKind::SaveLoad | TouchOverlayKind::SoundSettings | TouchOverlayKind::InputSettings => {
            (safe_h * 0.88).clamp(420.0, 900.0)
        }
    }
    .min(safe_h);

    TouchRect::new(
        left + (safe_w - panel_w) * 0.5,
        top + (safe_h - panel_h) * 0.5,
        panel_w,
        panel_h,
    )
}

pub fn global_menu_button_rects(panel: TouchRect, button_count: usize) -> Vec<TouchRect> {
    let button_count = button_count.max(1);
    let button_w = panel.width - 40.0;
    let button_x = panel.x + 20.0;
    let top_y = panel.y + 112.0;
    let bottom_y = panel.y + panel.height - 36.0;
    let gap = 24.0;
    let available_h = (bottom_y - top_y).max(140.0);
    let button_h =
        ((available_h - gap * (button_count as f32 - 1.0)) / button_count as f32).clamp(112.0, 148.0);
    let used_h = button_h * button_count as f32 + gap * (button_count as f32 - 1.0);
    let mut button_y = top_y + ((available_h - used_h) * 0.5).max(0.0);
    let mut rects = Vec::with_capacity(button_count);
    for _ in 0..button_count {
        rects.push(TouchRect::new(button_x, button_y, button_w, button_h));
        button_y += button_h + gap;
    }
    rects
}

pub fn global_menu_layout(
    screen_w: f32,
    screen_h: f32,
    button_count: usize,
) -> TouchGlobalMenuLayout {
    let panel = overlay_panel_rect(screen_w, screen_h, TouchOverlayKind::GlobalMenu);
    TouchGlobalMenuLayout {
        panel,
        title_x: panel.x + 24.0,
        title_y: panel.y + 42.0,
        button_rects: global_menu_button_rects(panel, button_count),
    }
}

impl TouchLayoutCache {
    pub fn global_menu_layout(
        &mut self,
        screen_w: f32,
        screen_h: f32,
        button_count: usize,
    ) -> TouchGlobalMenuLayout {
        let key_w = screen_w.max(0.0).round() as u32;
        let key_h = screen_h.max(0.0).round() as u32;
        if let Some(cached) = &self.cached_global_menu {
            if cached.screen_w == key_w
                && cached.screen_h == key_h
                && cached.button_count == button_count
            {
                return cached.layout.clone();
            }
        }
        let layout = global_menu_layout(screen_w, screen_h, button_count);
        self.cached_global_menu = Some(CachedGlobalMenuLayout {
            screen_w: key_w,
            screen_h: key_h,
            button_count,
            layout: layout.clone(),
        });
        layout
    }

    pub fn present_global_menu<'a>(
        &mut self,
        screen_w: f32,
        screen_h: f32,
        title: &'a str,
        labels: &[&'a str],
    ) -> TouchGlobalMenuPresentation<'a> {
        let layout = self.global_menu_layout(screen_w, screen_h, labels.len());
        let buttons = layout
            .button_rects
            .iter()
            .copied()
            .zip(labels.iter().copied())
            .map(|(rect, label)| TouchMenuButtonPresentation { rect, label })
            .collect();
        TouchGlobalMenuPresentation {
            panel: layout.panel,
            title,
            title_x: layout.title_x,
            title_y: layout.title_y,
            buttons,
        }
    }
}

pub fn overlay_back_button_rect(panel: TouchRect) -> TouchRect {
    TouchRect::new(
        panel.x + 24.0,
        panel.y + panel.height - 102.0,
        panel.width - 48.0,
        72.0,
    )
}

pub fn menu_button_style(state: TouchButtonVisualState) -> TouchButtonStyle {
    match state {
        TouchButtonVisualState::Pressed => TouchButtonStyle {
            shadow: RgbaColor::rgba(2, 4, 10, 74),
            halo: RgbaColor::rgba(130, 176, 244, 112),
            fill: RgbaColor::rgba(72, 112, 178, 252),
            border: RgbaColor::rgba(255, 255, 255, 255),
            text: RgbaColor::rgba(255, 255, 255, 255),
            font_size: 40,
            border_thickness: 6,
            label_baseline_offset: -12,
        },
        TouchButtonVisualState::Hovered => TouchButtonStyle {
            shadow: RgbaColor::rgba(2, 4, 10, 92),
            halo: RgbaColor::rgba(96, 144, 228, 96),
            fill: RgbaColor::rgba(54, 88, 146, 252),
            border: RgbaColor::rgba(248, 252, 255, 255),
            text: RgbaColor::rgba(255, 255, 255, 255),
            font_size: 40,
            border_thickness: 5,
            label_baseline_offset: -14,
        },
        TouchButtonVisualState::Normal => TouchButtonStyle {
            shadow: RgbaColor::rgba(2, 4, 10, 92),
            halo: RgbaColor::rgba(24, 42, 74, 28),
            fill: RgbaColor::rgba(24, 40, 72, 246),
            border: RgbaColor::rgba(232, 238, 252, 252),
            text: RgbaColor::rgba(228, 234, 248, 245),
            font_size: 40,
            border_thickness: 4,
            label_baseline_offset: -14,
        },
    }
}

pub fn overlay_style(kind: TouchOverlayKind) -> TouchOverlayStyle {
    match kind {
        TouchOverlayKind::GlobalMenu => TouchOverlayStyle {
            scrim: RgbaColor::rgba(2, 4, 10, 152),
            panel_fill: RgbaColor::rgba(12, 20, 36, 230),
            panel_border: RgbaColor::rgba(234, 240, 252, 248),
            panel_border_thickness: 4,
        },
        TouchOverlayKind::SaveLoad | TouchOverlayKind::SoundSettings | TouchOverlayKind::InputSettings => {
            TouchOverlayStyle {
                scrim: RgbaColor::rgba(4, 6, 10, 175),
                panel_fill: RgbaColor::rgba(12, 16, 24, 214),
                panel_border: RgbaColor::rgba(180, 188, 212, 220),
                panel_border_thickness: 4,
            }
        }
    }
}
