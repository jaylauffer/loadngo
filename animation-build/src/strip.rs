//! Frame strips as PNG: side-on panels of a figure's pose and top-down paths with
//! footprints, drawn as anti-aliased capsules, labelled with a small built-in font.

use image::{Rgb, RgbImage};
use loadngo_animation::humanoid::base_name;
use loadngo_animation::math::Vec3;

pub type Color = [u8; 3];
pub const WHITE: Color = [255, 255, 255];
pub const INK: Color = [30, 30, 34];
pub const GREY: Color = [150, 150, 156];
pub const LIGHT: Color = [222, 222, 226];
pub const NEAR: Color = [214, 110, 26];
pub const FAR: Color = [52, 102, 178];
pub const TORSO: Color = [70, 70, 78];
pub const GREEN: Color = [60, 150, 90];

/// An image to draw on.
pub struct Canvas {
    pub img: RgbImage,
}

fn blend(dst: &mut Rgb<u8>, c: Color, a: f32) {
    for (d, c) in dst.0.iter_mut().zip(c) {
        let v = f32::from(*d);
        *d = (v + (f32::from(c) - v) * a).round().clamp(0.0, 255.0) as u8;
    }
}

const GLYPHS: &[(char, [u8; 7])] = &[
    ('0', [14, 17, 19, 21, 25, 17, 14]),
    ('1', [4, 12, 4, 4, 4, 4, 14]),
    ('2', [14, 17, 1, 2, 4, 8, 31]),
    ('3', [31, 2, 4, 2, 1, 17, 14]),
    ('4', [2, 6, 10, 18, 31, 2, 2]),
    ('5', [31, 16, 30, 1, 1, 17, 14]),
    ('6', [6, 8, 16, 30, 17, 17, 14]),
    ('7', [31, 1, 2, 4, 8, 8, 8]),
    ('8', [14, 17, 17, 14, 17, 17, 14]),
    ('9', [14, 17, 17, 15, 1, 2, 12]),
    ('a', [14, 17, 17, 31, 17, 17, 17]),
    ('b', [30, 17, 17, 30, 17, 17, 30]),
    ('c', [14, 17, 16, 16, 16, 17, 14]),
    ('d', [28, 18, 17, 17, 17, 18, 28]),
    ('e', [31, 16, 16, 30, 16, 16, 31]),
    ('f', [31, 16, 16, 30, 16, 16, 16]),
    ('g', [14, 17, 16, 23, 17, 17, 15]),
    ('h', [17, 17, 17, 31, 17, 17, 17]),
    ('i', [14, 4, 4, 4, 4, 4, 14]),
    ('j', [7, 2, 2, 2, 2, 18, 12]),
    ('k', [17, 18, 20, 24, 20, 18, 17]),
    ('l', [16, 16, 16, 16, 16, 16, 31]),
    ('m', [17, 27, 21, 21, 17, 17, 17]),
    ('n', [17, 17, 25, 21, 19, 17, 17]),
    ('o', [14, 17, 17, 17, 17, 17, 14]),
    ('p', [30, 17, 17, 30, 16, 16, 16]),
    ('q', [14, 17, 17, 17, 21, 18, 13]),
    ('r', [30, 17, 17, 30, 20, 18, 17]),
    ('s', [15, 16, 16, 14, 1, 1, 30]),
    ('t', [31, 4, 4, 4, 4, 4, 4]),
    ('u', [17, 17, 17, 17, 17, 17, 14]),
    ('v', [17, 17, 17, 17, 17, 10, 4]),
    ('w', [17, 17, 17, 21, 21, 21, 10]),
    ('x', [17, 17, 10, 4, 10, 17, 17]),
    ('y', [17, 17, 17, 10, 4, 4, 4]),
    ('z', [31, 1, 2, 4, 8, 16, 31]),
    ('.', [0, 0, 0, 0, 0, 12, 12]),
    (':', [0, 12, 12, 0, 12, 12, 0]),
    ('-', [0, 0, 0, 31, 0, 0, 0]),
    ('/', [0, 1, 2, 4, 8, 16, 0]),
    ('%', [24, 25, 2, 4, 8, 19, 3]),
    ('=', [0, 0, 31, 0, 31, 0, 0]),
    ('(', [2, 4, 8, 8, 8, 4, 2]),
    (')', [8, 4, 2, 2, 2, 4, 8]),
    (',', [0, 0, 0, 0, 12, 4, 8]),
    ('+', [0, 4, 4, 31, 4, 4, 0]),
    ('_', [0, 0, 0, 0, 0, 0, 31]),
    ('x', [17, 17, 10, 4, 10, 17, 17]),
];

impl Canvas {
    #[must_use]
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            img: RgbImage::from_pixel(w, h, Rgb(WHITE)),
        }
    }

    /// A capsule from `a` to `b` of radius `r` (pixels).
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), r: f32, c: Color) {
        let (w, h) = (self.img.width() as f32, self.img.height() as f32);
        let x0 = (a.0.min(b.0) - r - 1.0).floor().max(0.0) as u32;
        let x1 = (a.0.max(b.0) + r + 1.0).ceil().min(w - 1.0).max(0.0) as u32;
        let y0 = (a.1.min(b.1) - r - 1.0).floor().max(0.0) as u32;
        let y1 = (a.1.max(b.1) + r + 1.0).ceil().min(h - 1.0).max(0.0) as u32;
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = if len2 > 0.0 {
                    (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (qx, qy) = (a.0 + dx * t - px, a.1 + dy * t - py);
                let d = (qx * qx + qy * qy).sqrt();
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    blend(self.img.get_pixel_mut(x, y), c, cov);
                }
            }
        }
    }

    pub fn dot(&mut self, p: (f32, f32), r: f32, c: Color) {
        self.line(p, p, r, c);
    }

    /// Text in the built-in 5×7 font at `scale` pixels a dot; unknown characters are blank.
    pub fn text(&mut self, x: f32, y: f32, s: &str, scale: u32, c: Color) {
        let mut cx = x as i64;
        for ch in s.chars() {
            let lower = ch.to_ascii_lowercase();
            if let Some((_, rows)) = GLYPHS.iter().find(|(g, _)| *g == lower) {
                for (ry, row) in rows.iter().enumerate() {
                    for bx in 0..5 {
                        if row & (1 << (4 - bx)) != 0 {
                            for sy in 0..scale {
                                for sx in 0..scale {
                                    let px = cx + i64::from(bx * scale + sx);
                                    let py =
                                        y as i64 + (ry as i64) * i64::from(scale) + i64::from(sy);
                                    if px >= 0
                                        && py >= 0
                                        && (px as u32) < self.img.width()
                                        && (py as u32) < self.img.height()
                                    {
                                        self.img.put_pixel(px as u32, py as u32, Rgb(c));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            cx += i64::from(6 * scale);
        }
    }

    /// Saves as PNG.
    ///
    /// # Errors
    /// Encoding or writing.
    pub fn png(&self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(
                self.img.as_raw(),
                self.img.width(),
                self.img.height(),
                image::ColorType::Rgb8,
            )
            .map_err(|e| e.to_string())?;
        Ok(out)
    }
}

use image::ImageEncoder;

/// Which side each bone is on, by its Mixamo name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Centre,
}

#[must_use]
pub fn sides(names: &[String]) -> Vec<Side> {
    names
        .iter()
        .map(|n| {
            let b = base_name(n);
            if b.starts_with("Left") {
                Side::Left
            } else if b.starts_with("Right") {
                Side::Right
            } else {
                Side::Centre
            }
        })
        .collect()
}

/// One pose to draw side-on.
pub struct PanelPose<'a> {
    pub positions: &'a [Vec3],
    pub root: Vec3,
    pub facing: f32,
    pub label: String,
    /// Feet low enough to count as on the floor (left, right).
    pub on_floor: [bool; 2],
}

/// Draws poses side-on from the figure's right, each in its own panel centred on its root,
/// in a row starting at `(x0, y0)`. Returns the row's height.
#[allow(clippy::too_many_arguments)]
pub fn side_row(
    canvas: &mut Canvas,
    x0: f32,
    y0: f32,
    panel_w: f32,
    panel_h: f32,
    px_per_m: f32,
    parents: &[Option<usize>],
    side: &[Side],
    feet: [usize; 2],
    poses: &[PanelPose<'_>],
) -> f32 {
    let ground = y0 + panel_h - 22.0;
    for (i, p) in poses.iter().enumerate() {
        let cx = x0 + panel_w * (i as f32 + 0.5);
        let (s, c) = p.facing.sin_cos();
        let fwd = Vec3::new(s, 0.0, c);
        let right = Vec3::new(-c, 0.0, s);
        let project = |q: Vec3| -> (f32, f32) {
            let d = q - p.root;
            (cx + d.dot(fwd) * px_per_m, ground - q.y * px_per_m)
        };
        canvas.line(
            (cx - panel_w * 0.45, ground + 1.0),
            (cx + panel_w * 0.45, ground + 1.0),
            0.8,
            GREY,
        );
        // Far side, then centre, then near side; deeper first within each.
        for pass in [Side::Left, Side::Centre, Side::Right] {
            for (b, par) in parents.iter().enumerate() {
                let Some(pa) = par else { continue };
                if side[b] != pass {
                    continue;
                }
                let (col, r) = match pass {
                    Side::Left => (FAR, 3.0),
                    Side::Right => (NEAR, 3.2),
                    Side::Centre => (TORSO, 4.0),
                };
                let _ = right;
                canvas.line(project(p.positions[*pa]), project(p.positions[b]), r, col);
            }
        }
        // Head.
        if let Some(h) = parents
            .iter()
            .enumerate()
            .rev()
            .find(|(b, _)| side[*b] == Side::Centre)
            .map(|(b, _)| b)
        {
            canvas.dot(project(p.positions[h]), 6.0, TORSO);
        }
        for (k, &f) in feet.iter().enumerate() {
            if p.on_floor[k] {
                let (x, _) = project(p.positions[f]);
                canvas.line(
                    (x - 5.0, ground + 4.0),
                    (x + 5.0, ground + 4.0),
                    1.5,
                    if k == 0 { FAR } else { NEAR },
                );
            }
        }
        canvas.text(cx - panel_w * 0.45, ground + 9.0, &p.label, 1, INK);
    }
    panel_h
}

/// A top-down map: the planned path, the simulated root, the character's root and its
/// footprints (ankle to toe at each touch-down).
pub struct TopView<'a> {
    pub planned: &'a [Vec3],
    pub simulation: &'a [Vec3],
    pub root: &'a [Vec3],
    /// `(left?, ankle, toe)` at each touch-down.
    pub prints: &'a [(bool, Vec3, Vec3)],
}

/// Draws a [`TopView`] into the rectangle at `(x0, y0)` of `w`×`h` pixels, fitted, seen
/// from above: +Z up the image and so +X to the left (a figure walking up the image has its
/// left on the left).
pub fn top_view(canvas: &mut Canvas, x0: f32, y0: f32, w: f32, h: f32, v: &TopView<'_>) {
    let all = v
        .planned
        .iter()
        .chain(v.root)
        .chain(v.prints.iter().map(|p| &p.1));
    let (mut lo, mut hi) = (
        Vec3::new(f32::INFINITY, 0.0, f32::INFINITY),
        Vec3::new(f32::NEG_INFINITY, 0.0, f32::NEG_INFINITY),
    );
    for p in all {
        lo.x = lo.x.min(p.x);
        lo.z = lo.z.min(p.z);
        hi.x = hi.x.max(p.x);
        hi.z = hi.z.max(p.z);
    }
    let pad = 0.6;
    let span_x = (hi.x - lo.x + 2.0 * pad).max(1.0);
    let span_z = (hi.z - lo.z + 2.0 * pad).max(1.0);
    let s = (w / span_x).min(h / span_z);
    let cx = x0 + w * 0.5;
    let cy = y0 + h * 0.5;
    let mid = Vec3::new((lo.x + hi.x) * 0.5, 0.0, (lo.z + hi.z) * 0.5);
    let map = |p: Vec3| (cx - (p.x - mid.x) * s, cy - (p.z - mid.z) * s);
    // Grid every metre.
    let gx0 = (lo.x - pad).floor() as i32;
    let gx1 = (hi.x + pad).ceil() as i32;
    let gz0 = (lo.z - pad).floor() as i32;
    let gz1 = (hi.z + pad).ceil() as i32;
    for gx in gx0..=gx1 {
        canvas.line(
            map(Vec3::new(gx as f32, 0.0, gz0 as f32)),
            map(Vec3::new(gx as f32, 0.0, gz1 as f32)),
            0.4,
            LIGHT,
        );
    }
    for gz in gz0..=gz1 {
        canvas.line(
            map(Vec3::new(gx0 as f32, 0.0, gz as f32)),
            map(Vec3::new(gx1 as f32, 0.0, gz as f32)),
            0.4,
            LIGHT,
        );
    }
    for w2 in v.planned.windows(2) {
        canvas.line(map(w2[0]), map(w2[1]), 2.5, [190, 225, 200]);
    }
    for w2 in v.simulation.windows(2) {
        canvas.line(map(w2[0]), map(w2[1]), 0.6, GREY);
    }
    for w2 in v.root.windows(2) {
        canvas.line(map(w2[0]), map(w2[1]), 0.9, INK);
    }
    for (left, ankle, toe) in v.prints {
        let c = if *left { FAR } else { NEAR };
        canvas.line(map(*ankle), map(*toe), (0.045 * s).max(1.5), c);
    }
}
