//! CPU reference implementation of Siglus stage wipes.
//!
//! The original engine renders the selected order/layer interval into temporary
//! targets and then combines the old/front and new/next targets.  Keeping that
//! structure is important: applying a clip or alpha independently to each leaf
//! sprite gives a different result for overlapping translucent objects.

use crate::assets::RgbaImage;

#[derive(Debug, Clone, Copy)]
struct RectF {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl RectF {
    fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: width as f32,
            h: height as f32,
        }
    }
}

pub fn eased_progress(raw: f32, speed_mode: i32) -> f32 {
    let p = raw.clamp(0.0, 1.0);
    match speed_mode {
        1 => p * p,
        2 => 1.0 - (1.0 - p) * (1.0 - p),
        3 => p * p * (3.0 - 2.0 * p),
        _ => p,
    }
}

pub fn uses_cpu_compositor(wipe_type: i32) -> bool {
    matches!(wipe_type, 1 | 2 | 5 | 10 | 11 | 20..=22 | 30..=32 | 40..=44
        | 60..=69 | 70..=79 | 80..=83 | 90..=93 | 100..=102 | 110..=111
        | 120..=122 | 130..=132 | 140..=142 | 150..=152 | 200 | 210..=215
        | 300 | 301 | 900 | 901)
}

pub fn compose(
    current: &RgbaImage,
    next: &RgbaImage,
    external_mask: Option<&RgbaImage>,
    wipe_type: i32,
    option: &[i32],
    progress: f32,
) -> RgbaImage {
    let p = progress.clamp(0.0, 1.0);
    if p <= 0.0 {
        return current.clone();
    }
    if p >= 1.0 {
        return next.clone();
    }

    match wipe_type {
        0 => crossfade(current, next, p),
        1 => current.clone(),
        2 => next.clone(),
        200 => compose_move(current, next, option, p),
        210..=215 => compose_scale(current, next, wipe_type, option, p),
        300 | 301 => compose_page(current, next, wipe_type, option, p),
        900 | 901 => compose_external_mask(current, next, external_mask, wipe_type, option, p),
        _ => compose_generated_mask(current, next, wipe_type, option, p),
    }
}

fn blank_like(img: &RgbaImage) -> RgbaImage {
    RgbaImage {
        width: img.width,
        height: img.height,
        center_x: 0,
        center_y: 0,
        rgba: vec![0; img.width as usize * img.height as usize * 4],
    }
}

fn same_size(a: &RgbaImage, b: &RgbaImage) -> bool {
    a.width == b.width && a.height == b.height && a.rgba.len() == b.rgba.len()
}

fn compose_generated_mask(
    current: &RgbaImage,
    next: &RgbaImage,
    wipe_type: i32,
    option: &[i32],
    p: f32,
) -> RgbaImage {
    if !same_size(current, next) {
        return crossfade(current, next, p);
    }
    let seed = generated_mask_seed(wipe_type, option, current.width, current.height);
    let Some(mask) = super::wipe_mask::generate(
        wipe_type,
        option,
        current.width,
        current.height,
        seed,
    ) else {
        return crossfade(current, next, p);
    };
    let mut out = blank_like(current);
    let fade = mask_fade(opt(option, 0, 0));
    for y in 0..current.height {
        for x in 0..current.width {
            let mask_gray = mask.pixels[(y * current.width + x) as usize] as f32 / 255.0;
            // The C++ generators store the first-revealed region as white.
            let threshold = 1.0 - mask_gray;
            let reveal = mask_reveal(p, threshold, fade);
            let idx = ((y * current.width + x) * 4) as usize;
            mix_pixel(&current.rgba, &next.rgba, &mut out.rgba, idx, reveal);
        }
    }
    out
}

fn generated_mask_seed(wipe_type: i32, option: &[i32], width: u32, height: u32) -> u32 {
    let mut seed = (wipe_type as u32)
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add(width.rotate_left(11))
        .wrapping_add(height.rotate_left(23));
    for &value in option {
        seed ^= (value as u32).wrapping_mul(0x85eb_ca6b);
        seed = seed.rotate_left(13).wrapping_mul(0xc2b2_ae35);
    }
    seed
}

fn mask_fade(mode: i32) -> f32 {
    match mode {
        0 => 0.0,
        1 => 1.0 - 1.0 / 2.0,
        2 => 1.0 - 1.0 / 4.0,
        3 => 1.0 - 1.0 / 8.0,
        4 => 1.0 - 1.0 / 16.0,
        5 => 1.0 - 1.0 / 32.0,
        6 => 1.0 - 1.0 / 64.0,
        7 => 1.0 - 1.0 / 128.0,
        _ => 1.0,
    }
}

fn mask_reveal(progress: f32, threshold: f32, fade: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    let threshold = threshold.clamp(0.0, 1.0);
    let fade = fade.clamp(0.0, 1.0);
    if fade <= f32::EPSILON {
        return if progress >= threshold { 1.0 } else { 0.0 };
    }
    // This is the source shader's mask-fade geometry: the mask threshold range
    // is compressed by (1-fade), while `fade == 1` becomes a full crossfade.
    ((progress - threshold * (1.0 - fade)) / fade).clamp(0.0, 1.0)
}

fn opt(option: &[i32], idx: usize, default: i32) -> i32 {
    option.get(idx).copied().unwrap_or(default)
}

fn hash01(x: u32, y: u32, seed: u32) -> f32 {
    let mut v = x.wrapping_mul(0x9e37_79b1)
        ^ y.wrapping_mul(0x85eb_ca6b)
        ^ seed.wrapping_mul(0xc2b2_ae35);
    v ^= v >> 16;
    v = v.wrapping_mul(0x7feb_352d);
    v ^= v >> 15;
    v = v.wrapping_mul(0x846c_a68b);
    v ^= v >> 16;
    (v as f64 / u32::MAX as f64) as f32
}

fn compose_external_mask(
    current: &RgbaImage,
    next: &RgbaImage,
    mask: Option<&RgbaImage>,
    wipe_type: i32,
    option: &[i32],
    p: f32,
) -> RgbaImage {
    let Some(mask) = mask else { return crossfade(current, next, p); };
    if !same_size(current, next) { return crossfade(current, next, p); }
    let mut out = blank_like(current);
    let fade = mask_fade(if wipe_type == 901 { 0 } else { opt(option, 0, 0) });
    for y in 0..current.height {
        for x in 0..current.width {
            let nx = (x as f32 + 0.5) / current.width.max(1) as f32;
            let ny = (y as f32 + 0.5) / current.height.max(1) as f32;
            let threshold = if wipe_type == 901 {
                rotating_mask_value(mask, nx, ny, option, p)
            } else {
                sample_mask(mask, nx, ny)
            };
            let reveal = if wipe_type == 901 {
                if threshold >= 0.5 { 1.0 } else { 0.0 }
            } else {
                mask_reveal(p, 1.0 - threshold, fade)
            };
            let idx = ((y * current.width + x) * 4) as usize;
            mix_pixel(&current.rgba, &next.rgba, &mut out.rgba, idx, reveal);
        }
    }
    out
}

fn rotating_mask_value(mask: &RgbaImage, x: f32, y: f32, option: &[i32], p: f32) -> f32 {
    let max_scale = (opt(option, 1, 1000).max(0) as f32 / 1000.0).max(0.0001);
    let scale = if opt(option, 0, 0) == 0 { max_scale * p } else { max_scale * (1.0 - p) };
    let angle = (opt(option, 2, 0) as f32 * 360.0 * p).to_radians();
    let (s, c) = angle.sin_cos();
    let dx = x - 0.5;
    let dy = y - 0.5;
    let rx = ( c * dx + s * dy) / scale + 0.5;
    let ry = (-s * dx + c * dy) / scale + 0.5;
    let duplicate = opt(option, 3, 0) > 0;
    let gap = opt(option, 4, 0) != 0;
    if duplicate {
        let cells = opt(option, 3, 1).clamp(1, 64) as f32;
        let fx = (rx * cells).fract();
        let fy = (ry * cells).fract();
        if gap && (fx < 0.03 || fy < 0.03 || fx > 0.97 || fy > 0.97) { 1.0 }
        else { sample_mask(mask, fx, fy) }
    } else if !(0.0..=1.0).contains(&rx) || !(0.0..=1.0).contains(&ry) {
        1.0
    } else {
        sample_mask(mask, rx, ry)
    }
}

fn sample_mask(mask: &RgbaImage, x: f32, y: f32) -> f32 {
    if mask.width == 0 || mask.height == 0 { return 1.0; }
    let sx = (x.clamp(0.0, 0.999_999) * mask.width as f32) as u32;
    let sy = (y.clamp(0.0, 0.999_999) * mask.height as f32) as u32;
    let idx = ((sy * mask.width + sx) * 4) as usize;
    let r = mask.rgba.get(idx).copied().unwrap_or(255) as f32;
    let g = mask.rgba.get(idx + 1).copied().unwrap_or(255) as f32;
    let b = mask.rgba.get(idx + 2).copied().unwrap_or(255) as f32;
    let a = mask.rgba.get(idx + 3).copied().unwrap_or(255) as f32;
    ((r * 0.299 + g * 0.587 + b * 0.114) * (a / 255.0) / 255.0).clamp(0.0, 1.0)
}

fn compose_move(current: &RgbaImage, next: &RgbaImage, option: &[i32], p: f32) -> RgbaImage {
    if !same_size(current, next) { return crossfade(current, next, p); }
    let mut out = blank_like(current);
    let dir = opt(option, 0, 0).rem_euclid(4);
    let current_mode = opt(option, 1, 1);
    let next_mode = opt(option, 2, 1);
    let full = RectF::full(current.width, current.height);
    let current_rect = move_rect(full, dir, current_mode, p, false);
    let next_rect = move_rect(full, dir, next_mode, p, true);
    // The original changes draw order when the FRONT side itself moves.  A
    // mode of zero means "draw unchanged"; it is not an implicit crossfade.
    if current_mode == 0 {
        draw_scaled(&mut out, current, current_rect, 1.0);
        draw_scaled(&mut out, next, next_rect, 1.0);
    } else {
        draw_scaled(&mut out, next, next_rect, 1.0);
        draw_scaled(&mut out, current, current_rect, 1.0);
    }
    out
}

fn move_rect(full: RectF, dir: i32, mode: i32, p: f32, incoming: bool) -> RectF {
    if mode == 0 { return full; }
    if mode == 1 {
        let (sx, sy, ex, ey) = match (incoming, dir) {
            (true, 0) => (0.0, -full.h, 0.0, 0.0),
            (true, 1) => (0.0, full.h, 0.0, 0.0),
            (true, 2) => (-full.w, 0.0, 0.0, 0.0),
            (true, _) => (full.w, 0.0, 0.0, 0.0),
            (false, 0) => (0.0, 0.0, 0.0, full.h),
            (false, 1) => (0.0, 0.0, 0.0, -full.h),
            (false, 2) => (0.0, 0.0, full.w, 0.0),
            (false, _) => (0.0, 0.0, -full.w, 0.0),
        };
        return RectF { x: lerp(sx, ex, p), y: lerp(sy, ey, p), ..full };
    }
    if mode == 2 {
        let scale = if incoming { p } else { 1.0 - p };
        return anchored_scale_rect(full, dir, scale, dir <= 1, dir >= 2);
    }
    full
}

fn anchored_scale_rect(full: RectF, dir: i32, scale: f32, scale_y: bool, scale_x: bool) -> RectF {
    let mut r = full;
    if scale_x {
        r.w = full.w * scale.max(0.0001);
        if dir == 3 { r.x = full.w - r.w; }
    }
    if scale_y {
        r.h = full.h * scale.max(0.0001);
        if dir == 1 { r.y = full.h - r.h; }
    }
    r
}

fn compose_scale(
    current: &RgbaImage,
    next: &RgbaImage,
    wipe_type: i32,
    option: &[i32],
    p: f32,
) -> RgbaImage {
    if !same_size(current, next) {
        return crossfade(current, next, p);
    }

    let mut out = blank_like(current);
    let full = RectF::full(current.width, current.height);
    match wipe_type {
        210 => {
            draw_scaled(&mut out, current, full, 1.0);
            let alpha = if opt(option, 1, 0) == 1 { p } else { 1.0 };
            draw_scaled(
                &mut out,
                next,
                scale_destination(full, option, p, current.width, current.height),
                alpha,
            );
        }
        211 => {
            draw_scaled(&mut out, next, full, 1.0);
            let alpha = if opt(option, 1, 0) == 1 { 1.0 - p } else { 1.0 };
            draw_scaled(
                &mut out,
                current,
                scale_destination(full, option, 1.0 - p, current.width, current.height),
                alpha,
            );
        }
        212 => {
            let (src, rate) = if p < 0.5 {
                (current, lerp(1.0, 0.001, p * 2.0))
            } else {
                (next, lerp(0.001, 1.0, (p - 0.5) * 2.0))
            };
            let uv = scale_uv(option, rate, current.width, current.height);
            draw_uv_rect(&mut out, src, full, uv, 1.0);
        }
        213 => {
            draw_scaled(&mut out, current, full, 1.0);
            let rate = lerp(0.333, 1.0, p);
            let uv = scale_uv(option, rate, current.width, current.height);
            draw_uv_rect(&mut out, next, full, uv, p);
        }
        214 => {
            draw_scaled(&mut out, next, full, 1.0);
            let rate = lerp(1.0, 0.333, p);
            let uv = scale_uv(option, rate, current.width, current.height);
            draw_uv_rect(&mut out, current, full, uv, 1.0 - p);
        }
        215 => {
            let specified = corrected_scale_rect(option, current.width, current.height);
            let expand = opt(option, 1, 0) == 0;
            let alpha = match opt(option, 0, 0) {
                1 => p,
                2 => 1.0 - p,
                _ => 1.0,
            };
            if expand {
                draw_scaled(&mut out, current, full, 1.0);
                draw_scaled(&mut out, next, lerp_rect(specified, full, p), alpha);
            } else {
                draw_scaled(&mut out, next, full, 1.0);
                draw_scaled(&mut out, current, lerp_rect(full, specified, p), alpha);
            }
        }
        _ => return crossfade(current, next, p),
    }
    out
}

fn scale_destination(
    full: RectF,
    option: &[i32],
    scale: f32,
    width: u32,
    height: u32,
) -> RectF {
    let w = width as f32;
    let h = height as f32;
    let (anchor_x, anchor_y, scale_x, scale_y) = match opt(option, 0, 0) {
        0 => (w * 0.5, h * 0.5, true, true),
        1 => (0.0, 0.0, true, true),
        2 => (w, 0.0, true, true),
        3 => (0.0, h, true, true),
        4 => (w, h, true, true),
        5 => (w * 0.5, h * 0.5, false, true),
        6 => (w * 0.5, h * 0.5, true, false),
        7 => (0.0, 0.0, false, true),
        8 => (0.0, h, false, true),
        9 => (0.0, 0.0, true, false),
        10 => (w, 0.0, true, false),
        11 => (
            opt(option, 2, 0).clamp(0, width as i32) as f32,
            opt(option, 3, 0).clamp(0, height as i32) as f32,
            true,
            true,
        ),
        _ => (w * 0.5, h * 0.5, true, true),
    };
    let sx = if scale_x { scale.max(0.000_001) } else { 1.0 };
    let sy = if scale_y { scale.max(0.000_001) } else { 1.0 };
    RectF {
        x: anchor_x + (full.x - anchor_x) * sx,
        y: anchor_y + (full.y - anchor_y) * sy,
        w: full.w * sx,
        h: full.h * sy,
    }
}

fn scale_uv(option: &[i32], mut rate: f32, width: u32, height: u32) -> (f32, f32, f32, f32) {
    let mut v_rate = rate;
    let (su, sv, eu, ev) = match opt(option, 0, 0) {
        0 => (0.5 - 0.5 * rate, 0.5 - 0.5 * v_rate, 0.5 + 0.5 * rate, 0.5 + 0.5 * v_rate),
        1 => (0.0, 0.0, rate, v_rate),
        2 => (1.0 - rate, 0.0, 1.0, v_rate),
        3 => (0.0, 1.0 - v_rate, rate, 1.0),
        4 => (1.0 - rate, 1.0 - v_rate, 1.0, 1.0),
        5 => {
            v_rate = lerp(0.49, 1.0, v_rate.clamp(0.0, 1.0));
            (0.0, 1.0 - v_rate, 1.0, v_rate)
        }
        6 => {
            rate = lerp(0.49, 1.0, rate.clamp(0.0, 1.0));
            (1.0 - rate, 0.0, rate, 1.0)
        }
        7 => (0.0, 0.0, 1.0, v_rate),
        8 => (0.0, 1.0 - v_rate, 1.0, 1.0),
        9 => (0.0, 0.0, rate, 1.0),
        10 => (1.0 - rate, 0.0, 1.0, 1.0),
        11 => {
            let x_rate = opt(option, 2, 0).clamp(0, width as i32) as f32 / width.max(1) as f32;
            let y_rate = opt(option, 3, 0).clamp(0, height as i32) as f32 / height.max(1) as f32;
            (
                x_rate - x_rate * rate,
                y_rate - y_rate * v_rate,
                x_rate + (1.0 - x_rate) * rate,
                y_rate + (1.0 - y_rate) * v_rate,
            )
        }
        _ => (0.0, 0.0, 1.0, 1.0),
    };
    (su, sv, eu, ev)
}

fn corrected_scale_rect(option: &[i32], width: u32, height: u32) -> RectF {
    let max_x = width as i32;
    let max_y = height as i32;
    let mut sx = opt(option, 2, 0).clamp(0, max_x);
    let mut sy = opt(option, 3, 0).clamp(0, max_y);
    let mut ex = opt(option, 4, max_x).clamp(0, max_x);
    let mut ey = opt(option, 5, max_y).clamp(0, max_y);
    if sx >= ex {
        if sx == ex {
            if sx + 1 >= max_x {
                sx = (ex - 1).max(0);
            } else {
                ex = sx + 1;
            }
        } else {
            std::mem::swap(&mut sx, &mut ex);
        }
    }
    if sy >= ey {
        if sy == ey {
            if sy + 1 >= max_y {
                sy = (ey - 1).max(0);
            } else {
                ey = sy + 1;
            }
        } else {
            std::mem::swap(&mut sy, &mut ey);
        }
    }
    RectF {
        x: sx as f32,
        y: sy as f32,
        w: (ex - sx).max(1) as f32,
        h: (ey - sy).max(1) as f32,
    }
}

fn draw_uv_rect(
    dst: &mut RgbaImage,
    src: &RgbaImage,
    rect: RectF,
    uv: (f32, f32, f32, f32),
    alpha: f32,
) {
    if rect.w.abs() < 0.001 || rect.h.abs() < 0.001 || alpha <= 0.0 {
        return;
    }
    let min_x = rect.x.floor().max(0.0) as u32;
    let min_y = rect.y.floor().max(0.0) as u32;
    let max_x = (rect.x + rect.w).ceil().min(dst.width as f32).max(0.0) as u32;
    let max_y = (rect.y + rect.h).ceil().min(dst.height as f32).max(0.0) as u32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let tx = ((x as f32 + 0.5 - rect.x) / rect.w).clamp(0.0, 1.0);
            let ty = ((y as f32 + 0.5 - rect.y) / rect.h).clamp(0.0, 1.0);
            let src_px = sample_rgba(src, lerp(uv.0, uv.2, tx), lerp(uv.1, uv.3, ty));
            let idx = ((y * dst.width + x) * 4) as usize;
            alpha_over(&mut dst.rgba[idx..idx + 4], src_px, alpha);
        }
    }
}

#[derive(Clone, Copy)]
struct PageVertex {
    x: f32,
    y: f32,
    z: f32,
    u: f32,
    v: f32,
}

#[derive(Clone, Copy)]
struct ProjectedPageVertex {
    x: f32,
    y: f32,
    depth: f32,
    inv_depth: f32,
    u_over_depth: f32,
    v_over_depth: f32,
}

fn compose_page(
    current: &RgbaImage,
    next: &RgbaImage,
    wipe_type: i32,
    option: &[i32],
    p: f32,
) -> RgbaImage {
    if !same_size(current, next) {
        return crossfade(current, next, p);
    }
    let mut out = blank_like(current);
    let mut depth = vec![f32::INFINITY; (current.width * current.height) as usize];
    match wipe_type {
        300 => {
            // The original draws NEXT first with the "front" rotation branch,
            // then FRONT with the other branch. Back-face culling selects the
            // visible side of the rotating full-screen sheet.
            draw_page_300(&mut out, &mut depth, next, option, p, true);
            draw_page_300(&mut out, &mut depth, current, option, p, false);
        }
        301 => {
            let front_stage_first = p < 0.5;
            let first = if front_stage_first { current } else { next };
            let second = if front_stage_first { next } else { current };
            draw_page_301(&mut out, &mut depth, first, option, p, true);
            draw_page_301(&mut out, &mut depth, second, option, p, false);
        }
        _ => return crossfade(current, next, p),
    }
    out
}

fn range_angle(start: f32, end: f32, range_type: i32, p: f32) -> f32 {
    let half = (start + end) * 0.5;
    match range_type {
        1 => lerp(half, end, p),
        2 => lerp(start, half, p),
        _ => lerp(start, end, p),
    }
}

fn draw_page_300(
    out: &mut RgbaImage,
    depth: &mut [f32],
    src: &RgbaImage,
    option: &[i32],
    p: f32,
    is_front: bool,
) {
    let reverse = opt(option, 0, 0) != 0;
    let (start, end) = if is_front {
        if reverse { (std::f32::consts::PI, 0.0) } else { (std::f32::consts::PI, std::f32::consts::TAU) }
    } else if reverse {
        (0.0, -std::f32::consts::PI)
    } else {
        (0.0, std::f32::consts::PI)
    };
    let angle = range_angle(start, end, opt(option, 2, 0), p);
    let w = src.width as f32;
    let h = src.height as f32;
    let vertices = [
        PageVertex { x: -w * 0.5, y: h * 0.5, z: 0.0, u: 0.0, v: 0.0 },
        PageVertex { x: w * 0.5, y: h * 0.5, z: 0.0, u: 1.0, v: 0.0 },
        PageVertex { x: -w * 0.5, y: -h * 0.5, z: 0.0, u: 0.0, v: 1.0 },
        PageVertex { x: w * 0.5, y: -h * 0.5, z: 0.0, u: 1.0, v: 1.0 },
    ];
    draw_projected_quad(out, depth, src, vertices, angle, view_angle(option));
}

fn draw_page_301(
    out: &mut RgbaImage,
    depth: &mut [f32],
    src: &RgbaImage,
    option: &[i32],
    p: f32,
    is_front: bool,
) {
    let reverse = opt(option, 0, 0) != 0;
    let w = src.width as f32;
    let h = src.height as f32;
    for half in 0..2 {
        let (start, end, z) = match (half, is_front, reverse) {
            (0, true, true) => (0.0, 0.0, 0.0),
            (0, true, false) => (std::f32::consts::PI, std::f32::consts::TAU, -1.0),
            (0, false, true) => (std::f32::consts::TAU, std::f32::consts::PI, -1.0),
            (0, false, false) => (0.0, 0.0, 0.0),
            (1, true, true) => (std::f32::consts::PI, 0.0, -1.0),
            (1, true, false) => (0.0, 0.0, 0.0),
            (1, false, true) => (0.0, 0.0, 0.0),
            (1, false, false) => (0.0, std::f32::consts::PI, -1.0),
            _ => unreachable!(),
        };
        let angle = range_angle(start, end, opt(option, 2, 0), p);
        let (x0, x1, u0, u1) = if half == 0 {
            (-w * 0.5, 0.0, 0.0, 0.5)
        } else {
            (0.0, w * 0.5, 0.5, 1.0)
        };
        let vertices = [
            PageVertex { x: x0, y: h * 0.5, z, u: u0, v: 0.0 },
            PageVertex { x: x1, y: h * 0.5, z, u: u1, v: 0.0 },
            PageVertex { x: x0, y: -h * 0.5, z, u: u0, v: 1.0 },
            PageVertex { x: x1, y: -h * 0.5, z, u: u1, v: 1.0 },
        ];
        draw_projected_quad(out, depth, src, vertices, angle, view_angle(option));
    }
}

fn view_angle(option: &[i32]) -> f32 {
    let tenth_degrees = opt(option, 1, 450);
    (tenth_degrees as f32 / 10.0).clamp(1.0, 179.0).to_radians()
}

fn draw_projected_quad(
    out: &mut RgbaImage,
    depth_buffer: &mut [f32],
    src: &RgbaImage,
    vertices: [PageVertex; 4],
    angle: f32,
    fov: f32,
) {
    let Some(v0) = project_page_vertex(vertices[0], angle, out.width, out.height, fov) else { return; };
    let Some(v1) = project_page_vertex(vertices[1], angle, out.width, out.height, fov) else { return; };
    let Some(v2) = project_page_vertex(vertices[2], angle, out.width, out.height, fov) else { return; };
    let Some(v3) = project_page_vertex(vertices[3], angle, out.width, out.height, fov) else { return; };
    raster_page_triangle(out, depth_buffer, src, [v0, v1, v2]);
    raster_page_triangle(out, depth_buffer, src, [v2, v1, v3]);
}

fn project_page_vertex(
    vertex: PageVertex,
    angle: f32,
    width: u32,
    height: u32,
    fov: f32,
) -> Option<ProjectedPageVertex> {
    let (sin, cos) = angle.sin_cos();
    let x = vertex.x * cos + vertex.z * sin;
    let z = -vertex.x * sin + vertex.z * cos;
    let focal = height.max(1) as f32 * 0.5 / (fov * 0.5).tan();
    let depth = z + focal;
    if depth <= 0.001 {
        return None;
    }
    let inv_depth = 1.0 / depth;
    Some(ProjectedPageVertex {
        x: width as f32 * 0.5 + x * focal * inv_depth,
        y: height as f32 * 0.5 - vertex.y * focal * inv_depth,
        depth,
        inv_depth,
        u_over_depth: vertex.u * inv_depth,
        v_over_depth: vertex.v * inv_depth,
    })
}

fn raster_page_triangle(
    out: &mut RgbaImage,
    depth_buffer: &mut [f32],
    src: &RgbaImage,
    v: [ProjectedPageVertex; 3],
) {
    let area = edge(v[0].x, v[0].y, v[1].x, v[1].y, v[2].x, v[2].y);
    if area <= 0.000_01 {
        return;
    }
    let min_x = v.iter().map(|p| p.x).fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let min_y = v.iter().map(|p| p.y).fold(f32::INFINITY, f32::min).floor().max(0.0) as u32;
    let max_x = v.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max).ceil().min(out.width as f32) as u32;
    let max_y = v.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max).ceil().min(out.height as f32) as u32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let w0 = edge(v[1].x, v[1].y, v[2].x, v[2].y, px, py) / area;
            let w1 = edge(v[2].x, v[2].y, v[0].x, v[0].y, px, py) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < -0.000_01 || w1 < -0.000_01 || w2 < -0.000_01 {
                continue;
            }
            let inv_depth = w0 * v[0].inv_depth + w1 * v[1].inv_depth + w2 * v[2].inv_depth;
            if inv_depth <= 0.0 {
                continue;
            }
            let pixel_depth = 1.0 / inv_depth;
            let index = (y * out.width + x) as usize;
            if pixel_depth > depth_buffer[index] {
                continue;
            }
            let u = (w0 * v[0].u_over_depth + w1 * v[1].u_over_depth + w2 * v[2].u_over_depth) / inv_depth;
            let vv = (w0 * v[0].v_over_depth + w1 * v[1].v_over_depth + w2 * v[2].v_over_depth) / inv_depth;
            let rgba = sample_rgba(src, u, vv);
            let rgba_index = index * 4;
            alpha_over(&mut out.rgba[rgba_index..rgba_index + 4], rgba, 1.0);
            depth_buffer[index] = pixel_depth;
        }
    }
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

fn crossfade(current: &RgbaImage, next: &RgbaImage, p: f32) -> RgbaImage {
    if !same_size(current, next) { return if p < 0.5 { current.clone() } else { next.clone() }; }
    let mut out = blank_like(current);
    for idx in (0..out.rgba.len()).step_by(4) { mix_pixel(&current.rgba, &next.rgba, &mut out.rgba, idx, p); }
    out
}

fn mix_pixel(a: &[u8], b: &[u8], out: &mut [u8], idx: usize, t: f32) {
    let t = t.clamp(0.0, 1.0);
    for c in 0..4 { out[idx + c] = lerp(a[idx + c] as f32, b[idx + c] as f32, t).round().clamp(0.0, 255.0) as u8; }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }

fn lerp_rect(a: RectF, b: RectF, t: f32) -> RectF {
    RectF { x: lerp(a.x, b.x, t), y: lerp(a.y, b.y, t), w: lerp(a.w, b.w, t), h: lerp(a.h, b.h, t) }
}

fn draw_scaled(dst: &mut RgbaImage, src: &RgbaImage, rect: RectF, alpha: f32) {
    if rect.w.abs() < 0.001 || rect.h.abs() < 0.001 || alpha <= 0.0 { return; }
    let min_x = rect.x.floor().max(0.0) as u32;
    let min_y = rect.y.floor().max(0.0) as u32;
    let max_x = (rect.x + rect.w).ceil().min(dst.width as f32).max(0.0) as u32;
    let max_y = (rect.y + rect.h).ceil().min(dst.height as f32).max(0.0) as u32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let u = ((x as f32 + 0.5 - rect.x) / rect.w).clamp(0.0, 0.999_999);
            let v = ((y as f32 + 0.5 - rect.y) / rect.h).clamp(0.0, 0.999_999);
            let src_px = sample_rgba(src, u, v);
            let idx = ((y * dst.width + x) * 4) as usize;
            alpha_over(&mut dst.rgba[idx..idx + 4], src_px, alpha);
        }
    }
}

fn sample_rgba(img: &RgbaImage, u: f32, v: f32) -> [u8; 4] {
    if img.width == 0 || img.height == 0 { return [0; 4]; }
    let x = (u.clamp(0.0, 0.999_999) * img.width as f32) as u32;
    let y = (v.clamp(0.0, 0.999_999) * img.height as f32) as u32;
    let idx = ((y * img.width + x) * 4) as usize;
    [img.rgba[idx], img.rgba[idx + 1], img.rgba[idx + 2], img.rgba[idx + 3]]
}

fn alpha_over(dst: &mut [u8], src: [u8; 4], opacity: f32) {
    let sa = src[3] as f32 / 255.0 * opacity.clamp(0.0, 1.0);
    let da = dst[3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= f32::EPSILON { dst.fill(0); return; }
    for c in 0..3 {
        let sc = src[c] as f32 / 255.0;
        let dc = dst[c] as f32 / 255.0;
        dst[c] = (((sc * sa + dc * da * (1.0 - sa)) / oa) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgba: [u8; 4]) -> RgbaImage {
        RgbaImage { width: 8, height: 8, center_x: 0, center_y: 0, rgba: rgba.repeat(64) }
    }

    #[test]
    fn every_cpu_wipe_has_exact_endpoints() {
        let a = solid([1, 2, 3, 255]);
        let b = solid([200, 201, 202, 255]);
        for ty in [1, 2, 5, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 200, 210, 211, 212, 213, 214, 215, 300, 301, 900, 901] {
            assert_eq!(compose(&a, &b, None, ty, &[], 0.0).rgba, a.rgba, "type {ty}");
            assert_eq!(compose(&a, &b, None, ty, &[], 1.0).rgba, b.rgba, "type {ty}");
        }
    }

    #[test]
    fn direction_and_blind_are_not_plain_alpha_fades() {
        let a = solid([0, 0, 0, 255]);
        let b = solid([255, 255, 255, 255]);
        let dir = compose(&a, &b, None, 100, &[8, 2], 0.5);
        let blind = compose(&a, &b, None, 102, &[8, 2, 2], 0.5);
        assert_ne!(dir.rgba, blind.rgba);
    }
}
