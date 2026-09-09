//! Optional host JPEG acceleration. Standalone hosts retain the Rust decoder.
use std::sync::OnceLock;

/// Decode into the caller's tightly packed, top-to-bottom RGBA8 buffer.
/// Return zero only after validating the JPEG dimensions and filling the buffer.
pub type DecodeJpeg = unsafe extern "C" fn(
    *const u8, usize, u32, u32, *mut u8, usize,
) -> i32;

static DECODER: OnceLock<DecodeJpeg> = OnceLock::new();

/// Register a process-lifetime decoder before opening games.
///
/// # Safety
/// The callback must be thread-safe, must not retain the borrowed buffers or
/// unwind across the ABI, and must not write beyond the supplied output length.
#[no_mangle]
pub unsafe extern "C" fn siglus_register_jpeg_decoder(decoder: Option<DecodeJpeg>) -> i32 {
    let Some(decoder) = decoder else { return -1; };
    if DECODER.set(decoder).is_err() { return -2; }
    0
}

pub(crate) fn decode(jpeg: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    decode_with(DECODER.get().copied()?, jpeg, width, height)
}

fn decode_with(decoder: DecodeJpeg, jpeg: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let len = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    // G00 dimensions are untrusted and have not yet been compared with the JPEG
    // header. Oversized requests use the fallback's own header/limit validation.
    if len == 0 || len > 256 * 1024 * 1024 { return None; }
    let mut pixels = vec![0; len];
    let _perf = crate::perf_trace::Span::new("image.jpeg_native");
    let status = unsafe {
        decoder(jpeg.as_ptr(), jpeg.len(), width, height, pixels.as_mut_ptr(), pixels.len())
    };
    (status == 0).then_some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn accepted(src: *const u8, size: usize, w: u32, h: u32,
        dst: *mut u8, len: usize) -> i32 {
        assert_eq!(std::slice::from_raw_parts(src, size), [0xff, 0xd8]);
        assert_eq!((w, h, len), (2, 1, 8));
        std::slice::from_raw_parts_mut(dst, len).copy_from_slice(&[1, 2, 3, 255, 4, 5, 6, 255]);
        0
    }
    unsafe extern "C" fn rejected(_: *const u8, _: usize, _: u32, _: u32,
        _: *mut u8, _: usize) -> i32 { -1 }

    #[test]
    fn host_decoder_output_is_owned_and_rejection_falls_back() {
        assert_eq!(decode_with(accepted, &[0xff, 0xd8], 2, 1).unwrap(),
            [1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(decode_with(rejected, &[], 2, 1).is_none());
    }

    #[test]
    fn invalid_dimensions_do_not_allocate_or_call_backend() {
        assert!(decode_with(accepted, &[], 0, 1).is_none());
        assert!(decode_with(accepted, &[], u32::MAX, u32::MAX).is_none());
        assert!(decode_with(accepted, &[], 65535, 65535).is_none());
    }
}
