pub mod g00;

use anyhow::{bail, Context, Result};
use std::path::Path;

/// A decoded RGBA image.
#[derive(Clone, Debug)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// Image-space center metadata from formats that carry it (notably G00 cuts).
    /// Siglus OBJECT/PCT rendering applies this in the object render path only;
    /// helper textures keep this at zero and remain top-left positioned.
    pub center_x: i32,
    pub center_y: i32,
    /// length = width * height * 4
    pub rgba: Vec<u8>,
}

/// Load an image from disk.
///
/// Supported:
/// - .g00 (decoded by our g00 decoder)
/// - .png/.jpg/.bmp (decoded by `image` crate)
///
/// DDS is detected but not decoded in this stage.
pub fn load_image_any(path: &Path, g00_frame_index: usize) -> Result<RgbaImage> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "g00" => {
            let bytes = crate::resource::read_file_bytes(path).with_context(|| format!("read {:?}", path))?;
            let decoded =
                g00::decode_g00(&bytes).with_context(|| format!("decode g00 {:?}", path))?;
            if decoded.frames.is_empty() {
                bail!("g00 has no frames: {:?}", path);
            }
            if g00_frame_index >= decoded.frames.len() {
                bail!(
                    "g00 frame index out of range: {:?} index={} count={}",
                    path,
                    g00_frame_index,
                    decoded.frames.len()
                );
            }
            Ok(decoded.frames[g00_frame_index].clone())
        }
        "png" | "jpg" | "jpeg" | "bmp" | "dds" => {
            #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
            let img = {
                let bytes = crate::resource::read_file_bytes(path)
                    .with_context(|| format!("read image {:?}", path))?;
                image::load_from_memory(&bytes).with_context(|| format!("decode image {:?}", path))?
            };
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            let img = image::open(path).with_context(|| format!("decode image {:?}", path))?;
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let mut raw = rgba.into_raw();
            // Siglus writes save thumbnails as 32bpp BMPs copied straight from
            // the opaque capture surface, so the unused 4th byte stays 0 for
            // every pixel.  Decoding those files verbatim yields a fully
            // transparent image and the thumbnail disappears; BMP has no
            // reliable alpha convention, so treat an all-zero alpha channel as
            // opaque while keeping BMPs that really carry alpha intact.
            if ext == "bmp" && raw.chunks_exact(4).all(|px| px[3] == 0) {
                for px in raw.chunks_exact_mut(4) {
                    px[3] = 255;
                }
            }
            Ok(RgbaImage {
                width: w,
                height: h,
                center_x: 0,
                center_y: 0,
                rgba: raw,
            })
        }
        _ => {
            bail!("unsupported image extension: {:?}", path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A 32bpp BMP whose unused 4th byte is zero (the shape Siglus writes for
    /// save thumbnails) must decode as an opaque image, otherwise the decoded
    /// thumbnail is fully transparent and disappears from the load menu.
    #[test]
    fn bmp_with_zero_alpha_decodes_opaque() {
        let width = 2u32;
        let height = 2u32;
        let pixels: [u8; 16] = [
            0x20, 0x30, 0x40, 0x00, 0x50, 0x60, 0x70, 0x00, 0x11, 0x22, 0x33, 0x00, 0x44, 0x55,
            0x66, 0x00,
        ];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&(54u32 + 16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&54u32.to_le_bytes());
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&(width as i32).to_le_bytes());
        bytes.extend_from_slice(&(-(height as i32)).to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&pixels);

        let path = std::env::temp_dir().join(format!(
            "siglus-bmp-alpha-{}-{}.bmp",
            std::process::id(),
            line!()
        ));
        {
            let mut file = std::fs::File::create(&path).expect("create temp bmp");
            file.write_all(&bytes).expect("write temp bmp");
        }
        let image = load_image_any(&path, 0).expect("decode temp bmp");
        let _ = std::fs::remove_file(&path);

        assert_eq!((image.width, image.height), (width, height));
        assert!(
            image.rgba.chunks_exact(4).all(|px| px[3] == 255),
            "all-zero BMP alpha must decode opaque: {:?}",
            image.rgba
        );
    }
}
