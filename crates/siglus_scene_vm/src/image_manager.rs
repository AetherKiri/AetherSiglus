use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::assets::{load_image_any, RgbaImage};
use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId(pub u32);

impl ImageId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone)]
struct ImageKey {
    path: PathBuf,
    frame_index: usize,
}

impl PartialEq for ImageKey {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.frame_index == other.frame_index
    }
}

impl Eq for ImageKey {}

impl Hash for ImageKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
        self.frame_index.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct G00ComposePart {
    file_name: String,
    x: i32,
    y: i32,
    cut_no: i32,
    blend_type: i32,
}

fn normalized_g00_composite_descriptor(raw: &str) -> String {
    // Original tnm_load_pct_d3d_sub_split_file_name() removes every ASCII
    // space before parsing and before the composed resource is cached.
    raw.chars().filter(|&ch| ch != ' ').collect()
}

fn parse_g00_composite_descriptor(raw: &str) -> Result<Vec<G00ComposePart>> {
    if !raw.contains('|') {
        bail!("not a composed g00 descriptor: {raw}");
    }

    let compact = normalized_g00_composite_descriptor(raw);
    let bytes = compact.as_bytes();
    let mut pos = 0usize;
    let mut parts = Vec::new();

    loop {
        let name_start = pos;
        while pos < bytes.len() && bytes[pos] != b'(' && bytes[pos] != b'|' {
            pos += 1;
        }

        let mut part = G00ComposePart {
            file_name: compact[name_start..pos].to_string(),
            x: 0,
            y: 0,
            cut_no: 0,
            blend_type: 0,
        };

        if pos < bytes.len() && bytes[pos] == b'(' {
            let param_start = pos + 1;
            let Some(rel_close) = compact[param_start..].find(')') else {
                bail!("unterminated composed g00 parameters: {raw}");
            };
            let close = param_start + rel_close;
            let params: Vec<&str> = compact[param_start..close].split(',').collect();
            if params.len() < 2 {
                bail!("composed g00 parameters require x,y: {raw}");
            }
            part.x = params[0]
                .parse::<i32>()
                .with_context(|| format!("invalid composed g00 x in {raw}"))?;
            part.y = params[1]
                .parse::<i32>()
                .with_context(|| format!("invalid composed g00 y in {raw}"))?;
            for param in params.iter().skip(2) {
                if let Some(value) = param.strip_prefix("blend=") {
                    part.blend_type = value
                        .parse::<i32>()
                        .with_context(|| format!("invalid composed g00 blend in {raw}"))?;
                } else {
                    part.cut_no = param
                        .parse::<i32>()
                        .with_context(|| format!("invalid composed g00 cut in {raw}"))?;
                }
            }
            pos = close + 1;
        }

        parts.push(part);
        if pos == bytes.len() {
            break;
        }
        if bytes[pos] != b'|' {
            bail!("unexpected character in composed g00 descriptor: {raw}");
        }
        pos += 1;
        if pos == bytes.len() {
            // The original parser produces an empty final entry, which is then
            // rejected because only the first composed entry may omit a file.
            parts.push(G00ComposePart {
                file_name: String::new(),
                x: 0,
                y: 0,
                cut_no: 0,
                blend_type: 0,
            });
            break;
        }
    }

    if parts.is_empty() {
        bail!("empty composed g00 descriptor");
    }
    for (index, part) in parts.iter().enumerate().skip(1) {
        if part.file_name.is_empty() {
            bail!("composed g00 entry {index} has no file name");
        }
    }
    Ok(parts)
}

pub(crate) fn g00_composite_component_names(raw: &str) -> Option<Vec<String>> {
    if !raw.contains('|') {
        return None;
    }
    parse_g00_composite_descriptor(raw).ok().map(|parts| {
        parts
            .into_iter()
            .filter_map(|part| (!part.file_name.is_empty()).then_some(part.file_name))
            .collect()
    })
}

#[derive(Debug)]
pub struct ImageManager {
    project_dir: PathBuf,
    current_append_dir: String,
    key_to_id: HashMap<ImageKey, ImageId>,
    /// Original Tona3 keeps one C_d3d_album per resolved G00 resource.  Keep
    /// the complete cut -> ImageId table alive for the same resource lifetime
    /// so PATNO/GAN changes never decode the file again.
    g00_album_to_ids: HashMap<PathBuf, Vec<ImageId>>,
    /// CG delta cuts are paired with an extracted `__base.g00` canvas. Keep
    /// the synthetic image keyed by the original path/frame so raw album
    /// entries are never composed more than once.
    cg_composite_to_ids: HashMap<ImageKey, ImageId>,
    composite_to_id: HashMap<(String, String), ImageId>,
    solid_to_id: HashMap<(u8, u8, u8, u8), ImageId>,
    images: Vec<ImageEntry>,
    access_clock: std::cell::Cell<u64>,
    resident_bytes: usize,
    pub cache_budget_bytes: usize,
}

#[derive(Debug, Clone)]
struct ImageEntry {
    img: Option<Arc<RgbaImage>>,
    version: u64,
    last_used: std::cell::Cell<u64>,
}

#[derive(Debug, Clone)]
pub struct DebugImageInfo {
    pub id: ImageId,
    pub width: u32,
    pub height: u32,
    pub version: u64,
    pub source_path: Option<PathBuf>,
    pub frame_index: Option<usize>,
    pub composite_append_dir: Option<String>,
    pub composite_descriptor: Option<String>,
}

fn compose_g00_cut(dst: &mut RgbaImage, src: &RgbaImage, x: i32, y: i32, blend_type: i32) {
    if dst.width == 0 || dst.height == 0 || src.width == 0 || src.height == 0 {
        return;
    }

    let dst_left = x.max(0) as u32;
    let dst_top = y.max(0) as u32;
    let src_left = x.saturating_neg().max(0) as u32;
    let src_top = y.saturating_neg().max(0) as u32;
    if dst_left >= dst.width || dst_top >= dst.height || src_left >= src.width || src_top >= src.height {
        return;
    }
    let width = (src.width - src_left).min(dst.width - dst_left);
    let height = (src.height - src_top).min(dst.height - dst_top);

    for row in 0..height {
        let si = (((src_top + row) * src.width + src_left) * 4) as usize;
        let di = (((dst_top + row) * dst.width + dst_left) * 4) as usize;
        let len = width as usize * 4;
        let src_row = &src.rgba[si..si + len];
        let dst_row = &mut dst.rgba[di..di + len];
        #[cfg(target_arch = "x86_64")]
        if !matches!(blend_type, 1 | 3) {
            // SSE2 is guaranteed on x86_64. Only exact transparent/opaque
            // pixels are vectorized; partial alpha keeps Tona3's integer math.
            unsafe { compose_g00_normal_row_sse2(dst_row, src_row); }
            continue;
        }
        for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
            compose_g00_pixel(dst, src, blend_type);
        }
    }
}

fn compose_g00_pixel(dst: &mut [u8], src: &[u8], blend_type: i32) {
    let sa = src[3] as i64;
    if sa == 0 { return; }
    let da = dst[3] as i64;
    // Opaque add/multiply must still combine source and destination colors.
    if da == 0 || (sa == 255 && !matches!(blend_type, 1 | 3)) {
        dst.copy_from_slice(src);
        return;
    }
    let ra = sa + da - sa * da / 255;
    if ra <= 0 { return; }
    for c in 0..3 {
        let sc = src[c] as i64;
        let dc = dst[c] as i64;
        let color = match blend_type {
            1 | 3 => {
                let mixed = if blend_type == 1 { (sc + dc).min(255) } else { sc * dc / 255 };
                (sa * da * mixed + sa * (255 - da) * sc + (255 - sa) * da * dc) / ra / 255
            }
            _ => ((255 * sa * sc + (255 - sa) * da * dc) >> 8) / ra,
        };
        dst[c] = color.clamp(0, 255) as u8;
    }
    dst[3] = ra.clamp(0, 255) as u8;
}

fn cg_base_path(path: &Path) -> Option<PathBuf> {
    if !path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("g00"))
    {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let lower = stem.to_ascii_lowercase();
    if !lower.starts_with("cg_") || lower.ends_with("__base") {
        return None;
    }
    Some(path.with_file_name(format!("{stem}__base.g00")))
}

fn compose_cg_base_delta_image(base: &RgbaImage, delta: &RgbaImage) -> RgbaImage {
    let mut composed = base.clone();
    let dst_x = composed.center_x.saturating_sub(delta.center_x);
    let dst_y = composed.center_y.saturating_sub(delta.center_y);
    compose_g00_cut(&mut composed, delta, dst_x, dst_y, 0);
    composed
}

#[cfg(target_arch = "x86_64")]
unsafe fn compose_g00_normal_row_sse2(dst: &mut [u8], src: &[u8]) {
    use std::arch::x86_64::*;
    debug_assert_eq!(dst.len(), src.len());
    let alpha_mask = _mm_set1_epi32(0xff000000u32 as i32);
    let zero = _mm_setzero_si128();
    let end = src.len() / 16 * 16;
    let mut offset = 0;
    while offset < end {
        let source = _mm_loadu_si128(src.as_ptr().add(offset).cast());
        let alpha = _mm_and_si128(source, alpha_mask);
        let opaque = _mm_cmpeq_epi32(alpha, alpha_mask);
        let transparent = _mm_cmpeq_epi32(alpha, zero);
        if _mm_movemask_epi8(_mm_or_si128(opaque, transparent)) == 0xffff {
            let ptr = dst.as_mut_ptr().add(offset).cast();
            let old = _mm_loadu_si128(ptr);
            let pixels = _mm_or_si128(_mm_and_si128(source, opaque), _mm_andnot_si128(opaque, old));
            _mm_storeu_si128(ptr, pixels);
        } else {
            for pixel in (offset..offset + 16).step_by(4) {
                compose_g00_pixel(&mut dst[pixel..pixel + 4], &src[pixel..pixel + 4], 0);
            }
        }
        offset += 16;
    }
    for pixel in (offset..src.len()).step_by(4) {
        compose_g00_pixel(&mut dst[pixel..pixel + 4], &src[pixel..pixel + 4], 0);
    }
}

impl ImageManager {
    pub fn new(project_dir: PathBuf) -> Self {
        Self {
            project_dir,
            current_append_dir: String::new(),
            key_to_id: HashMap::new(),
            g00_album_to_ids: HashMap::new(),
            cg_composite_to_ids: HashMap::new(),
            composite_to_id: HashMap::new(),
            solid_to_id: HashMap::new(),
            images: Vec::new(),
            access_clock: std::cell::Cell::new(0),
            resident_bytes: 0,
            cache_budget_bytes: 256 * 1024 * 1024,
        }
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn current_append_dir(&self) -> &str {
        &self.current_append_dir
    }

    pub fn set_current_append_dir(&mut self, append_dir: impl Into<String>) {
        let append_dir = append_dir.into();
        if self.current_append_dir != append_dir {
            self.current_append_dir = append_dir;
        }
    }

    pub fn set_current_append_dir_ref(&mut self, append_dir: &str) {
        if self.current_append_dir != append_dir {
            self.current_append_dir.clear();
            self.current_append_dir.push_str(append_dir);
        }
    }

    pub fn get(&self, id: ImageId) -> Option<&Arc<RgbaImage>> {
        let entry = self.images.get(id.index())?;
        let img = entry.img.as_ref()?;
        self.access_clock.set(self.access_clock.get().wrapping_add(1));
        entry.last_used.set(self.access_clock.get());
        Some(img)
    }

    pub fn get_entry(&self, id: ImageId) -> Option<(&Arc<RgbaImage>, u64)> {
        Some((self.get(id)?, self.images[id.index()].version))
    }

    /// Create a 1x1 solid RGBA image and return its image id.
    ///
    /// This is used for UI placeholders (e.g. message window background) until
    /// full UI skinning is implemented.
    pub fn solid_rgba(&mut self, rgba: (u8, u8, u8, u8)) -> ImageId {
        if let Some(id) = self.solid_to_id.get(&rgba) {
            return *id;
        }
        let img = RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![rgba.0, rgba.1, rgba.2, rgba.3],
        };
        let id = self.insert_image(img);
        self.solid_to_id.insert(rgba, id);
        id
    }

    /// Load a BG resource by name (Siglus policy: g00/ then bg/, with extension fallback).
    ///
    /// BG is not animated in our current bring-up, so frame index is always 0.
    pub fn load_bg(&mut self, name: &str) -> Result<ImageId> {
        let (path, _ty) = crate::resource::find_bg_image_with_append_dir(
            &self.project_dir,
            &self.current_append_dir,
            name,
        )
        .with_context(|| format!("find bg resource {name}"))?;
        self.load_file(&path, 0)
    }

    /// Load a BG resource with an explicit frame index (kept for compatibility).
    pub fn load_bg_frame(&mut self, name: &str, frame_index: usize) -> Result<ImageId> {
        let (path, _ty) = crate::resource::find_bg_image_with_append_dir(
            &self.project_dir,
            &self.current_append_dir,
            name,
        )
        .with_context(|| format!("find bg resource {name}"))?;
        self.load_file(&path, frame_index)
    }

    /// Load an image restricted to the `g00/` directory (with extension fallback).
    ///
    /// Used for CHR / sprite image loading.
    pub fn load_g00(&mut self, name: &str, frame_index: u32) -> Result<ImageId> {
        if name.contains('|') {
            if frame_index != 0 {
                bail!("composed g00 has one texture; invalid frame index {frame_index}");
            }
            return self.load_g00_composed(name);
        }
        let (path, _ty) = crate::resource::find_g00_image_with_append_dir(
            &self.project_dir,
            &self.current_append_dir,
            name,
        )
        .with_context(|| format!("find g00 resource {name}"))?;
        self.load_file(&path, frame_index as usize)
    }

    /// Extracted Siglus CGs commonly store a full `cg_*__base.g00` canvas and
    /// one or more sparse `cg_*.g00` display-rectangle deltas. The original
    /// renderer composites the delta onto the base before presenting it; do
    /// the same for direct G00 loads and save-state rehydration.
    fn compose_cg_base_delta(
        &mut self,
        resolved: &Path,
        frame_index: usize,
        delta_id: ImageId,
    ) -> Result<ImageId> {
        let Some(base_path) = cg_base_path(resolved) else {
            return Ok(delta_id);
        };
        let Some(base_path) = crate::resource::resolve_game_file(&base_path)? else {
            return Ok(delta_id);
        };

        let key = ImageKey {
            path: resolved.to_path_buf(),
            frame_index,
        };
        if let Some(id) = self.cg_composite_to_ids.get(&key) {
            return Ok(*id);
        }

        let delta = self
            .get(delta_id)
            .cloned()
            .with_context(|| format!("missing CG delta image id={}", delta_id.index()))?;
        let base_id = self.load_file(&base_path, 0)?;
        let base = self
            .get(base_id)
            .cloned()
            .with_context(|| format!("missing CG base image id={}", base_id.index()))?;

        let composed = compose_cg_base_delta_image(&base, &delta);
        let composed_id = self.insert_image(composed);
        self.cg_composite_to_ids.insert(key.clone(), composed_id);
        // All later callers (including paths that use load_file directly)
        // should observe the fully composed image rather than the sparse cut.
        self.key_to_id.insert(key, composed_id);
        Ok(composed_id)
    }

    fn decode_composed_g00_part(&mut self, part: &G00ComposePart) -> Result<Arc<RgbaImage>> {
        let (path, ty) = crate::resource::find_g00_image_with_append_dir(
            &self.project_dir,
            &self.current_append_dir,
            &part.file_name,
        )
        .with_context(|| format!("find composed g00 resource {}", part.file_name))?;
        if ty != crate::resource::PctType::G00 {
            bail!(
                "composed texture accepts g00 only: {} resolved as {}",
                part.file_name,
                ty.ext()
            );
        }

        let requested = if path.is_absolute() {
            path
        } else if crate::resource::resolve_game_file(&path)?.is_some() {
            path
        } else {
            self.project_dir.join(path)
        };
        let resolved = crate::resource::resolve_game_file(&requested)?
            .unwrap_or(requested);

        // Tona3 composes cuts from an already-loaded C_d3d_album. Preserve the
        // original clamp-to-last-cut behavior while reusing that same album
        // cache instead of decoding the G00 again for every component.
        let album = self.ensure_g00_album(&resolved)?;
        let max_index = album.len() - 1;
        let cut_no = part.cut_no.clamp(0, max_index as i32) as usize;
        let id = album[cut_no];
        self.get(id)
            .cloned()
            .with_context(|| format!("missing cached composed g00 image id={}", id.index()))
    }

    /// Load Siglus/Tona3's composed-G00 descriptor syntax:
    /// `base(x,y,cut,blend=n)|overlay(x,y,cut,blend=n)|...`.
    ///
    /// Tona3 creates one texture from the first cut and draws every later cut
    /// into that fixed-size texture. Coordinates are anchor-relative: each
    /// overlay is shifted by the base cut center minus the overlay cut center.
    pub fn load_g00_composed(&mut self, descriptor: &str) -> Result<ImageId> {
        let _perf = crate::perf_trace::Span::new("image.compose");
        let normalized = normalized_g00_composite_descriptor(descriptor);
        let cache_key = (self.current_append_dir.clone(), normalized.clone());
        if let Some(id) = self.composite_to_id.get(&cache_key) {
            return Ok(*id);
        }

        let parts = parse_g00_composite_descriptor(&normalized)?;
        let first = parts.first().context("composed g00 has no first entry")?;
        let mut composed = if first.file_name.is_empty() {
            if first.x <= 0 || first.y <= 0 {
                bail!("blank composed g00 base requires positive width,height");
            }
            let pixel_len = (first.x as usize)
                .checked_mul(first.y as usize)
                .and_then(|len| len.checked_mul(4))
                .context("blank composed g00 size overflow")?;
            RgbaImage {
                width: first.x as u32,
                height: first.y as u32,
                center_x: 0,
                center_y: 0,
                rgba: vec![0; pixel_len],
            }
        } else {
            // Only the writable base needs a pixel copy. Overlay cuts remain
            // shared with the album cache throughout the blend.
            (*self.decode_composed_g00_part(first)?).clone()
        };

        let base_center_x = composed.center_x;
        let base_center_y = composed.center_y;
        for part in parts.iter().skip(1) {
            let overlay = self.decode_composed_g00_part(part)?;
            let dst_x = part
                .x
                .saturating_add(base_center_x)
                .saturating_sub(overlay.center_x);
            let dst_y = part
                .y
                .saturating_add(base_center_y)
                .saturating_sub(overlay.center_y);
            compose_g00_cut(&mut composed, &overlay, dst_x, dst_y, part.blend_type);
        }

        let id = self.insert_image(composed);
        self.composite_to_id.insert(cache_key, id);
        Ok(id)
    }

    fn ensure_g00_album(&mut self, resolved: &Path) -> Result<&[ImageId]> {
        if !self.g00_album_to_ids.contains_key(resolved) {
            let _perf = crate::perf_trace::Span::new("image.g00_album");
            if crate::perf_trace::enabled() {
                eprintln!("[SG_PERF_IMAGE] loading={resolved:?}");
            }
            let bytes = crate::resource::read_file_bytes(resolved)
                .with_context(|| format!("read g00 album {:?}", resolved))?;
            let decoded = crate::assets::g00::decode_g00(&bytes)
                .with_context(|| format!("decode g00 album {:?}", resolved))?;
            if decoded.frames.is_empty() {
                bail!("g00 has no frames: {:?}", resolved);
            }

            let mut ids = Vec::with_capacity(decoded.frames.len());
            for (cut_index, img) in decoded.frames.into_iter().enumerate() {
                let id = self.insert_image(img);
                self.key_to_id.insert(
                    ImageKey {
                        path: resolved.to_path_buf(),
                        frame_index: cut_index,
                    },
                    id,
                );
                ids.push(id);
            }
            self.g00_album_to_ids.insert(resolved.to_path_buf(), ids);
        }

        self.g00_album_to_ids
            .get(resolved)
            .map(Vec::as_slice)
            .context("G00 album cache insertion failed")
    }

    /// Load an image from an explicit path (relative to project_dir if not absolute).
    pub fn load_file(&mut self, path: &Path, frame_index: usize) -> Result<ImageId> {
        let requested = if path.is_absolute() {
            path.to_path_buf()
        } else if crate::resource::resolve_game_file(path)?.is_some() {
            // Resource lookup helpers can return a project-rooted relative path
            // (for example `testcase/g00/foo.g00`). Resolve it before deciding
            // to join project_dir again, including Windows-style case folding.
            path.to_path_buf()
        } else {
            self.project_dir.join(path)
        };
        let resolved = crate::resource::resolve_game_file(&requested)?
            .unwrap_or(requested);

        let key = ImageKey {
            path: resolved.clone(),
            frame_index,
        };

        let ext = resolved
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if ext == "g00" {
            if let Some(id) = self.cg_composite_to_ids.get(&key) {
                return Ok(*id);
            }
        }

        if let Some(id) = self.key_to_id.get(&key) {
            if ext == "g00" {
                return self.compose_cg_base_delta(&resolved, frame_index, *id);
            }
            return Ok(*id);
        }

        if ext == "g00" {
            // C_tnm_d3d_resource_manager::create_album_from_g00() loads and
            // caches the complete album once. GAN then only changes PATNO.
            // Do the same here: the first requested cut decodes the G00 once
            // and registers every cut; all later PATNO changes are O(1).
            let album = self.ensure_g00_album(&resolved)?.to_vec();
            let delta_id = album.get(frame_index).copied().with_context(|| {
                format!(
                    "g00 frame index out of range: {:?} index={} count={}",
                    resolved,
                    frame_index,
                    album.len()
                )
            })?;
            return self.compose_cg_base_delta(&resolved, frame_index, delta_id);
        }

        let img = load_image_any(&resolved, frame_index)
            .with_context(|| format!("load image {:?}", resolved))?;
        let id = self.insert_image(img);
        self.key_to_id.insert(key, id);
        Ok(id)
    }

    /// Insert an already-decoded image into the manager and return a new ImageId.
    pub fn insert_image(&mut self, img: RgbaImage) -> ImageId {
        self.insert_image_arc(Arc::new(img))
    }

    pub fn insert_image_arc(&mut self, img: Arc<RgbaImage>) -> ImageId {
        let id = ImageId(self.images.len() as u32);
        self.resident_bytes = self.resident_bytes.saturating_add(img.rgba.len());
        self.images.push(ImageEntry { img: Some(img), version: 0,
            last_used: std::cell::Cell::new(self.access_clock.get()) });
        id
    }

    /// Replace an existing image in-place and bump its version.
    ///
    /// This allows the renderer to update the GPU texture without changing the ImageId.
    pub fn replace_image(&mut self, id: ImageId, img: RgbaImage) -> Result<()> {
        let Some(entry) = self.images.get_mut(id.index()) else {
            anyhow::bail!("replace_image: invalid ImageId {}", id.index());
        };
        self.resident_bytes = self.resident_bytes.saturating_sub(entry.img.as_ref().map(|i| i.rgba.len()).unwrap_or(0))
            .saturating_add(img.rgba.len());
        entry.img = Some(Arc::new(img));
        entry.version = entry.version.wrapping_add(1);
        Ok(())
    }

    pub fn replace_image_arc(&mut self, id: ImageId, img: Arc<RgbaImage>) -> Result<()> {
        let Some(entry) = self.images.get_mut(id.index()) else {
            anyhow::bail!("replace_image_arc: invalid ImageId {}", id.index());
        };
        self.resident_bytes = self.resident_bytes.saturating_sub(entry.img.as_ref().map(|i| i.rgba.len()).unwrap_or(0))
            .saturating_add(img.rgba.len());
        entry.img = Some(img);
        entry.version = entry.version.wrapping_add(1);
        Ok(())
    }

    pub fn debug_image_info(&self, id: ImageId) -> Option<DebugImageInfo> {
        let entry = self.images.get(id.index())?;
        let img = entry.img.as_ref()?;
        let mut source_path = None;
        let mut frame_index = None;
        for (key, key_id) in &self.key_to_id {
            if *key_id == id {
                source_path = Some(key.path.clone());
                frame_index = Some(key.frame_index);
                break;
            }
        }

        // Composed G00 textures are synthetic ImageIds and therefore do not
        // appear in key_to_id. Keep their original descriptor visible to the
        // renderer HUD so a bad composed texture can be distinguished from a
        // correctly decoded face/eye difference layer.
        let mut composite_append_dir = None;
        let mut composite_descriptor = None;
        for ((append_dir, descriptor), composite_id) in &self.composite_to_id {
            if *composite_id == id {
                composite_append_dir = Some(append_dir.clone());
                composite_descriptor = Some(descriptor.clone());
                break;
            }
        }

        Some(DebugImageInfo {
            id,
            width: img.width,
            height: img.height,
            version: entry.version,
            source_path,
            frame_index,
            composite_append_dir,
            composite_descriptor,
        })
    }

    pub fn resident_bytes(&self) -> usize { self.resident_bytes }

    pub fn pin_live_albums(&self, live: &mut std::collections::HashSet<ImageId>) {
        for album in self.g00_album_to_ids.values() {
            if album.iter().any(|id| live.contains(id)) { live.extend(album.iter().copied()); }
        }
    }

    /// Evict reconstructible, unowned assets. IDs become tombstones and are
    /// never reused. A live cut pins its entire album, preventing GAN churn.
    pub fn collect_cached_assets(&mut self, roots: &std::collections::HashSet<ImageId>) -> usize {
        use std::collections::HashSet;
        if self.resident_bytes <= self.cache_budget_bytes { return 0; }
        let mut live = roots.clone();
        for (index, entry) in self.images.iter().enumerate() {
            if entry.img.as_ref().is_some_and(|img| Arc::strong_count(img) > 1) {
                live.insert(ImageId(index as u32));
            }
        }
        let mut covered = HashSet::new();
        let mut groups = Vec::new();
        for album in self.g00_album_to_ids.values() {
            covered.extend(album.iter().copied());
            if !album.iter().any(|id| live.contains(id)) { groups.push(album.clone()); }
        }
        for id in self.key_to_id.values().chain(self.composite_to_id.values()).copied() {
            if !live.contains(&id) && covered.insert(id) { groups.push(vec![id]); }
        }
        groups.sort_unstable_by_key(|ids| ids.iter().filter_map(|id| self.images.get(id.index()))
            .map(|entry| entry.last_used.get()).max().unwrap_or(0));
        let mut evicted = HashSet::new();
        for group in groups {
            if self.resident_bytes <= self.cache_budget_bytes { break; }
            for id in group {
                if let Some(img) = self.images[id.index()].img.take() {
                    self.resident_bytes = self.resident_bytes.saturating_sub(img.rgba.len());
                    evicted.insert(id);
                }
            }
        }
        self.key_to_id.retain(|_, id| !evicted.contains(id));
        self.cg_composite_to_ids.retain(|_, id| !evicted.contains(id));
        self.composite_to_id.retain(|_, id| !evicted.contains(id));
        self.g00_album_to_ids.retain(|_, ids| !ids.iter().any(|id| evicted.contains(id)));
        evicted.len()
    }
}

#[cfg(test)]
mod composed_g00_tests {
    use super::*;

    fn pixel() -> RgbaImage { RgbaImage { width: 1, height: 1, center_x: 0, center_y: 0, rgba: vec![255; 4] } }

    #[test]
    fn cache_budget_evicts_cold_albums_atomically_but_keeps_live_animation() {
        let mut images = ImageManager::new(PathBuf::from("."));
        images.cache_budget_bytes = 8;
        let mut albums = Vec::new();
        for name in ["live.g00", "cold.g00"] {
            let path = PathBuf::from(name);
            let ids: Vec<_> = (0..2).map(|_| images.insert_image(pixel())).collect();
            for (frame_index, id) in ids.iter().enumerate() {
                images.key_to_id.insert(ImageKey { path: path.clone(), frame_index }, *id);
            }
            images.g00_album_to_ids.insert(path, ids.clone());
            albums.push(ids);
        }
        let roots = [albums[0][0]].into_iter().collect();
        assert_eq!(images.collect_cached_assets(&roots), 2);
        assert_eq!(images.resident_bytes(), 8);
        assert!(albums[0].iter().all(|id| images.get(*id).is_some()));
        assert!(albums[1].iter().all(|id| images.get(*id).is_none()));
        assert!(!images.g00_album_to_ids.contains_key(&PathBuf::from("cold.g00")));
        assert!(images.key_to_id.values().all(|id| albums[0].contains(id)));
        let next = images.insert_image(pixel());
        assert!(next.0 > albums[1][1].0, "evicted IDs must never alias new pixels");
    }

    #[test]
    fn cache_keeps_external_owners_and_non_reconstructible_images() {
        let mut images = ImageManager::new(PathBuf::from("."));
        images.cache_budget_bytes = 0;
        let generated = images.insert_image(pixel());
        let held = images.insert_image(pixel());
        images.key_to_id.insert(ImageKey { path: PathBuf::from("held.png"), frame_index: 0 }, held);
        let owner = images.get(held).unwrap().clone();
        assert_eq!(images.collect_cached_assets(&Default::default()), 0);
        drop(owner);
        assert_eq!(images.collect_cached_assets(&Default::default()), 1);
        assert!(images.get(generated).is_some());
        assert_eq!(images.resident_bytes(), 4);
    }

    #[test]
    fn debug_info_reports_composed_descriptor_origin() {
        let mut images = ImageManager::new(PathBuf::from("."));
        let id = images.insert_image(RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![255, 255, 255, 255],
        });
        images.composite_to_id.insert(
            ("PDT".to_string(), "base(0,0,0)|face(0,0,2)".to_string()),
            id,
        );

        let info = images.debug_image_info(id).expect("debug image info");
        assert_eq!(info.composite_append_dir.as_deref(), Some("PDT"));
        assert_eq!(
            info.composite_descriptor.as_deref(),
            Some("base(0,0,0)|face(0,0,2)")
        );
        assert!(info.source_path.is_none());
        assert!(info.frame_index.is_none());
    }

    #[test]
    fn parses_siglus_composed_descriptor() {
        let parts = parse_g00_composite_descriptor(
            " bs3_rk2_base41(0, 0, 0) | bs3_rk2_face001(12, -3, 4, blend=1) ",
        )
        .expect("composed descriptor");
        assert_eq!(
            parts,
            vec![
                G00ComposePart {
                    file_name: "bs3_rk2_base41".to_string(),
                    x: 0,
                    y: 0,
                    cut_no: 0,
                    blend_type: 0,
                },
                G00ComposePart {
                    file_name: "bs3_rk2_face001".to_string(),
                    x: 12,
                    y: -3,
                    cut_no: 4,
                    blend_type: 1,
                },
            ]
        );
    }

    #[test]
    fn composed_cut_uses_base_and_overlay_centers() {
        let mut base = RgbaImage {
            width: 4,
            height: 4,
            center_x: 2,
            center_y: 2,
            rgba: vec![0; 4 * 4 * 4],
        };
        let overlay = RgbaImage {
            width: 2,
            height: 2,
            center_x: 1,
            center_y: 1,
            rgba: vec![255; 2 * 2 * 4],
        };

        // Tona3 draw position for (0,0) is base_center-overlay_center=(1,1).
        compose_g00_cut(&mut base, &overlay, 1, 1, 0);
        for y in 0..4usize {
            for x in 0..4usize {
                let alpha = base.rgba[(y * 4 + x) * 4 + 3];
                assert_eq!(alpha, if (1..3).contains(&x) && (1..3).contains(&y) { 255 } else { 0 });
            }
        }
        assert_eq!((base.center_x, base.center_y), (2, 2));
    }

    #[test]
    fn cg_delta_is_restored_onto_full_base_canvas() {
        let base = RgbaImage {
            width: 4,
            height: 3,
            center_x: 0,
            center_y: 0,
            rgba: vec![0; 4 * 3 * 4],
        };
        let delta = RgbaImage {
            width: 1,
            height: 1,
            center_x: -2,
            center_y: -1,
            rgba: vec![255, 0, 0, 255],
        };

        let composed = compose_cg_base_delta_image(&base, &delta);
        assert_eq!((composed.width, composed.height), (4, 3));
        assert_eq!((composed.center_x, composed.center_y), (0, 0));
        assert_eq!(&composed.rgba[(1 * 4 + 2) * 4..(1 * 4 + 3) * 4], &[255, 0, 0, 255]);
        assert_eq!(composed.rgba.iter().filter(|&&value| value != 0).count(), 2);
    }

    #[test]
    fn cg_base_path_only_matches_delta_g00_names() {
        assert_eq!(
            cg_base_path(Path::new("g00/cg_sr09_0101.g00")),
            Some(PathBuf::from("g00/cg_sr09_0101__base.g00"))
        );
        assert!(cg_base_path(Path::new("g00/CG_SR09_0101__BASE.G00")).is_none());
        assert!(cg_base_path(Path::new("g00/chr_0101.g00")).is_none());
        assert!(cg_base_path(Path::new("g00/cg_sr09_0101.png")).is_none());
    }

    #[test]
    fn opaque_add_source_still_uses_tona_add_equation() {
        let mut base = RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![20, 40, 60, 128],
        };
        let overlay = RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![200, 100, 50, 255],
        };
        compose_g00_cut(&mut base, &overlay, 0, 0, 1);

        let expected = |sc: i64, dc: i64| {
            let sa = 255i64;
            let da = 128i64;
            let ra = 255i64;
            ((sa * da * (sc + dc).min(255)
                + sa * (255 - da) * sc
                + (255 - sa) * da * dc)
                / ra
                / 255) as u8
        };
        assert_eq!(
            base.rgba,
            vec![
                expected(200, 20),
                expected(100, 40),
                expected(50, 60),
                255,
            ]
        );
    }

    #[test]
    fn composed_alpha_matches_tona_integer_equation() {
        let mut base = RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![20, 40, 60, 128],
        };
        let overlay = RgbaImage {
            width: 1,
            height: 1,
            center_x: 0,
            center_y: 0,
            rgba: vec![200, 100, 50, 128],
        };
        compose_g00_cut(&mut base, &overlay, 0, 0, 0);

        let sa = 128i64;
        let da = 128i64;
        let ra = sa + da - sa * da / 255;
        let expected = |sc: i64, dc: i64| {
            ((((255 * sa * sc) + ((255 - sa) * da * dc)) >> 8) / ra) as u8
        };
        assert_eq!(
            base.rgba,
            vec![
                expected(200, 20),
                expected(100, 40),
                expected(50, 60),
                ra as u8,
            ]
        );
    }

    #[test]
    fn composed_cut_fast_rows_match_scalar_across_alpha_modes_and_clipping() {
        for width in [1, 3, 4, 5, 17] {
            for alpha_mode in 0..3 {
                let make = |salt: usize| RgbaImage {
                    width, height: 5, center_x: 0, center_y: 0,
                    rgba: (0..width as usize * 5 * 4).map(|i| {
                        if i % 4 == 3 {
                            match alpha_mode { 0 => 255, 1 => [0, 255][(i / 4 + salt) % 2],
                                _ => [0, 1, 127, 254, 255][(i / 4 + salt) % 5] }
                        } else { (i * 71 + salt * 43) as u8 }
                    }).collect(),
                };
                let source = make(2);
                for (x, y) in [(-2, -1), (0, 0), (1, 2), (20, 0)] {
                    for blend in [0, 1, 2, 3, -1] {
                        let mut actual = make(1);
                        let mut expected = actual.clone();
                        for sy in 0..5i32 {
                            for sx in 0..width as i32 {
                                let (dx, dy) = (sx + x, sy + y);
                                if dx < 0 || dy < 0 || dx >= width as i32 || dy >= 5 { continue; }
                                let si = (sy as usize * width as usize + sx as usize) * 4;
                                let di = (dy as usize * width as usize + dx as usize) * 4;
                                compose_g00_pixel(&mut expected.rgba[di..di + 4], &source.rgba[si..si + 4], blend);
                            }
                        }
                        compose_g00_cut(&mut actual, &source, x, y, blend);
                        assert_eq!(actual.rgba, expected.rgba,
                            "width={width} alpha={alpha_mode} offset=({x},{y}) blend={blend}");
                    }
                }
            }
        }
    }
}
