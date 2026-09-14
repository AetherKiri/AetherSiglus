//! Native BMP capture trailer: seven i32 fields, i32 flags, UTF-16Z strings.

use anyhow::{bail, ensure, Result};

pub fn encode(flags: &[i64], strings: &[String]) -> Vec<u8> {
    let mut body = Vec::new();
    for &value in flags {
        body.extend_from_slice(&(value as i32).to_le_bytes());
    }
    let string_offset = 28 + body.len();
    for value in strings {
        for unit in value.encode_utf16().chain(std::iter::once(0)) {
            body.extend_from_slice(&unit.to_le_bytes());
        }
    }
    let mut out = Vec::with_capacity(28 + body.len());
    for word in [
        28,
        28,
        flags.len() * 4,
        flags.len(),
        string_offset,
        28 + body.len() - string_offset,
        strings.len(),
    ] {
        out.extend_from_slice(&(word as i32).to_le_bytes());
    }
    out.extend(body);
    out
}

pub fn decode_bmp(bmp: &[u8]) -> Result<(Vec<i64>, Vec<String>)> {
    ensure!(bmp.len() >= 14 && &bmp[..2] == b"BM", "not a BMP capture");
    let base = u32::from_le_bytes(bmp[2..6].try_into()?) as usize;
    ensure!(base >= 14 && base <= bmp.len(), "invalid BMP byte length");
    let trailer = &bmp[base..];
    ensure!(trailer.len() >= 28, "BMP contains no capture flags");
    let mut h = [0usize; 7];
    for (i, value) in h.iter_mut().enumerate() {
        *value = usize::try_from(i32::from_le_bytes(trailer[i * 4..i * 4 + 4].try_into()?))?;
    }
    ensure!(
        h[0] >= 28 && h[0] <= trailer.len(),
        "invalid capture header"
    );
    let region = |offset: usize, size: usize| -> Result<&[u8]> {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("capture size overflow"))?;
        ensure!(
            offset >= h[0] && end <= trailer.len(),
            "capture field outside trailer"
        );
        Ok(&trailer[offset..end])
    };
    let flags = region(h[1], h[2])?;
    let strings = region(h[4], h[5])?;
    ensure!(
        h[3] <= flags.len() / 4 && h[6] <= strings.len() / 2 && strings.len() % 2 == 0,
        "invalid capture item count"
    );
    let flags = flags
        .chunks_exact(4)
        .take(h[3])
        .map(|v| i32::from_le_bytes(v.try_into().unwrap()) as i64)
        .collect();
    let mut units = strings
        .chunks_exact(2)
        .map(|v| u16::from_le_bytes([v[0], v[1]]));
    let mut out = Vec::with_capacity(h[6]);
    for _ in 0..h[6] {
        let mut text = Vec::new();
        loop {
            match units.next() {
                Some(0) => break,
                Some(unit) => text.push(unit),
                None => bail!("unterminated capture string"),
            }
        }
        out.push(String::from_utf16(&text)?);
    }
    Ok((flags, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bmp(trailer: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 14];
        bytes[..2].copy_from_slice(b"BM");
        bytes[2..6].copy_from_slice(&14u32.to_le_bytes());
        bytes.extend_from_slice(trailer);
        bytes
    }
    #[test]
    fn round_trip_keeps_signed_flags_and_unicode_strings() {
        let flags = vec![-1, i32::MIN as i64, 345];
        let strings = vec!["中文 日本語 🌸\nline".into(), String::new()];
        let bytes = bmp(&encode(&flags, &strings));
        assert_eq!(decode_bmp(&bytes).unwrap(), (flags, strings));
        assert_eq!(u32::from_le_bytes(bytes[2..6].try_into().unwrap()), 14);
    }
    #[test]
    fn missing_or_truncated_metadata_is_not_success() {
        assert!(decode_bmp(&bmp(&[])).is_err());
        let mut bytes = bmp(&encode(&[1], &["test".into()]));
        bytes.pop();
        assert!(decode_bmp(&bytes).is_err());
    }
}
