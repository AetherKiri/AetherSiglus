//! Pitch-preserving voice time compression. Keep the original sample rate;
//! align short overlapping waveform segments instead of resampling playback.

use anyhow::{bail, ensure, Result};
use siglus_assets::vorbis::{pcm16_to_wav_bytes, Pcm16};

pub fn compress_wav(wav: Vec<u8>, percent: u16) -> Result<Vec<u8>> {
    let percent = percent.clamp(100, 400);
    if percent == 100 {
        return Ok(wav);
    }
    ensure!(
        wav.len() >= 12 && &wav[..4] == b"RIFF" && &wav[8..12] == b"WAVE",
        "JITAN needs RIFF/WAVE"
    );
    let mut pos = 12usize;
    let mut format = None;
    let mut data = None;
    while pos + 8 <= wav.len() {
        let tag = &wav[pos..pos + 4];
        let size = u32::from_le_bytes(wav[pos + 4..pos + 8].try_into()?) as usize;
        pos += 8;
        let end = pos
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("WAV size overflow"))?;
        ensure!(end <= wav.len(), "truncated WAV chunk");
        if tag == b"fmt " {
            ensure!(size >= 16, "short WAV format");
            format = Some((
                u16::from_le_bytes(wav[pos..pos + 2].try_into()?),
                u16::from_le_bytes(wav[pos + 2..pos + 4].try_into()?),
                u32::from_le_bytes(wav[pos + 4..pos + 8].try_into()?),
                u16::from_le_bytes(wav[pos + 14..pos + 16].try_into()?),
            ));
        } else if tag == b"data" {
            data = Some(&wav[pos..end]);
        }
        pos = end.saturating_add(size & 1);
    }
    let Some((kind, channels, rate, bits)) = format else {
        bail!("missing WAV format");
    };
    ensure!(
        kind == 1 && bits == 16 && (1..=8).contains(&channels) && (8000..=192000).contains(&rate),
        "unsupported JITAN PCM format {kind}/{bits}/{channels}/{rate}"
    );
    let data = data.ok_or_else(|| anyhow::anyhow!("missing WAV data"))?;
    ensure!(
        data.len() % (channels as usize * 2) == 0,
        "unaligned PCM samples"
    );
    let samples: Vec<i16> = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    // The original leaves very short voices (<100 ms) unchanged.
    if samples.len() / (channels as usize) < rate as usize / 10 {
        return Ok(wav);
    }
    let output = compress_pcm(&samples, channels as usize, rate as usize, percent as usize);
    Ok(pcm16_to_wav_bytes(&Pcm16 {
        channels,
        sample_rate: rate,
        samples: output,
    }))
}

fn compress_pcm(input: &[i16], channels: usize, rate: usize, percent: usize) -> Vec<i16> {
    let frames = input.len() / channels;
    let target = frames * 100 / percent;
    let hop = (rate / 80).max(1);
    let overlap = (rate / 110).max(1);
    let window = hop + overlap;
    let search = hop / 4;
    let mut output = vec![0i16; (target + window) * channels];
    let first = window.min(frames).min(target);
    output[..first * channels].copy_from_slice(&input[..first * channels]);
    let mut out_pos = hop;
    let mut previous_source = 0;
    while out_pos < target {
        let expected = (out_pos * percent / 100).min(frames.saturating_sub(window));
        let begin = expected
            .saturating_sub(search)
            .max(previous_source + 1)
            .min(frames - window);
        let end = (expected + search).min(frames - window).max(begin);
        let mut best = begin;
        let mut best_score = f64::NEG_INFINITY;
        for source in begin..=end {
            let (mut cross, mut a2, mut b2) = (0f64, 0f64, 0f64);
            // Every fourth frame keeps long-voice preparation off the hot path.
            for offset in (0..overlap).step_by(4) {
                for channel in 0..channels {
                    let a = output[(out_pos + offset) * channels + channel] as f64;
                    let b = input[(source + offset) * channels + channel] as f64;
                    cross += a * b;
                    a2 += a * a;
                    b2 += b * b;
                }
            }
            let score = cross / (a2 * b2).sqrt().max(1.0) - source.abs_diff(expected) as f64 * 1e-8;
            if score > best_score {
                best_score = score;
                best = source;
            }
        }
        let count = window.min(frames - best).min(target + window - out_pos);
        for offset in 0..count {
            for channel in 0..channels {
                let at = (out_pos + offset) * channels + channel;
                let sample = input[(best + offset) * channels + channel] as i32;
                output[at] = if offset < overlap {
                    ((output[at] as i32 * (overlap - offset) as i32 + sample * offset as i32)
                        / overlap as i32) as i16
                } else {
                    sample as i16
                };
            }
        }
        previous_source = best;
        out_pos += hop;
    }
    output.truncate(target * channels);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compression_changes_duration_without_transposing_pitch() {
        let rate = 16000;
        let input: Vec<i16> = (0..rate * 2)
            .map(|i| {
                ((i as f64 * 440.0 * std::f64::consts::TAU / rate as f64).sin() * 10000.0) as i16
            })
            .collect();
        for speed in [150, 200, 300, 400] {
            let out = compress_pcm(&input, 1, rate, speed);
            assert_eq!(out.len(), input.len() * 100 / speed);
            let crossings = out.windows(2).filter(|v| v[0] <= 0 && v[1] > 0).count();
            let frequency = crossings as f64 * rate as f64 / out.len() as f64;
            assert!((frequency - 440.0).abs() < 12.0, "{speed}%: {frequency} Hz");
        }
    }
    #[test]
    fn normal_rate_is_byte_identical_and_invalid_pcm_is_rejected() {
        let bytes = vec![1, 2, 3];
        assert_eq!(compress_wav(bytes.clone(), 100).unwrap(), bytes);
        assert!(compress_wav(vec![0; 44], 150).is_err());
    }
}
