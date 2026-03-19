pub fn single_line_text_box_height(font_size: u16) -> f32 {
    ((font_size as f32) * 1.6)
        .ceil()
        .max(font_size as f32 + 8.0)
}

pub fn multiline_line_step(font_size: u16) -> f32 {
    (single_line_text_box_height(font_size) - 4.0).max(font_size as f32 + 6.0)
}

#[cfg(test)]
mod tests {
    use super::{multiline_line_step, single_line_text_box_height};

    #[test]
    fn single_line_text_box_height_scales_above_font_size() {
        assert!(single_line_text_box_height(17) >= 27.0);
        assert!(single_line_text_box_height(26) > single_line_text_box_height(17));
    }

    #[test]
    fn multiline_step_stays_below_full_box_height() {
        assert!(multiline_line_step(17) < single_line_text_box_height(17));
        assert!(multiline_line_step(24) < single_line_text_box_height(24));
    }
}
