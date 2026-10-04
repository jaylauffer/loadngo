//! Text layout for the hosts that rasterize with `fontdue` (Linux, Windows,
//! Android): bundled fallback fonts and real shaping.
//!
//! `fontdue` draws one character at a time from one font. That is enough
//! for Latin, but a character the font lacks becomes a missing-glyph box, and
//! Thai and Sinhala are not drawn character by character at all: Thai marks
//! stack above and below the base letter, and Sinhala vowel signs reorder and
//! split around the consonant and consonants form conjuncts. This crate
//! decides which font draws each character, shapes the runs of complex-script
//! text with `rustybuzz` (a HarfBuzz port), and hands the host positioned
//! glyphs it rasterizes with `fontdue`'s glyph-index API.
//!
//! The host's own font keeps every character it can draw (Latin text lays out
//! exactly as before, with no shaping); only the scripts below change.
//!
//! | Script | Face | Takes over |
//! |---|---|---|
//! | Thai | Noto Sans Thai | always (needs shaping even when the host font has glyphs) |
//! | Sinhala | Noto Sans Sinhala | always |
//! | Simplified Chinese | Noto Sans SC (GB2312 subset) | only characters the host font lacks |
//!
//! The faces are embedded (`include_bytes!`), so they behave the same on
//! every platform and need no asset path. Licences: `fonts/OFL-*.txt`.
//! Each face is parsed on first use, so a Latin-only program pays nothing.

use fontdue::{Font, FontSettings, Metrics};
use rustybuzz::{Direction, Face, UnicodeBuffer};
use std::sync::OnceLock;

/// Where a placed glyph comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphSource {
    /// The host's own font, drawn by character exactly as it always was.
    Primary(char),
    /// A bundled face (index into [`face_name`]) drawn by glyph index.
    Fallback { face: usize, glyph: u16 },
}

/// One glyph placed on a line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub source: GlyphSource,
    /// Pen position of the glyph origin, in pixels from the start of the
    /// line, shaping offsets included.
    pub x: f32,
    /// Shaping offset in pixels from the baseline, positive upward (the
    /// raised or lowered position of a Thai tone mark, for instance).
    pub y: f32,
}

/// A laid-out line: glyphs in visual (left-to-right) order, with the line's
/// measured width and drawn advance.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedLine {
    pub glyphs: Vec<PlacedGlyph>,
    /// The line's width by the measuring rule (host glyphs count the wider
    /// of advance and ink width).
    pub width: f32,
    /// Where the drawing pen ends: the sum of the advances glyphs are placed
    /// by. Hosts that measure by ink extent start from this.
    pub advance: f32,
}

struct FallbackSpec {
    name: &'static str,
    data: &'static [u8],
    /// Takes characters of its script even when the host font has a glyph:
    /// a font with its own Thai glyphs and no shaping must not draw Thai.
    owns: fn(char) -> bool,
}

const FACES: &[FallbackSpec] = &[
    FallbackSpec {
        name: "Noto Sans Thai",
        data: include_bytes!("../fonts/NotoSansThai-Regular.ttf"),
        owns: |c| ('\u{0E00}'..='\u{0E7F}').contains(&c),
    },
    FallbackSpec {
        name: "Noto Sans Sinhala",
        data: include_bytes!("../fonts/NotoSansSinhala-Regular.ttf"),
        owns: |c| ('\u{0D80}'..='\u{0DFF}').contains(&c),
    },
    FallbackSpec {
        name: "Noto Sans SC",
        data: include_bytes!("../fonts/NotoSansSC-GB2312-Regular.otf"),
        owns: |_| false,
    },
];

struct Loaded {
    shaper: OnceLock<Option<Face<'static>>>,
    raster: OnceLock<Option<Font>>,
}

static LOADED: [Loaded; FACES.len()] = [const {
    Loaded {
        shaper: OnceLock::new(),
        raster: OnceLock::new(),
    }
}; FACES.len()];

/// Name of bundled face `index`, for diagnostics.
#[must_use]
pub fn face_name(index: usize) -> &'static str {
    FACES[index].name
}

fn shaper_face(index: usize) -> Option<&'static Face<'static>> {
    LOADED[index]
        .shaper
        .get_or_init(|| Face::from_slice(FACES[index].data, 0))
        .as_ref()
}

fn raster_font(index: usize) -> Option<&'static Font> {
    LOADED[index]
        .raster
        .get_or_init(|| Font::from_bytes(FACES[index].data, FontSettings::default()).ok())
        .as_ref()
}

fn face_covers(index: usize, ch: char) -> bool {
    shaper_face(index).is_some_and(|face| face.glyph_index(ch).is_some())
}

/// Characters that join the run before them instead of choosing a font:
/// the zero-width joiners and the variation selector. Sinhala conjuncts are
/// formed with U+200D, so it must reach the shaper inside the same run.
fn is_joiner(ch: char) -> bool {
    matches!(ch, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FE0F}')
}

fn primary_has(primary: &Font, ch: char) -> bool {
    primary.lookup_glyph_index(ch) != 0
}

/// The bundled face that should draw `ch`, if any.
fn face_for(primary: &Font, ch: char) -> Option<usize> {
    let owner = (0..FACES.len()).find(|&i| (FACES[i].owns)(ch) && face_covers(i, ch));
    if owner.is_some() {
        return owner;
    }
    if primary_has(primary, ch) {
        return None;
    }
    (0..FACES.len()).find(|&i| face_covers(i, ch))
}

/// Whether any character of `text` needs a bundled face. Latin text answers
/// `false` after one glyph lookup per character and allocates nothing.
fn needs_fallback(primary: &Font, text: &str) -> bool {
    text.chars()
        .any(|ch| !ch.is_ascii() && !is_joiner(ch) && face_for(primary, ch).is_some())
}

/// How far a character advances the pen in the host's own font. The host
/// text paths have always used the wider of advance and ink width.
fn primary_advance(primary: &Font, ch: char, px: f32) -> f32 {
    let metrics = primary.metrics(ch, px);
    metrics.advance_width.max(metrics.width as f32)
}

/// Width of one line (no newlines) in pixels.
#[must_use]
pub fn line_width(primary: &Font, text: &str, px: f32) -> f32 {
    if needs_fallback(primary, text) {
        return layout_line(primary, text, px).width;
    }
    text.chars()
        .map(|ch| primary_advance(primary, ch, px))
        .sum()
}

/// Lays out one line (no newlines) at `px` pixels per em.
///
/// Glyphs of the host font are placed by plain advance, as the hosts have
/// always drawn them, while `width` is the measuring rule
/// ([`primary_advance`]: the wider of advance and ink width). The two differ
/// only for host glyphs whose ink is wider than their advance.
#[must_use]
pub fn layout_line(primary: &Font, text: &str, px: f32) -> ShapedLine {
    layout_line_with_space_floor(primary, text, px, 0.0)
}

/// [`layout_line`] for a host that advances a space by at least
/// `space_floor` pixels (Android uses `0.3 * px`), whatever the host font's
/// own space advance is.
#[must_use]
pub fn layout_line_with_space_floor(
    primary: &Font,
    text: &str,
    px: f32,
    space_floor: f32,
) -> ShapedLine {
    let mut glyphs = Vec::with_capacity(text.len());
    let mut pen = 0.0f32;
    let mut width = 0.0f32;
    let mut run = String::new();
    let mut run_face: Option<usize> = None;

    for ch in text.chars() {
        let face = if is_joiner(ch) {
            run_face
        } else {
            face_for(primary, ch)
        };
        if face != run_face {
            flush_run(&mut glyphs, &mut pen, &mut width, &mut run, run_face, px);
            run_face = face;
        }
        match face {
            Some(_) => run.push(ch),
            None => {
                glyphs.push(PlacedGlyph {
                    source: GlyphSource::Primary(ch),
                    x: pen,
                    y: 0.0,
                });
                let mut advance = primary.metrics(ch, px).advance_width;
                if ch == ' ' {
                    advance = advance.max(space_floor);
                }
                pen += advance;
                width += primary_advance(primary, ch, px);
            }
        }
    }
    flush_run(&mut glyphs, &mut pen, &mut width, &mut run, run_face, px);
    ShapedLine {
        glyphs,
        width,
        advance: pen,
    }
}

fn flush_run(
    glyphs: &mut Vec<PlacedGlyph>,
    pen: &mut f32,
    width: &mut f32,
    run: &mut String,
    face: Option<usize>,
    px: f32,
) {
    let Some(index) = face else {
        run.clear();
        return;
    };
    if run.is_empty() {
        return;
    }
    let Some(shaper) = shaper_face(index) else {
        run.clear();
        return;
    };
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(run);
    buffer.set_direction(Direction::LeftToRight);
    buffer.guess_segment_properties();
    let shaped = rustybuzz::shape(shaper, &[], buffer);
    let scale = px / shaper.units_per_em().max(1) as f32;
    for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        glyphs.push(PlacedGlyph {
            source: GlyphSource::Fallback {
                face: index,
                glyph: info.glyph_id as u16,
            },
            x: *pen + position.x_offset as f32 * scale,
            y: position.y_offset as f32 * scale,
        });
        let advance = position.x_advance as f32 * scale;
        *pen += advance;
        *width += advance;
    }
    run.clear();
}

/// Metrics of a placed glyph without drawing it (for hosts that measure by
/// ink extent).
#[must_use]
pub fn glyph_metrics(primary: &Font, glyph: &PlacedGlyph, px: f32) -> Metrics {
    match glyph.source {
        GlyphSource::Primary(ch) => primary.metrics(ch, px),
        GlyphSource::Fallback { face, glyph } => raster_font(face)
            .map(|font| font.metrics_indexed(glyph, px))
            .unwrap_or_default(),
    }
}

/// Rasterizes a placed glyph. A bundled face that failed to parse yields an
/// empty bitmap, as a missing glyph would.
#[must_use]
pub fn rasterize(primary: &Font, glyph: &PlacedGlyph, px: f32) -> (Metrics, Vec<u8>) {
    match glyph.source {
        GlyphSource::Primary(ch) => primary.rasterize(ch, px),
        GlyphSource::Fallback { face, glyph } => match raster_font(face) {
            Some(font) => font.rasterize_indexed(glyph, px),
            None => (Metrics::default(), Vec::new()),
        },
    }
}

#[cfg(test)]
mod tests;
