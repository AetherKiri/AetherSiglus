//! Fallback for titles that draw text from pre-baked glyph atlases.
//!
//! Some Siglus titles render dialogue balloons as one G00 cut per character
//! instead of using the live font. The game script maps a character code to
//! `_moji_<face><size>_<row>` plus a cut index (`code / row_width`,
//! `code % row_width`), which is why those atlases are full Unicode-cell grids:
//! cell `n` of row `r` is the glyph for code `r * row_width + n`.
//!
//! Translation patches that replace only the script text inherit the original
//! atlas. Characters the original face never had are stored as empty
//! placeholders (`count = 0`, a 1x1 transparent cut), so the balloon shows a
//! gap exactly where the missing glyph belongs. This module rasters such a
//! character from the live font stack using the atlas' own cut geometry, so the
//! synthesized cut is indistinguishable from a real one apart from the face.
//!
//! Safety: only cuts that are genuinely empty are replaced, only inside files
//! named like a glyph atlas, and only when the atlas' grid is consistent. The
//! whole pass is disabled with `AETHERKIRI_GLYPH_ATLAS_FALLBACK=off`.

use crate::assets::RgbaImage;
use crate::text_render::RasterGlyph;
use ab_glyph::{Font, FontArc};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

/// Rasterization size used to measure a face's full-size ink height.
const CALIBRATION_PX: f32 = 40.0;
/// Ideograph every CJK face covers, used as the full-size ink reference.
const REFERENCE_CHAR: char = '中';
/// Coverage above which a raster pixel counts as glyph body, not fringe.
const BODY_COVERAGE: u8 = 8;
/// Outline thickness used when the atlas row carries no measurable one.
const DEFAULT_OUTLINE_PX: i32 = 3;
/// Largest outline copied from an atlas cut (the sheets stay well below this).
const MAX_OUTLINE_PX: i32 = 6;

/// Geometry and style shared by the real cuts of one atlas row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AtlasTemplate {
    pub width: u32,
    pub height: u32,
    /// Ink height of a full-size glyph in the atlas, in pixels.
    pub ink_px: f32,
    /// Thickness of the white outline the atlas bakes around each glyph.
    pub outline_px: i32,
}

/// Row index encoded in a glyph-atlas file stem (`_moji_AN48_0084` -> 84).
pub(crate) fn row_from_stem(stem: &str) -> Option<u32> {
    // Checked on every G00 load, so stay allocation-free.
    let prefix = stem.get(.."_moji_".len())?;
    if !prefix.eq_ignore_ascii_case("_moji_") {
        return None;
    }
    let rest = &stem["_moji_".len()..];
    let (_, row) = rest.rsplit_once('_')?;
    if row.len() != 4 || !row.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    row.parse::<u32>().ok()
}

/// A cut the atlas left empty: the decoder emits a 1x1 transparent placeholder.
pub(crate) fn is_empty_cut(img: &RgbaImage) -> bool {
    img.width == 1 && img.height == 1 && img.rgba.iter().all(|byte| *byte == 0)
}

/// Character the atlas reserves for `index` in `row`, given the atlas row
/// width (one `row_width` per row, which is also the number of cuts per file).
pub(crate) fn character(row: u32, index: usize, row_width: usize) -> Option<char> {
    if row_width == 0 {
        return None;
    }
    let code = (row as usize).checked_mul(row_width)?.checked_add(index)?;
    let ch = char::from_u32(u32::try_from(code).ok()?)?;
    (!ch.is_control()).then_some(ch)
}

/// Whether glyph-atlas synthesis is enabled (on unless explicitly disabled).
pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var("AETHERKIRI_GLYPH_ATLAS_FALLBACK")
                .unwrap_or_default()
                .trim(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// Measure the cut geometry of one atlas row from its non-empty cuts.
pub(crate) fn template(frames: &[RgbaImage]) -> Option<AtlasTemplate> {
    // The atlas rasterizes a whole row with one cell layout, so the most common
    // (and only) real cut size is the geometry a synthesized cut must copy:
    // the script positions every glyph object on that same grid.
    let mut sizes: HashMap<(u32, u32), usize> = HashMap::new();
    for img in frames.iter().filter(|img| !is_empty_cut(img)) {
        *sizes.entry((img.width, img.height)).or_default() += 1;
    }
    let ((width, height), _) = sizes.into_iter().max_by_key(|(_, count)| *count)?;
    if width == 0 || height == 0 {
        return None;
    }

    let mut ink_px = 0.0f32;
    let mut outline_px = 0;
    for img in frames {
        if img.width != width || img.height != height {
            continue;
        }
        // Only the dark body defines the glyph's size: the atlas bakes an
        // opaque white outline around it, which must not inflate the em.
        if let Some((_, top, _, bottom)) = body_bbox_image(img) {
            ink_px = ink_px.max((bottom - top) as f32);
        }
        outline_px = outline_px.max(outline_px_from(img).unwrap_or(0));
    }
    if ink_px < 1.0 {
        return None;
    }
    Some(AtlasTemplate {
        width,
        height,
        ink_px,
        outline_px: if outline_px > 0 {
            outline_px.min(MAX_OUTLINE_PX)
        } else {
            DEFAULT_OUTLINE_PX
        },
    })
}

/// Build the missing cut for `ch` with the live font stack.
///
/// Returns `None` when no face covers `ch`, leaving the placeholder in place.
pub(crate) fn synthesize(
    project_dir: &Path,
    primary: Option<&FontArc>,
    template: &AtlasTemplate,
    ch: char,
) -> Option<RgbaImage> {
    // Calibrate against a reference ideograph instead of the target glyph: the
    // atlas ink height describes a full-size character, while glyphs that are
    // naturally small (一, punctuation) must stay small.
    let reference = rasterize(project_dir, primary, REFERENCE_CHAR, CALIBRATION_PX)?;
    let (_, ref_top, _, ref_bottom) = ink_bbox(&reference)?;
    let reference_ink = (ref_bottom - ref_top) as f32;
    if reference_ink < 1.0 {
        return None;
    }
    let px = (CALIBRATION_PX * template.ink_px / reference_ink).clamp(1.0, 512.0);

    let glyph = rasterize(project_dir, primary, ch, px)?;
    let bbox = ink_bbox(&glyph)?;
    compose(template, &glyph, bbox)
}

fn rasterize(
    project_dir: &Path,
    primary: Option<&FontArc>,
    ch: char,
    px: f32,
) -> Option<RasterGlyph> {
    if let Some(font) = primary
        && font.glyph_id(ch).0 != 0
    {
        return Some(crate::font_fallback::rasterize_glyph_cached(font, ch, px));
    }
    crate::font_fallback::rasterize_chain_glyph(project_dir, ch, px)
}

/// Paint one glyph into an atlas-shaped cut.
///
/// The sheets store opaque grayscale ink plus an opaque white outline on a
/// transparent cell: the game tints the cut with `color_add`, so the grayscale
/// body becomes the dialogue colour while the outline stays white.
fn compose(
    template: &AtlasTemplate,
    glyph: &RasterGlyph,
    bbox: (i32, i32, i32, i32),
) -> Option<RgbaImage> {
    let width = template.width as i32;
    let height = template.height as i32;
    if width <= 0 || height <= 0 || glyph.width == 0 || glyph.height == 0 {
        return None;
    }
    let (left, top, right, bottom) = bbox;
    // The atlas keeps the character's em box centred in its cell, which puts
    // the ink box at the same offset; copy that placement.
    let offset_x = (width - (right - left)) / 2 - left;
    let offset_y = (height - (bottom - top)) / 2 - top;

    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let mut body = vec![false; (width * height) as usize];
    for gy in 0..glyph.height as i32 {
        for gx in 0..glyph.width as i32 {
            let coverage = glyph.bitmap[gy as usize * glyph.width + gx as usize];
            if coverage == 0 {
                continue;
            }
            let x = gx + offset_x;
            let y = gy + offset_y;
            if x < 0 || y < 0 || x >= width || y >= height {
                continue;
            }
            let index = ((y * width + x) * 4) as usize;
            let value = 255 - coverage;
            rgba[index] = value;
            rgba[index + 1] = value;
            rgba[index + 2] = value;
            rgba[index + 3] = 255;
            if coverage >= BODY_COVERAGE {
                body[(y * width + x) as usize] = true;
            }
        }
    }

    let radius = template.outline_px.max(0);
    if radius > 0 {
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) as usize;
                if body[index] || rgba[index * 4 + 3] != 0 {
                    continue;
                }
                let mut covered = false;
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        let nx = x + dx;
                        let ny = y + dy;
                        if nx < 0 || ny < 0 || nx >= width || ny >= height {
                            continue;
                        }
                        if body[(ny * width + nx) as usize] {
                            covered = true;
                            break;
                        }
                    }
                    if covered {
                        break;
                    }
                }
                if covered {
                    let pixel = index * 4;
                    rgba[pixel] = 255;
                    rgba[pixel + 1] = 255;
                    rgba[pixel + 2] = 255;
                    rgba[pixel + 3] = 255;
                }
            }
        }
    }

    Some(RgbaImage {
        width: template.width,
        height: template.height,
        center_x: 0,
        center_y: 0,
        rgba,
    })
}

type InkBox = (i32, i32, i32, i32);

fn ink_bbox(glyph: &RasterGlyph) -> Option<InkBox> {
    blit_bbox(glyph.width, glyph.height, |x, y| {
        glyph.bitmap[y as usize * glyph.width + x as usize] != 0
    })
}

/// Bounding box of the opaque dark glyph body, ignoring the white outline.
fn body_bbox_image(img: &RgbaImage) -> Option<InkBox> {
    blit_bbox(img.width as usize, img.height as usize, |x, y| {
        let index = (y as usize * img.width as usize + x as usize) * 4;
        is_atlas_body(
            img.rgba[index],
            img.rgba[index + 1],
            img.rgba[index + 2],
            img.rgba[index + 3],
        )
    })
}

fn is_atlas_body(r: u8, g: u8, b: u8, a: u8) -> bool {
    a > 128 && r.max(g).max(b) < 200
}

fn is_atlas_outline(r: u8, g: u8, b: u8, a: u8) -> bool {
    a > 128 && r.min(g).min(b) >= 200
}

fn blit_bbox(width: usize, height: usize, covered: impl Fn(i32, i32) -> bool) -> Option<InkBox> {
    let (mut left, mut top) = (i32::MAX, i32::MAX);
    let (mut right, mut bottom) = (i32::MIN, i32::MIN);
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            if !covered(x, y) {
                continue;
            }
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    (left <= right && top <= bottom).then_some((left, top, right, bottom))
}

/// Widest distance an outline pixel reaches from the glyph body in one cut.
fn outline_px_from(img: &RgbaImage) -> Option<i32> {
    let width = img.width as i32;
    let height = img.height as i32;
    if width <= 0 || height <= 0 {
        return None;
    }
    let pixel = |x: i32, y: i32| -> (u8, u8, u8, u8) {
        let index = ((y * width + x) * 4) as usize;
        (
            img.rgba[index],
            img.rgba[index + 1],
            img.rgba[index + 2],
            img.rgba[index + 3],
        )
    };

    let is_body = |x: i32, y: i32| {
        let (r, g, b, a) = pixel(x, y);
        is_atlas_body(r, g, b, a)
    };
    let is_white = |x: i32, y: i32| {
        let (r, g, b, a) = pixel(x, y);
        is_atlas_outline(r, g, b, a)
    };

    let mut distance = vec![i32::MAX; (width * height) as usize];
    let mut queue = std::collections::VecDeque::new();
    for y in 0..height {
        for x in 0..width {
            if is_body(x, y) {
                distance[(y * width + x) as usize] = 0;
                queue.push_back((x, y));
            }
        }
    }
    if queue.is_empty() {
        return None;
    }
    while let Some((x, y)) = queue.pop_front() {
        let current = distance[(y * width + x) as usize];
        if current >= MAX_OUTLINE_PX + 2 {
            continue;
        }
        for (dx, dy) in NEIGHBOURS {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= width || ny >= height {
                continue;
            }
            let index = (ny * width + nx) as usize;
            if distance[index] != i32::MAX {
                continue;
            }
            distance[index] = current + 1;
            queue.push_back((nx, ny));
        }
    }

    let mut outline = 0;
    for y in 0..height {
        for x in 0..width {
            if !is_white(x, y) {
                continue;
            }
            let value = distance[(y * width + x) as usize];
            if value != i32::MAX {
                outline = outline.max(value);
            }
        }
    }
    (outline > 0).then_some(outline)
}

const NEIGHBOURS: [(i32, i32); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::RgbaImage;

    fn test_font() -> FontArc {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/default.ttf");
        let bytes = std::fs::read(path).expect("read bundled test font");
        FontArc::from(ab_glyph::FontVec::try_from_vec(bytes).expect("parse bundled test font"))
    }

    fn empty_cut() -> RgbaImage {
        RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![0, 0, 0, 0],
        }
    }

    /// A stand-in atlas cut: an opaque black bar with a white outline.
    fn cut(ink_height: u32, outline_px: i32) -> RgbaImage {
        let (width, height) = (48u32, 40u32);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let ink_top = (height - ink_height) / 2;
        let ink_bottom = ink_top + ink_height;
        for y in ink_top..ink_bottom {
            for x in 8..40 {
                let index = ((y * width + x) * 4) as usize;
                rgba[index..index + 4].copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                let index = ((y as u32 * width + x as u32) * 4) as usize;
                if rgba[index + 3] != 0 {
                    continue;
                }
                let near_ink = (0..=outline_px).any(|dx| {
                    (0..=outline_px).any(|dy| {
                        let inside_ink = (ink_top as i32..ink_bottom as i32).contains(&(y + dy))
                            && (8..40).contains(&(x + dx));
                        let inside_ink_neg = (ink_top as i32..ink_bottom as i32)
                            .contains(&(y - dy))
                            && (8..40).contains(&(x - dx));
                        inside_ink || inside_ink_neg
                    })
                });
                if near_ink {
                    rgba[index..index + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        RgbaImage {
            width,
            height,
            center_x: 0,
            center_y: 0,
            rgba,
        }
    }

    #[test]
    fn atlas_rows_come_from_the_file_stem() {
        assert_eq!(row_from_stem("_moji_AN48_0084"), Some(84));
        assert_eq!(row_from_stem("_moji_SE24_0012"), Some(12));
        // Emote sheets and unpadded rows are not character atlases.
        assert_eq!(row_from_stem("_exmoji_c01_48"), None);
        assert_eq!(row_from_stem("_moji_AN48_84"), None);
        assert_eq!(row_from_stem("bg101"), None);
    }

    #[test]
    fn atlas_cells_decode_to_their_unicode_code() {
        // The mapping the game script uses: `code = row * row_width + index`.
        assert_eq!(character(84, 189, 441), Some('酱'));
        assert_eq!(character(48, 359, 441), Some('吗'));
        // Cell 0 of row 0 is NUL, which is never a glyph.
        assert_eq!(character(0, 0, 441), None);
    }

    #[test]
    fn only_placeholder_cuts_look_empty() {
        assert!(is_empty_cut(&empty_cut()));
        assert!(!is_empty_cut(&cut(20, 2)));
        let mut opaque = empty_cut();
        opaque.rgba[3] = 255;
        assert!(!is_empty_cut(&opaque));
    }

    #[test]
    fn template_measures_cell_ink_and_outline() {
        let frames = vec![empty_cut(), cut(20, 2), cut(30, 4)];
        let template = template(&frames).expect("template from real cuts");
        assert_eq!((template.width, template.height), (48, 40));
        // The tallest ink describes a full-size glyph; the widest outline is
        // what the synthesized cut has to reproduce.
        assert_eq!(template.ink_px, 30.0);
        assert_eq!(template.outline_px, 4);
    }

    #[test]
    fn synthesized_cut_matches_the_atlas_geometry() {
        let font = test_font();
        let template = AtlasTemplate {
            width: 48,
            height: 40,
            ink_px: 28.0,
            outline_px: 3,
        };
        let cut = synthesize(Path::new("."), Some(&font), &template, '中')
            .expect("bundled face covers the reference ideograph");
        assert_eq!((cut.width, cut.height), (48, 40));
        assert!(!is_empty_cut(&cut));
        // Ink stays dark (the engine tints it with `color_add`) while the
        // outline is opaque white, exactly like the atlas' own cuts.
        let darkest = cut
            .rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 0)
            .map(|px| px[0])
            .min()
            .expect("synthesized ink");
        assert!(darkest < 32, "ink should stay near black, got {darkest}");
        let white = cut
            .rgba
            .chunks_exact(4)
            .filter(|px| px[3] > 0 && px[0] > 200 && px[1] > 200 && px[2] > 200)
            .count();
        assert!(white > 0, "synthesized cut needs the atlas' white outline");
        // The ink is centred in the cell like the real cuts.
        let (_, top, _, bottom) = body_bbox_image(&cut).expect("ink box");
        assert!((top + bottom) / 2 >= 18 && (top + bottom) / 2 <= 22);
    }
}
