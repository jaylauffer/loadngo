use super::*;

/// A Latin host font, as the games' own fonts are.
fn primary() -> Font {
    Font::from_bytes(
        &include_bytes!("../../assets/fonts/exo/Exo-Regular.otf")[..],
        FontSettings::default(),
    )
    .expect("Exo parses")
}

const THAI: usize = 0;
const SINHALA: usize = 1;
const SC: usize = 2;

/// One glyph of `hb-shape --no-glyph-names` output: `gid=cluster@dx,dy+advance`
/// (the `@dx,dy` part is absent when both are zero), in font units.
struct Expected {
    glyph: u16,
    dx: f32,
    dy: f32,
    advance: f32,
}

/// Parses `[19=0+587|47=0@23,0+0|...]`.
fn parse_hb(output: &str) -> Vec<Expected> {
    output
        .trim_matches(['[', ']'])
        .split('|')
        .map(|item| {
            let (head, advance) = item.rsplit_once('+').expect("advance");
            let (head, offset) = match head.split_once('@') {
                Some((head, offset)) => (head, Some(offset)),
                None => (head, None),
            };
            let (glyph, _cluster) = head.split_once('=').expect("gid=cluster");
            let (dx, dy) = offset.map_or((0.0, 0.0), |offset| {
                let (dx, dy) = offset.split_once(',').expect("dx,dy");
                (dx.parse().expect("dx"), dy.parse().expect("dy"))
            });
            Expected {
                glyph: glyph.parse().expect("gid"),
                dx,
                dy,
                advance: advance.parse().expect("advance"),
            }
        })
        .collect()
}

/// Shapes `text` with face `index` at one pixel per font unit, so positions
/// compare directly with HarfBuzz's.
fn check_against_harfbuzz(index: usize, text: &str, hb_output: &str) {
    let upem = shaper_face(index).expect("face").units_per_em() as f32;
    let line = layout_line(&primary(), text, upem);
    let expected = parse_hb(hb_output);
    assert_eq!(
        line.glyphs.len(),
        expected.len(),
        "{text:?}: glyph count differs from HarfBuzz"
    );
    let mut pen = 0.0f32;
    for (placed, want) in line.glyphs.iter().zip(&expected) {
        assert_eq!(
            placed.source,
            GlyphSource::Fallback {
                face: index,
                glyph: want.glyph
            },
            "{text:?}: glyph differs"
        );
        assert!(
            (placed.x - (pen + want.dx)).abs() < 0.01,
            "{text:?}: x {} vs {}",
            placed.x,
            pen + want.dx
        );
        assert!((placed.y - want.dy).abs() < 0.01, "{text:?}: y offset");
        pen += want.advance;
    }
    assert!((line.width - pen).abs() < 0.01, "{text:?}: width");
}

// The expected strings are real HarfBuzz 14.3.0 output:
//   hb-shape --font-file=fonts/<face> --no-glyph-names --direction=ltr --text=<text>

#[test]
fn thai_living_room_matches_harfbuzz() {
    check_against_harfbuzz(
        THAI,
        "ห้องนั่งเล่น",
        "[19=0+587|47=0@23,0+0|72=2+574|58=3+537|71=4+613|45=4+0|44=4@30,-57+0|58=7+537|91=8+285|33=9+571|42=9@-7,0+0|71=11+613]",
    );
}

#[test]
fn thai_greeting_matches_harfbuzz() {
    check_against_harfbuzz(
        THAI,
        "สวัสดี",
        "[110=0+572|134=1+492|45=1@10,0+0|110=3+572|12=4+616|94=4+0]",
    );
}

#[test]
fn thai_stacked_marks_match_harfbuzz() {
    check_against_harfbuzz(
        THAI,
        "ผู้ใหญ่",
        "[78=0+648|103=0@1,0+0|47=0@1,0+0|89=3+289|19=4+587|137=5+909|42=5+0]",
    );
}

#[test]
fn sinhala_sri_conjunct_with_zero_width_joiner_matches_harfbuzz() {
    check_against_harfbuzz(
        SINHALA,
        "ශ්\u{200D}රී",
        "[58=0+915|130=0@-865,0+0|96=0@-748,0+0]",
    );
}

#[test]
fn sinhala_anusvara_matches_harfbuzz() {
    check_against_harfbuzz(
        SINHALA,
        "සිංහල",
        "[60=0+898|85=0@-727,0+0|64=0+470|61=3+970|56=4+852]",
    );
}

#[test]
fn sinhala_split_vowel_matches_harfbuzz() {
    // U+0DDC is drawn as a sign before the consonant and one after it.
    check_against_harfbuzz(
        SINHALA,
        "කොළඹ",
        "[114=0+631|23=0+1007|82=0+338|62=2+784|53=3+874]",
    );
}

#[test]
fn sinhala_rakaransaya_matches_harfbuzz() {
    check_against_harfbuzz(
        SINHALA,
        "ප්\u{200D}රකාශ",
        "[48=0+830|128=0@-756,0+0|23=4+1007|82=4+338|58=6+915]",
    );
}

#[test]
fn sinhala_kssa_ligature_is_one_glyph() {
    check_against_harfbuzz(SINHALA, "ක්\u{200D}ෂ", "[67=0+1424]");
}

#[test]
fn latin_text_never_leaves_the_host_font() {
    let font = primary();
    let text = "Living Room 4";
    let line = layout_line(&font, text, 18.0);
    assert!(line
        .glyphs
        .iter()
        .all(|glyph| matches!(glyph.source, GlyphSource::Primary(_))));
    let by_hand: f32 = text
        .chars()
        .map(|ch| {
            let metrics = font.metrics(ch, 18.0);
            metrics.advance_width.max(metrics.width as f32)
        })
        .sum();
    assert_eq!(line.width, by_hand);
    assert_eq!(line_width(&font, text, 18.0), by_hand);
    assert!(!needs_fallback(&font, text));
}

#[test]
fn simplified_chinese_uses_the_chinese_face_and_each_character_advances() {
    let font = primary();
    let line = layout_line(&font, "你好，世界", 24.0);
    assert_eq!(line.glyphs.len(), 5);
    for glyph in &line.glyphs {
        match glyph.source {
            GlyphSource::Fallback { face, glyph } => {
                assert_eq!(face, SC);
                assert_ne!(glyph, 0, "a covered character is not .notdef");
            }
            GlyphSource::Primary(ch) => panic!("{ch:?} left in the host font"),
        }
    }
    assert!(line.width > 24.0 * 4.0 && line.width < 24.0 * 5.5);
    assert!(line.glyphs.windows(2).all(|pair| pair[1].x > pair[0].x));
}

#[test]
fn the_host_font_wins_a_character_it_has_unless_a_face_owns_the_script() {
    // A host font with Thai glyphs of its own, unshaped, must not draw Thai.
    let thai_face_as_host = Font::from_bytes(
        &include_bytes!("../fonts/NotoSansThai-Regular.ttf")[..],
        FontSettings::default(),
    )
    .expect("Thai parses");
    assert!(primary_has(&thai_face_as_host, 'ก'));
    let line = layout_line(&thai_face_as_host, "ก", 20.0);
    assert!(matches!(
        line.glyphs[0].source,
        GlyphSource::Fallback { face: THAI, .. }
    ));
    assert_eq!(line.glyphs.len(), 1);
}

#[test]
fn a_character_no_face_covers_stays_in_the_host_font() {
    let font = primary();
    // Hebrew: not in Exo and not in any bundled face.
    let line = layout_line(&font, "א", 18.0);
    assert_eq!(line.glyphs[0].source, GlyphSource::Primary('א'));
}

#[test]
fn mixed_scripts_split_into_runs_in_order() {
    let font = primary();
    let line = layout_line(&font, "Room ห้อง 你好 ok", 20.0);
    let sources: Vec<_> = line
        .glyphs
        .iter()
        .map(|glyph| match glyph.source {
            GlyphSource::Primary(_) => 'p',
            GlyphSource::Fallback { face, .. } => match face {
                THAI => 't',
                SINHALA => 's',
                _ => 'c',
            },
        })
        .collect();
    let text: String = sources.iter().collect();
    // "Room " | Thai glyphs | " " | 2 Chinese | " ok"
    assert!(text.starts_with("ppppp"));
    assert!(text.contains('t') && text.contains("cc"));
    assert!(text.ends_with("ppp"), "{text}");
    // Marks may sit left of the previous origin, so only the runs' order is fixed.
    let first_chinese = sources
        .iter()
        .position(|&kind| kind == 'c')
        .expect("chinese");
    let last_thai = sources.iter().rposition(|&kind| kind == 't').expect("thai");
    assert!(last_thai < first_chinese);
    assert!(line.glyphs[first_chinese].x > line.glyphs[last_thai].x);
    assert!((line.width - line_width(&font, "Room ห้อง 你好 ok", 20.0)).abs() < 0.001);
}

#[test]
fn rasterizing_a_shaped_glyph_draws_ink() {
    let font = primary();
    let line = layout_line(&font, "ก", 32.0);
    let (metrics, bitmap) = rasterize(&font, &line.glyphs[0], 32.0);
    assert!(metrics.width > 4 && metrics.height > 4);
    assert!(bitmap.iter().any(|&alpha| alpha > 128));
    let line = layout_line(&font, "你", 32.0);
    let (metrics, bitmap) = rasterize(&font, &line.glyphs[0], 32.0);
    assert!(metrics.width > 10 && bitmap.iter().any(|&alpha| alpha > 128));
}

#[test]
fn a_trailing_joiner_without_a_run_does_not_panic() {
    let font = primary();
    let line = layout_line(&font, "\u{200D}a\u{200D}", 12.0);
    assert!(!line.glyphs.is_empty());
}

#[test]
fn a_host_font_with_sinhala_glyphs_still_hands_sinhala_to_the_shaper() {
    let sinhala_face_as_host = Font::from_bytes(
        &include_bytes!("../fonts/NotoSansSinhala-Regular.ttf")[..],
        FontSettings::default(),
    )
    .expect("Sinhala parses");
    assert!(primary_has(&sinhala_face_as_host, 'ක'));
    let line = layout_line(&sinhala_face_as_host, "ක", 20.0);
    assert!(matches!(
        line.glyphs[0].source,
        GlyphSource::Fallback { face: SINHALA, .. }
    ));
}

#[test]
fn the_space_floor_widens_a_narrow_space_and_only_a_space() {
    let font = primary();
    let natural = layout_line(&font, "a b", 20.0);
    let floored = layout_line_with_space_floor(&font, "a b", 20.0, 50.0);
    let space_to_b = |line: &ShapedLine| line.glyphs[2].x - line.glyphs[1].x;
    assert!(space_to_b(&natural) < 20.0);
    assert!((space_to_b(&floored) - 50.0).abs() < 0.001);
    // 'a' keeps its own advance: the floor is for spaces.
    assert_eq!(natural.glyphs[1].x, floored.glyphs[1].x);
    assert!((floored.advance - natural.advance - (50.0 - space_to_b(&natural))).abs() < 0.001);
}
