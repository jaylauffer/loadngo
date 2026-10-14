//! The editor drawn without a window, by loadngo's software rasterizer
//! (`loadngo_renderer::software`), for `--render` and tests. Paint ops go
//! through the renderer's encoder, as on screen; the frame being edited is
//! the one image they draw.
//!
//! Text is left out: fonts belong to each host's window code, and no window
//! is open here. Everything else (panels, the picture, the grid, the hover
//! frame, the swatches) is drawn as on screen.

use loadngo_host_core::DecodedImage;
use loadngo_renderer::software::{RgbaCanvas, RgbaImage};
use loadngo_renderer::{Renderer, RendererConfig};
use ui_core::PaintOp;

use crate::editor::{Editor, IMAGE_KEY};

/// An approximate text width (no fonts here): wide enough that layouts
/// measured with it leave room for the real text.
pub fn approximate_width(text: &str, size: u16) -> f32 {
    text.chars().count() as f32 * f32::from(size) * 0.6
}

/// The editor at `width` x `height`, as straight-alpha RGBA.
pub fn render(editor: &Editor, width: u32, height: u32) -> DecodedImage {
    let mut scene: Vec<PaintOp> = Vec::new();
    editor.paint(&mut scene);
    let commands = Renderer::new(RendererConfig::default()).encode_paint_ops(&scene);
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    {
        let mut canvas = RgbaCanvas::new(&mut pixels, width as usize, height as usize);
        let image = editor.image().map(|(image, _)| image);
        for command in &commands {
            canvas.draw_command(
                command,
                |key| image.filter(|_| key == IMAGE_KEY).map(RgbaImage::from),
                |_, _| {},
            );
        }
    }
    DecodedImage::new(width, height, pixels)
}

#[cfg(test)]
mod tests {
    use super::{approximate_width, render};
    use crate::editor::{Editor, CELL_HEIGHT, CELL_WIDTH};
    use crate::picture::{self, Opened};
    use loadngo_host_core::{DecodedImage, InputSnapshot};

    /// A 40x30 picture: pixel (x, y) is (x*6, y*8, 90, alpha), alpha 255 in
    /// the left half and 128 in the right.
    fn opened() -> Opened {
        let (width, height) = (40u32, 30u32);
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let alpha = if x < 20 { 255 } else { 128 };
                rgba.extend_from_slice(&[(x * 6) as u8, (y * 8) as u8, 90, alpha]);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pattern.png");
        let image = DecodedImage::new(width, height, rgba);
        std::fs::write(
            &path,
            picture::encode(&image, &picture::Format::Png).unwrap(),
        )
        .unwrap();
        let opened = picture::open(&path, 0).unwrap();
        assert_eq!(opened.image.rgba8, image.rgba8);
        opened
    }

    fn laid_out(width: f32, height: f32) -> Editor {
        let mut editor = Editor::new(true);
        editor.show(opened());
        let _ = editor.frame(
            &InputSnapshot::default(),
            (width, height),
            0.0,
            &approximate_width,
        );
        editor
    }

    fn at(image: &DecodedImage, x: f32, y: f32) -> [u8; 4] {
        picture::pixel(image, x as u32, y as u32).unwrap()
    }

    #[test]
    fn the_rendered_editor_shows_the_hovered_pixel_in_its_cell() {
        let mut editor = laid_out(1000.0, 700.0);
        editor.hover_pixel(10, 12);
        let _ = editor.frame(
            &InputSnapshot::default(),
            (1000.0, 700.0),
            0.0,
            &approximate_width,
        );
        let cell = editor
            .hover_cell()
            .expect("the hovered pixel's cell is in the grid");
        assert_eq!(
            (cell.width, cell.height),
            (CELL_WIDTH as f32, CELL_HEIGHT as f32)
        );
        let scene = render(&editor, 1000, 700);

        // The middle of the cell is the pixel, exactly (it is opaque).
        let middle = at(
            &scene,
            cell.x + cell.width / 2.0,
            cell.y + cell.height / 2.0,
        );
        assert_eq!(middle, [60, 96, 90, 255]);
        // Its edges carry the hover frame, white on this dark pixel...
        assert_eq!(at(&scene, cell.x, cell.y + 5.0), [255, 255, 255, 255]);
        assert_eq!(
            at(&scene, cell.x + cell.width - 1.0, cell.y + 5.0),
            [255, 255, 255, 255]
        );
        assert_eq!(at(&scene, cell.x + 5.0, cell.y), [255, 255, 255, 255]);
        // ...and the next cell along shows pixel (11, 12), unframed.
        let next = at(&scene, cell.x + cell.width + 8.0, cell.y + 6.0);
        assert_eq!(next, [66, 96, 90, 255]);
        let next_edge = at(&scene, cell.x + cell.width, cell.y + 6.0);
        assert_eq!(next_edge, [66, 96, 90, 255]);
    }

    #[test]
    fn a_translucent_pixel_is_blended_over_the_dark_backdrop_as_the_c_plus_plus_did() {
        let mut editor = laid_out(1000.0, 700.0);
        editor.hover_pixel(30, 5);
        let _ = editor.frame(
            &InputSnapshot::default(),
            (1000.0, 700.0),
            0.0,
            &approximate_width,
        );
        let cell = editor.hover_cell().unwrap();
        let scene = render(&editor, 1000, 700);
        let middle = at(&scene, cell.x + 8.0, cell.y + 6.0);
        // (180, 40, 90) at alpha 128 over the backdrop (3, 5, 15).
        let blend = |c: u32, under: u32| ((c * 128 + under * 127 + 127) / 255) as u8;
        assert_eq!(middle, [blend(180, 3), blend(40, 5), blend(90, 15), 255]);
    }

    #[test]
    fn the_picture_panel_draws_the_frame_at_a_whole_scale() {
        let editor = laid_out(1000.0, 700.0);
        let scene = render(&editor, 1000, 700);
        // Find the picture by its first pixel's colour, (0, 0, 90).
        let mut found = None;
        'search: for y in 0..700 {
            for x in 0..400 {
                if picture::pixel(&scene, x, y) == Some([0, 0, 90, 255]) {
                    found = Some((x, y));
                    break 'search;
                }
            }
        }
        let (x0, y0) = found.expect("the picture is drawn");
        // Scaled up by a whole factor, so each pixel is a block of one colour.
        let mut scale = 0;
        while picture::pixel(&scene, x0 + scale, y0) == Some([0, 0, 90, 255]) {
            scale += 1;
        }
        assert!(scale >= 2, "scale {scale}");
        assert_eq!(
            picture::pixel(&scene, x0 + scale, y0),
            Some([6, 0, 90, 255])
        );
    }
}
