//! The one software rasterizer every host uses for `FrameCommand`s.
//!
//! Each host needs a CPU drawing path. It is the fallback when no GPU backend
//! is available (Linux, Windows and Android all have one), and it builds
//! small pre-rasterized textures. Until 2026-09-25 each host carried its own
//! copy. The Linux and Windows copies were near-identical; the Android one
//! differed in line width, image clipping and image sampling. None of them
//! could be compiled or tested except on its own platform. This module is
//! plain Rust with no platform code, so its behaviour is pinned by tests that
//! run everywhere.
//!
//! Semantics, chosen to match the GPU backends wherever they apply:
//!
//! - **Coverage:** a rectangle covers the pixels whose *centers* fall inside
//!   it, the rule GPU rasterizers use. Integer-aligned rects are exact.
//! - **Blending:** source-over with straight (non-premultiplied) alpha, and
//!   destination alpha is respected. Drawing into a transparent texture
//!   therefore yields correct alpha rather than colors pre-darkened against
//!   black. Onto an opaque framebuffer this is the ordinary
//!   `src * a + dst * (1 - a)`.
//! - **Lines** are exactly `thickness` pixels wide (a square brush stepped
//!   along a Bresenham path), not `thickness + 1` for even widths.
//! - **Images** honor `ImageRequest::clip_rect` and `alpha`, and sample from
//!   the image's full rect, so a partly off-screen image is cropped, not
//!   squashed.
//! - **Text** needs fonts, which stay host-specific, so
//!   [`RgbaCanvas::draw_command`] hands `Text` to a caller-supplied closure.
//!   That closure draws glyph coverage through [`RgbaCanvas::blend_pixel`].
//! - `PushClip`/`PopClip` are resolved before commands reach a backend (see
//!   `docs/CLIP_AND_SCISSOR.md`), so they are no-ops here.

use ui_core::geometry::{Color, Point, Rect};

use crate::{FrameCommand, ImageRequest, TextRequest};

/// A borrowed straight-alpha RGBA8 image, row-major with no padding.
#[derive(Debug, Clone, Copy)]
pub struct RgbaImage<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a [u8],
}

impl<'a> From<&'a loadngo_host_core::DecodedImage> for RgbaImage<'a> {
    fn from(image: &'a loadngo_host_core::DecodedImage) -> Self {
        Self {
            width: image.width as usize,
            height: image.height as usize,
            pixels: &image.rgba8,
        }
    }
}

/// An RGBA8 pixel buffer to draw into. `stride` is in pixels (a hardware
/// buffer's row pitch can exceed its width).
pub struct RgbaCanvas<'a> {
    bytes: &'a mut [u8],
    width: usize,
    height: usize,
    stride: usize,
}

impl<'a> RgbaCanvas<'a> {
    /// A tightly packed `width * height * 4`-byte buffer.
    pub fn new(bytes: &'a mut [u8], width: usize, height: usize) -> Self {
        Self::with_stride(bytes, width, height, width)
    }

    /// A buffer whose rows are `stride` pixels apart. Rows that would run
    /// past the end of `bytes` are treated as outside the canvas.
    pub fn with_stride(bytes: &'a mut [u8], width: usize, height: usize, stride: usize) -> Self {
        Self {
            bytes,
            width,
            height,
            stride: stride.max(width),
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return None;
        }
        let index = (y as usize * self.stride + x as usize) * 4;
        (index + 3 < self.bytes.len()).then_some(index)
    }

    /// Source-over blends `color`, scaled by `coverage` (`0.0..=1.0`, e.g.
    /// glyph coverage or an image's alpha), into one pixel. Out-of-bounds
    /// coordinates are ignored.
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: Color, coverage: f32) {
        let Some(index) = self.index(x, y) else {
            return;
        };
        let src_a = (color.a as f32 / 255.0) * coverage.clamp(0.0, 1.0);
        if src_a <= 0.0 {
            return;
        }
        let pixel = &mut self.bytes[index..index + 4];
        if src_a >= 1.0 {
            pixel.copy_from_slice(&[color.r, color.g, color.b, 255]);
            return;
        }
        let dst_a = pixel[3] as f32 / 255.0;
        let out_a = src_a + dst_a * (1.0 - src_a);
        let channel = |src: u8, dst: u8| {
            let value = (src as f32 * src_a + dst as f32 * dst_a * (1.0 - src_a)) / out_a;
            value.round().clamp(0.0, 255.0) as u8
        };
        pixel[0] = channel(color.r, pixel[0]);
        pixel[1] = channel(color.g, pixel[1]);
        pixel[2] = channel(color.b, pixel[2]);
        pixel[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    /// Sets every pixel to `color` exactly (no blending), like a GPU clear.
    pub fn clear(&mut self, color: Color) {
        let value = [color.r, color.g, color.b, color.a];
        for y in 0..self.height as i32 {
            for x in 0..self.width as i32 {
                if let Some(index) = self.index(x, y) {
                    self.bytes[index..index + 4].copy_from_slice(&value);
                }
            }
        }
    }

    /// Pixel bounds `(x0, y0, x1, y1)`, end-exclusive, of the pixels whose
    /// centers lie in `rect`, cropped to the canvas.
    fn covered_pixels(&self, rect: Rect) -> Option<(i32, i32, i32, i32)> {
        let first = |start: f32, limit: usize| (start - 0.5).ceil().clamp(0.0, limit as f32) as i32;
        let x0 = first(rect.x, self.width);
        let y0 = first(rect.y, self.height);
        let x1 = first(rect.x + rect.width, self.width);
        let y1 = first(rect.y + rect.height, self.height);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let Some((x0, y0, x1, y1)) = self.covered_pixels(rect) else {
            return;
        };
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend_pixel(x, y, color, 1.0);
            }
        }
    }

    /// An outline `thickness` pixels wide, inside `rect`.
    pub fn stroke_rect(&mut self, rect: Rect, color: Color, thickness: i32) {
        let t = thickness.max(1) as f32;
        if rect.width <= 2.0 * t || rect.height <= 2.0 * t {
            // No hollow middle left: the outline is the whole rect.
            self.fill_rect(rect, color);
            return;
        }
        let edges = [
            Rect { height: t, ..rect },
            Rect {
                y: rect.y + rect.height - t,
                height: t,
                ..rect
            },
            Rect {
                y: rect.y + t,
                width: t,
                height: rect.height - 2.0 * t,
                ..rect
            },
            Rect {
                x: rect.x + rect.width - t,
                y: rect.y + t,
                width: t,
                height: rect.height - 2.0 * t,
            },
        ];
        // Edges don't overlap, so a translucent outline blends evenly.
        for edge in edges {
            self.fill_rect(edge, color);
        }
    }

    /// A `thickness`-pixel square brush stepped along the Bresenham path from
    /// `from` to `to`. Brush positions overlap along the path, so translucent
    /// lines darken where steps overlap, as in every earlier host copy.
    pub fn line(&mut self, from: Point, to: Point, color: Color, thickness: i32) {
        let thickness = thickness.max(1);
        let back = thickness / 2;
        let (mut x, mut y) = (from.x.round() as i32, from.y.round() as i32);
        let (x1, y1) = (to.x.round() as i32, to.y.round() as i32);
        let dx = (x1 - x).abs();
        let dy = -(y1 - y).abs();
        let sx = if x < x1 { 1 } else { -1 };
        let sy = if y < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            for by in 0..thickness {
                for bx in 0..thickness {
                    self.blend_pixel(x - back + bx, y - back + by, color, 1.0);
                }
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    pub fn polyline(&mut self, points: &[Point], color: Color, thickness: i32, closed: bool) {
        for segment in points.windows(2) {
            self.line(segment[0], segment[1], color, thickness);
        }
        if closed && points.len() > 2 {
            self.line(points[points.len() - 1], points[0], color, thickness);
        }
    }

    /// Filled disc: pixels within `radius` of the rounded center.
    pub fn fill_circle(&mut self, center: Point, radius: f32, color: Color) {
        if radius <= 0.0 || !radius.is_finite() {
            return;
        }
        let reach = radius.round() as i32;
        let (cx, cy) = (center.x.round() as i32, center.y.round() as i32);
        let r2 = radius * radius;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                if (dx * dx + dy * dy) as f32 <= r2 {
                    self.blend_pixel(cx + dx, cy + dy, color, 1.0);
                }
            }
        }
    }

    /// Nearest-neighbour scales `image` onto `rect`, cropped to `clip` and
    /// faded by `alpha`.
    pub fn blit_image(&mut self, image: RgbaImage<'_>, rect: Rect, clip: Option<Rect>, alpha: f32) {
        if image.width == 0
            || image.height == 0
            || image.pixels.len() < image.width * image.height * 4
            || rect.width <= 0.0
            || rect.height <= 0.0
        {
            return;
        }
        let visible = match clip {
            Some(clip) => match intersect_rects(rect, clip) {
                Some(visible) => visible,
                None => return,
            },
            None => rect,
        };
        let Some((x0, y0, x1, y1)) = self.covered_pixels(visible) else {
            return;
        };
        let sample = |pixel: i32, start: f32, extent: f32, size: usize| {
            ((((pixel as f32 + 0.5 - start) / extent) * size as f32).floor() as usize).min(size - 1)
        };
        for y in y0..y1 {
            let sy = sample(y, rect.y, rect.height, image.height);
            for x in x0..x1 {
                let sx = sample(x, rect.x, rect.width, image.width);
                let i = (sy * image.width + sx) * 4;
                let p = &image.pixels[i..i + 4];
                self.blend_pixel(x, y, Color::rgba(p[0], p[1], p[2], p[3]), alpha);
            }
        }
    }

    /// Draws one frame command. `image` resolves an image key; `text` draws a
    /// text request with the host's fonts.
    pub fn draw_command<'img>(
        &mut self,
        command: &FrameCommand,
        image: impl Fn(&str) -> Option<RgbaImage<'img>>,
        text: impl FnOnce(&mut Self, &TextRequest),
    ) {
        match command {
            FrameCommand::Clear { color } => self.clear(*color),
            FrameCommand::FillRect { rect, color } => self.fill_rect(*rect, *color),
            FrameCommand::StrokeRect {
                rect,
                color,
                thickness,
            } => self.stroke_rect(*rect, *color, *thickness),
            FrameCommand::Line {
                from,
                to,
                color,
                thickness,
            } => self.line(*from, *to, *color, *thickness),
            FrameCommand::Circle {
                center,
                radius,
                color,
            } => self.fill_circle(*center, radius.max(1.0), *color),
            FrameCommand::Polyline {
                points,
                color,
                thickness,
                closed,
            } => self.polyline(points, *color, *thickness, *closed),
            FrameCommand::Arc {
                center,
                radius,
                start_angle,
                sweep_angle,
                color,
                thickness,
            } => {
                let points = arc_points(*center, *radius, *start_angle, *sweep_angle);
                self.polyline(&points, *color, *thickness, false);
            }
            FrameCommand::ParticleBatch { particles } => {
                for particle in particles {
                    self.fill_circle(particle.center, particle.radius.max(1.0), particle.color);
                }
            }
            FrameCommand::Image(request) => self.draw_image_request(request, image),
            FrameCommand::Text(request) => text(self, request),
            FrameCommand::PushClip { .. } | FrameCommand::PopClip => {}
        }
    }

    fn draw_image_request<'img>(
        &mut self,
        request: &ImageRequest,
        image: impl Fn(&str) -> Option<RgbaImage<'img>>,
    ) {
        if let Some(pixels) = image(&request.image_key) {
            self.blit_image(pixels, request.rect, request.clip_rect, request.alpha);
        }
    }
}

/// An owned RGBA8 image holding one primitive drawn on a transparent
/// background, and where it goes on the surface.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterizedPrimitive {
    pub rect: Rect,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// Draws a line into its own transparent image, sized to its bounds plus the
/// brush, for hosts that pre-rasterize primitives into textures.
pub fn rasterize_line(
    from: Point,
    to: Point,
    color: Color,
    thickness: i32,
) -> Option<RasterizedPrimitive> {
    let pad = thickness.max(1) as f32;
    let x = from.x.min(to.x).round() - pad;
    let y = from.y.min(to.y).round() - pad;
    let width = ((from.x - to.x).abs().round() + 2.0 * pad + 1.0) as usize;
    let height = ((from.y - to.y).abs().round() + 2.0 * pad + 1.0) as usize;
    let mut pixels = vec![0; width * height * 4];
    let local = |p: Point| Point {
        x: p.x - x,
        y: p.y - y,
    };
    RgbaCanvas::new(&mut pixels, width, height).line(local(from), local(to), color, thickness);
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[3] != 0)
        .then_some(RasterizedPrimitive {
            rect: Rect {
                x,
                y,
                width: width as f32,
                height: height as f32,
            },
            width,
            height,
            pixels,
        })
}

/// Polyline approximation of an arc, shared by every software path.
pub fn arc_points(center: Point, radius: f32, start_angle: f32, sweep_angle: f32) -> Vec<Point> {
    if radius <= 0.0 || sweep_angle.abs() <= f32::EPSILON {
        return Vec::new();
    }
    let segments = ((radius.abs() * sweep_angle.abs()) / 10.0)
        .ceil()
        .clamp(8.0, 96.0) as usize;
    (0..=segments)
        .map(|index| {
            let angle = start_angle + sweep_angle * (index as f32 / segments as f32);
            Point {
                x: center.x + radius * angle.cos(),
                y: center.y + radius * angle.sin(),
            }
        })
        .collect()
}

/// The overlap of two rects, if they overlap.
pub fn intersect_rects(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then_some(Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::rgba(255, 0, 0, 255);
    const BLACK: Color = Color::rgba(0, 0, 0, 255);

    struct Surface {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    }

    impl Surface {
        fn new(width: usize, height: usize) -> Self {
            Self {
                width,
                height,
                bytes: vec![0; width * height * 4],
            }
        }

        fn canvas(&mut self) -> RgbaCanvas<'_> {
            RgbaCanvas::new(&mut self.bytes, self.width, self.height)
        }

        fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
            let i = (y * self.width + x) * 4;
            self.bytes[i..i + 4].try_into().unwrap()
        }

        fn painted(&self) -> usize {
            self.bytes
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] != 0)
                .count()
        }

        /// Row-major map of painted pixels, for readable failures.
        fn map(&self) -> String {
            (0..self.height)
                .map(|y| {
                    (0..self.width)
                        .map(|x| if self.pixel(x, y)[3] != 0 { '#' } else { '.' })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn pt(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    #[test]
    fn integer_rect_covers_exactly_its_pixels() {
        let mut s = Surface::new(6, 6);
        s.canvas().fill_rect(rect(1.0, 2.0, 3.0, 2.0), RED);
        assert_eq!(s.painted(), 6, "\n{}", s.map());
        assert_eq!(s.pixel(1, 2), [255, 0, 0, 255]);
        assert_eq!(s.pixel(3, 3), [255, 0, 0, 255]);
        assert_eq!(s.pixel(4, 3)[3], 0);
    }

    #[test]
    fn fractional_rect_uses_the_pixel_center_rule() {
        let mut s = Surface::new(6, 1);
        // Centers 1.5 and 2.5 lie inside [1.4, 2.6); 0.5 and 3.5 don't.
        s.canvas().fill_rect(rect(1.4, 0.0, 1.2, 1.0), RED);
        assert_eq!(s.map(), ".##...");
    }

    #[test]
    fn shapes_off_the_canvas_are_cropped_without_panicking() {
        let mut s = Surface::new(4, 4);
        let mut canvas = s.canvas();
        canvas.fill_rect(rect(-10.0, -10.0, 12.0, 12.0), RED);
        canvas.line(pt(-5.0, 1.0), pt(50.0, 1.0), RED, 3);
        canvas.fill_circle(pt(100.0, 100.0), 5.0, RED);
        canvas.stroke_rect(rect(2.0, 2.0, 40.0, 40.0), RED, 2);
        let image = [255u8; 4];
        let tiny = RgbaImage {
            width: 1,
            height: 1,
            pixels: &image,
        };
        canvas.blit_image(tiny, rect(10.0, 10.0, 4.0, 4.0), None, 1.0);
        canvas.blit_image(tiny, rect(-8.0, -8.0, 4.0, 4.0), None, 1.0);
    }

    #[test]
    fn line_is_exactly_its_thickness_wide() {
        for thickness in 1..=4 {
            let mut s = Surface::new(12, 12);
            s.canvas().line(pt(2.0, 6.0), pt(9.0, 6.0), RED, thickness);
            let column: usize = (0..12).filter(|&y| s.pixel(5, y)[3] != 0).count();
            assert_eq!(column, thickness as usize, "\n{}", s.map());
        }
    }

    #[test]
    fn closed_polyline_joins_its_last_point_to_its_first() {
        let mut open = Surface::new(8, 8);
        let mut closed = Surface::new(8, 8);
        let points = [pt(1.0, 1.0), pt(6.0, 1.0), pt(6.0, 6.0)];
        open.canvas().polyline(&points, RED, 1, false);
        closed.canvas().polyline(&points, RED, 1, true);
        assert_eq!(open.pixel(3, 3)[3], 0);
        assert_ne!(closed.pixel(3, 3)[3], 0, "\n{}", closed.map());
    }

    #[test]
    fn circle_is_symmetric_and_bounded_by_its_radius() {
        let mut s = Surface::new(11, 11);
        s.canvas().fill_circle(pt(5.0, 5.0), 3.0, RED);
        for y in 0..11 {
            for x in 0..11 {
                let (dx, dy) = (x as i32 - 5, y as i32 - 5);
                let inside = dx * dx + dy * dy <= 9;
                assert_eq!(s.pixel(x, y)[3] != 0, inside, "({x},{y})\n{}", s.map());
            }
        }
    }

    #[test]
    fn stroke_rect_outline_does_not_double_blend_corners() {
        let mut s = Surface::new(8, 8);
        s.canvas().clear(BLACK);
        s.canvas()
            .stroke_rect(rect(1.0, 1.0, 6.0, 6.0), Color::rgba(255, 0, 0, 128), 1);
        assert_eq!(s.pixel(1, 1), s.pixel(3, 1), "corner vs top edge");
        assert_eq!(s.pixel(1, 1), s.pixel(1, 3), "corner vs left edge");
        assert_eq!(s.pixel(3, 3), [0, 0, 0, 255], "interior untouched");
    }

    #[test]
    fn half_alpha_over_opaque_black_is_half_red() {
        let mut s = Surface::new(1, 1);
        s.canvas().clear(BLACK);
        s.canvas()
            .blend_pixel(0, 0, Color::rgba(255, 0, 0, 128), 1.0);
        assert_eq!(s.pixel(0, 0), [128, 0, 0, 255]);
    }

    #[test]
    fn translucent_color_on_a_transparent_texture_keeps_its_color() {
        // The old Android texture path forced alpha to 255 here, baking a
        // dark fringe into every translucent edge.
        let mut s = Surface::new(1, 1);
        s.canvas()
            .blend_pixel(0, 0, Color::rgba(200, 100, 50, 128), 1.0);
        assert_eq!(s.pixel(0, 0), [200, 100, 50, 128]);
    }

    #[test]
    fn image_blit_honors_clip_and_samples_from_the_full_rect() {
        // 2x1 image: left red, right blue, stretched over 4x1 pixels.
        let pixels = [255, 0, 0, 255, 0, 0, 255, 255];
        let image = RgbaImage {
            width: 2,
            height: 1,
            pixels: &pixels,
        };
        let mut s = Surface::new(4, 1);
        s.canvas().blit_image(
            image,
            rect(0.0, 0.0, 4.0, 1.0),
            Some(rect(2.0, 0.0, 2.0, 1.0)),
            1.0,
        );
        assert_eq!(s.pixel(1, 0)[3], 0, "clipped away");
        assert_eq!(s.pixel(2, 0), [0, 0, 255, 255]);

        // Partly off the left edge: the visible half is the image's right
        // half, not the whole image squashed into it.
        let mut s = Surface::new(4, 1);
        s.canvas()
            .blit_image(image, rect(-2.0, 0.0, 4.0, 1.0), None, 1.0);
        assert_eq!(s.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(s.pixel(1, 0), [0, 0, 255, 255]);
    }

    #[test]
    fn stride_leaves_row_padding_untouched() {
        let mut bytes = vec![0u8; 3 * 2 * 4];
        RgbaCanvas::with_stride(&mut bytes, 2, 2, 3).clear(RED);
        let padding: Vec<u8> = [&bytes[8..12], &bytes[20..24]].concat();
        assert!(padding.iter().all(|&b| b == 0));
        assert_eq!(&bytes[12..16], &[255, 0, 0, 255]);
    }

    #[test]
    fn draw_command_routes_text_to_the_host_and_images_by_key() {
        let pixels = [0, 255, 0, 255];
        let mut s = Surface::new(4, 4);
        let mut text_calls = 0;
        let image_command = FrameCommand::Image(ImageRequest {
            rect: rect(0.0, 0.0, 2.0, 2.0),
            clip_rect: None,
            image_key: "green".to_string(),
            alpha: 1.0,
        });
        let lookup = |key: &str| {
            (key == "green").then_some(RgbaImage {
                width: 1,
                height: 1,
                pixels: &pixels,
            })
        };
        s.canvas().draw_command(&image_command, lookup, |_, _| {
            text_calls += 1;
        });
        assert_eq!(s.pixel(1, 1), [0, 255, 0, 255]);
        assert_eq!(text_calls, 0);
    }

    #[test]
    fn rasterized_line_lands_where_the_line_is() {
        let raster = rasterize_line(pt(10.0, 20.0), pt(14.0, 20.0), RED, 1).expect("visible");
        let ink: Vec<(f32, f32)> = raster
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(_, p)| p[3] != 0)
            .map(|(i, _)| {
                (
                    raster.rect.x + (i % raster.width) as f32,
                    raster.rect.y + (i / raster.width) as f32,
                )
            })
            .collect();
        assert_eq!(ink.len(), 5);
        assert!(ink
            .iter()
            .all(|&(x, y)| (10.0..=14.0).contains(&x) && y == 20.0));
    }

    #[test]
    fn arc_points_span_the_sweep() {
        let points = arc_points(pt(0.0, 0.0), 10.0, 0.0, std::f32::consts::FRAC_PI_2);
        let first = points.first().unwrap();
        let last = points.last().unwrap();
        assert!((first.x - 10.0).abs() < 1e-4 && first.y.abs() < 1e-4);
        assert!(last.x.abs() < 1e-4 && (last.y - 10.0).abs() < 1e-4);
        assert!(arc_points(pt(0.0, 0.0), 0.0, 0.0, 1.0).is_empty());
    }
}
