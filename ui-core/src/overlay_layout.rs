use crate::geometry::{Insets, Rect};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayStackMetrics {
    pub content_padding: Insets,
    pub title_gap: f32,
    pub section_gap: f32,
    pub action_gap: f32,
    pub action_height: f32,
    pub action_outsets: Insets,
}

impl Default for OverlayStackMetrics {
    fn default() -> Self {
        Self {
            content_padding: Insets::default(),
            title_gap: 0.0,
            section_gap: 0.0,
            action_gap: 0.0,
            action_height: 0.0,
            action_outsets: Insets::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OverlayStackLayout {
    pub panel_bounds: Rect,
    pub content_rect: Rect,
    pub title_rect: Rect,
    pub body_rect: Rect,
    pub action_rects: Vec<Rect>,
}

impl OverlayStackLayout {
    fn fitted_action_stack(
        metrics: OverlayStackMetrics,
        action_count: usize,
        available_height: f32,
    ) -> (f32, f32, f32) {
        if action_count == 0 {
            return (0.0, 0.0, 0.0);
        }

        let requested_height = metrics.action_height.max(0.0);
        let requested_gap = metrics.action_gap.max(0.0);
        let available_height = available_height.max(0.0);
        let gap_count = action_count.saturating_sub(1) as f32;
        let minimum_height = requested_height.min(44.0);

        let mut action_height = requested_height;
        let mut action_gap = requested_gap;
        let mut used_height = action_count as f32 * action_height + gap_count * action_gap;

        if used_height > available_height && gap_count > 0.0 {
            let remaining_after_buttons =
                (available_height - action_count as f32 * action_height).max(0.0);
            action_gap = (remaining_after_buttons / gap_count).min(requested_gap);
            used_height = action_count as f32 * action_height + gap_count * action_gap;
        }

        if used_height > available_height && action_count > 0 {
            let remaining_after_gaps = (available_height - gap_count * action_gap).max(0.0);
            action_height = (remaining_after_gaps / action_count as f32).max(minimum_height);
            used_height = action_count as f32 * action_height + gap_count * action_gap;
        }

        if used_height > available_height && action_count > 0 {
            let remaining_after_gaps = (available_height - gap_count * action_gap).max(0.0);
            action_height = remaining_after_gaps / action_count as f32;
            used_height = action_count as f32 * action_height + gap_count * action_gap;
        }

        (
            action_height.max(0.0),
            action_gap.max(0.0),
            used_height.max(0.0),
        )
    }

    pub fn new(
        panel_bounds: Rect,
        metrics: OverlayStackMetrics,
        action_count: usize,
        title_height: f32,
        body_height: f32,
    ) -> Self {
        let content_rect = Rect {
            x: panel_bounds.x + metrics.content_padding.left,
            y: panel_bounds.y + metrics.content_padding.top,
            width: (panel_bounds.width
                - metrics.content_padding.left
                - metrics.content_padding.right)
                .max(0.0),
            height: (panel_bounds.height
                - metrics.content_padding.top
                - metrics.content_padding.bottom)
                .max(0.0),
        };

        let title_rect = Rect {
            x: content_rect.x,
            y: content_rect.y,
            width: content_rect.width,
            height: title_height.max(0.0),
        };
        let body_rect = Rect {
            x: content_rect.x,
            y: title_rect.y + title_rect.height + metrics.title_gap.max(0.0),
            width: content_rect.width,
            height: body_height.max(0.0),
        };

        let content_bottom = panel_bounds.y + panel_bounds.height - metrics.content_padding.bottom;
        let action_top = if action_count == 0 {
            body_rect.y + body_rect.height
        } else {
            let min_top = body_rect.y
                + body_rect.height
                + metrics.section_gap.max(0.0)
                + metrics.action_outsets.top.max(0.0);
            let usable_bottom = content_bottom - metrics.action_outsets.bottom.max(0.0);
            let available_actions_height = (usable_bottom - min_top).max(0.0);
            let (_, _, total_actions_height) =
                Self::fitted_action_stack(metrics, action_count, available_actions_height);
            min_top + ((available_actions_height - total_actions_height) * 0.5).max(0.0)
        };

        let action_bottom_limit = content_bottom - metrics.action_outsets.bottom.max(0.0);
        let available_actions_height = (action_bottom_limit - action_top).max(0.0);
        let (action_height, action_gap, _) =
            Self::fitted_action_stack(metrics, action_count, available_actions_height);

        let mut action_rects = Vec::with_capacity(action_count);
        let mut y = action_top;
        for _ in 0..action_count {
            action_rects.push(Rect {
                x: content_rect.x,
                y,
                width: content_rect.width,
                height: action_height,
            });
            y += action_height + action_gap;
        }

        Self {
            panel_bounds,
            content_rect,
            title_rect,
            body_rect,
            action_rects,
        }
    }

    pub fn required_panel_height(
        metrics: OverlayStackMetrics,
        action_count: usize,
        title_height: f32,
        body_height: f32,
    ) -> f32 {
        let actions_height = if action_count == 0 {
            0.0
        } else {
            action_count as f32 * metrics.action_height.max(0.0)
                + (action_count.saturating_sub(1)) as f32 * metrics.action_gap.max(0.0)
                + metrics.action_outsets.top.max(0.0)
                + metrics.action_outsets.bottom.max(0.0)
        };

        metrics.content_padding.top.max(0.0)
            + title_height.max(0.0)
            + metrics.title_gap.max(0.0)
            + body_height.max(0.0)
            + if action_count == 0 {
                0.0
            } else {
                metrics.section_gap.max(0.0)
            }
            + actions_height
            + metrics.content_padding.bottom.max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{OverlayStackLayout, OverlayStackMetrics};
    use crate::geometry::{Insets, Rect};

    #[test]
    fn overlay_stack_layout_places_actions_below_body_and_above_bottom_outset() {
        let metrics = OverlayStackMetrics {
            content_padding: Insets {
                left: 24.0,
                top: 32.0,
                right: 24.0,
                bottom: 40.0,
            },
            title_gap: 18.0,
            section_gap: 20.0,
            action_gap: 12.0,
            action_height: 40.0,
            action_outsets: Insets {
                left: 2.0,
                top: 9.0,
                right: 6.0,
                bottom: 6.0,
            },
        };
        let panel = Rect {
            x: 100.0,
            y: 50.0,
            width: 520.0,
            height: 448.0,
        };

        let layout = OverlayStackLayout::new(panel, metrics, 2, 64.0, 148.0);

        assert_eq!(layout.title_rect.y, 82.0);
        assert_eq!(layout.body_rect.y, 164.0);
        assert!(layout.action_rects[0].y >= layout.body_rect.y + layout.body_rect.height + 20.0);
        let last = layout.action_rects.last().unwrap();
        assert!(last.y + last.height <= panel.y + panel.height - 40.0 - 6.0);
    }

    #[test]
    fn overlay_stack_required_height_accounts_for_action_outsets() {
        let metrics = OverlayStackMetrics {
            content_padding: Insets {
                left: 24.0,
                top: 32.0,
                right: 24.0,
                bottom: 40.0,
            },
            title_gap: 18.0,
            section_gap: 20.0,
            action_gap: 12.0,
            action_height: 40.0,
            action_outsets: Insets {
                left: 2.0,
                top: 9.0,
                right: 6.0,
                bottom: 6.0,
            },
        };

        let required = OverlayStackLayout::required_panel_height(metrics, 2, 64.0, 148.0);
        assert_eq!(
            required,
            32.0 + 64.0 + 18.0 + 148.0 + 20.0 + 9.0 + 40.0 + 12.0 + 40.0 + 6.0 + 40.0
        );
    }

    #[test]
    fn overlay_stack_layout_shrinks_actions_to_fit_short_panel() {
        let metrics = OverlayStackMetrics {
            content_padding: Insets {
                left: 24.0,
                top: 24.0,
                right: 24.0,
                bottom: 24.0,
            },
            title_gap: 12.0,
            section_gap: 12.0,
            action_gap: 12.0,
            action_height: 56.0,
            action_outsets: Insets {
                left: 2.0,
                top: 9.0,
                right: 6.0,
                bottom: 6.0,
            },
        };
        let panel = Rect {
            x: 0.0,
            y: 0.0,
            width: 360.0,
            height: 240.0,
        };

        let layout = OverlayStackLayout::new(panel, metrics, 3, 40.0, 0.0);
        let last = layout.action_rects.last().unwrap();

        assert!(layout.action_rects[0].height < 56.0);
        assert!(last.y + last.height <= panel.y + panel.height - 24.0 - 6.0 + 0.001);
    }
}
