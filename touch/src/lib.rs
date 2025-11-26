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

const GLOBAL_MENU_TITLE_LEFT: f32 = 24.0;
const GLOBAL_MENU_TITLE_TOP: f32 = 42.0;
const GLOBAL_MENU_BUTTON_SIDE_INSET: f32 = 20.0;
const GLOBAL_MENU_BUTTON_TOP: f32 = 108.0;
const GLOBAL_MENU_BUTTON_BOTTOM_INSET: f32 = 42.0;
const GLOBAL_MENU_BUTTON_GAP: f32 = 18.0;
const GLOBAL_MENU_BUTTON_MIN_HEIGHT: f32 = 88.0;
const GLOBAL_MENU_BUTTON_MAX_HEIGHT: f32 = 128.0;
const GLOBAL_MENU_BUTTON_MIN_TOUCH_HEIGHT: f32 = 72.0;

fn fit_button_stack(
    available_h: f32,
    button_count: usize,
    preferred_height: f32,
    preferred_gap: f32,
    minimum_height: f32,
) -> (f32, f32, f32) {
    if button_count == 0 {
        return (0.0, 0.0, 0.0);
    }

    let available_h = available_h.max(0.0);
    let gap_count = button_count.saturating_sub(1) as f32;
    let mut button_h = preferred_height.max(0.0);
    let mut gap = preferred_gap.max(0.0);
    let mut used_h = button_count as f32 * button_h + gap_count * gap;

    if used_h > available_h && gap_count > 0.0 {
        let remaining_after_buttons = (available_h - button_count as f32 * button_h).max(0.0);
        gap = (remaining_after_buttons / gap_count)
            .min(preferred_gap)
            .max(0.0);
        used_h = button_count as f32 * button_h + gap_count * gap;
    }

    if used_h > available_h {
        let remaining_after_gaps = (available_h - gap_count * gap).max(0.0);
        button_h = (remaining_after_gaps / button_count as f32).max(minimum_height);
        used_h = button_count as f32 * button_h + gap_count * gap;
    }

    if used_h > available_h {
        let remaining_after_gaps = (available_h - gap_count * gap).max(0.0);
        button_h = remaining_after_gaps / button_count as f32;
        used_h = button_count as f32 * button_h + gap_count * gap;
    }

    (button_h.max(0.0), gap.max(0.0), used_h.max(0.0))
}

fn required_global_menu_panel_height(button_count: usize) -> f32 {
    let button_count = button_count.max(1);
    let gap_count = button_count.saturating_sub(1) as f32;
    GLOBAL_MENU_TITLE_TOP
        + (GLOBAL_MENU_BUTTON_TOP - GLOBAL_MENU_TITLE_TOP)
        + button_count as f32 * GLOBAL_MENU_BUTTON_MIN_HEIGHT
        + gap_count * GLOBAL_MENU_BUTTON_GAP
        + GLOBAL_MENU_BUTTON_BOTTOM_INSET
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
    let x = ((screen_w - button_w) * 0.5).clamp(
        insets.left,
        (screen_w - insets.right - button_w).max(insets.left),
    );
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
        TouchOverlayKind::SaveLoad
        | TouchOverlayKind::SoundSettings
        | TouchOverlayKind::InputSettings => (safe_h * 0.88).clamp(420.0, 900.0),
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
    let button_w = panel.width - GLOBAL_MENU_BUTTON_SIDE_INSET * 2.0;
    let button_x = panel.x + GLOBAL_MENU_BUTTON_SIDE_INSET;
    let top_y = panel.y + GLOBAL_MENU_BUTTON_TOP;
    let bottom_y = panel.y + panel.height - GLOBAL_MENU_BUTTON_BOTTOM_INSET;
    let available_h = (bottom_y - top_y).max(0.0);
    let preferred_h = ((available_h
        - GLOBAL_MENU_BUTTON_GAP * (button_count.saturating_sub(1)) as f32)
        / button_count as f32)
        .clamp(GLOBAL_MENU_BUTTON_MIN_HEIGHT, GLOBAL_MENU_BUTTON_MAX_HEIGHT);
    let (button_h, gap, used_h) = fit_button_stack(
        available_h,
        button_count,
        preferred_h,
        GLOBAL_MENU_BUTTON_GAP,
        GLOBAL_MENU_BUTTON_MIN_TOUCH_HEIGHT,
    );
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
    let mut panel = overlay_panel_rect(screen_w, screen_h, TouchOverlayKind::GlobalMenu);
    let required_h = required_global_menu_panel_height(button_count);
    if required_h > panel.height {
        let growth = required_h - panel.height;
        let max_y = (screen_h - panel.height - growth).max(0.0);
        panel = TouchRect::new(
            panel.x,
            (panel.y - growth * 0.5).clamp(0.0, max_y),
            panel.width,
            panel.height + growth,
        );
    }
    TouchGlobalMenuLayout {
        panel,
        title_x: panel.x + GLOBAL_MENU_TITLE_LEFT,
        title_y: panel.y + GLOBAL_MENU_TITLE_TOP,
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
        panel.x + GLOBAL_MENU_TITLE_LEFT,
        panel.y + panel.height - 102.0,
        panel.width - GLOBAL_MENU_TITLE_LEFT * 2.0,
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
        TouchOverlayKind::SaveLoad
        | TouchOverlayKind::SoundSettings
        | TouchOverlayKind::InputSettings => TouchOverlayStyle {
            scrim: RgbaColor::rgba(4, 6, 10, 175),
            panel_fill: RgbaColor::rgba(12, 16, 24, 214),
            panel_border: RgbaColor::rgba(180, 188, 212, 220),
            panel_border_thickness: 4,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{global_menu_layout, overlay_back_button_rect};

    #[test]
    fn global_menu_buttons_stay_within_panel_bounds() {
        let layout = global_menu_layout(393.0, 852.0, 4);
        let last = layout.button_rects.last().unwrap();

        assert!(layout.button_rects[0].height >= 72.0);
        assert!(last.y + last.height <= layout.panel.y + layout.panel.height - 42.0 + 0.001);
    }

    #[test]
    fn global_menu_panel_grows_when_needed() {
        let layout = global_menu_layout(360.0, 640.0, 4);
        let last = layout.button_rects.last().unwrap();

        assert!(layout.panel.height >= 556.0);
        assert!(last.y + last.height <= layout.panel.y + layout.panel.height - 42.0 + 0.001);
    }

    #[test]
    fn overlay_back_button_stays_inside_panel() {
        let layout = global_menu_layout(393.0, 852.0, 4);
        let back = overlay_back_button_rect(layout.panel);

        assert!(back.y >= layout.panel.y);
        assert!(back.y + back.height <= layout.panel.y + layout.panel.height);
    }
}
