//! The WAV files Hydra records and sends: 16 kHz, 16-bit mono PCM.

use anyhow::{Result, bail};

pub const SAMPLE_RATE: u32 = 16_000;
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;

/// The samples in a WAV file: everything after its `data` chunk header.
/// arecord may leave the chunk size unset when interrupted, so the size is
/// not trusted.
pub fn pcm(wav: &[u8]) -> Result<&[u8]> {
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        bail!("the recording is not a WAV file");
    }
    let mut offset = 12;
    while offset + 8 <= wav.len() {
        let id = &wav[offset..offset + 4];
        let size = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into()?) as usize;
        offset += 8;
        if id == b"data" {
            let samples = &wav[offset..];
            // Whole 16-bit samples only.
            return Ok(&samples[..samples.len() & !1]);
        }
        offset = offset.saturating_add(size + (size & 1));
    }
    bail!("the recording has no audio data")
}

/// A 16 kHz, 16-bit mono WAV file holding `pcm`.
pub fn encode(pcm: &[u8]) -> Vec<u8> {
    let size = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(BYTES_PER_SECOND as u32).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Converts mono samples at `rate` to 16 kHz, 16-bit PCM. Each output
/// sample averages the input samples it covers, which also filters out
/// most of what would alias.
#[cfg(any(windows, test))]
pub fn resample(samples: &[f32], rate: u32) -> Vec<u8> {
    let step = f64::from(rate) / f64::from(SAMPLE_RATE);
    let count = (samples.len() as f64 / step) as usize;
    let mut pcm = Vec::with_capacity(count * 2);
    for index in 0..count {
        let start = (index as f64 * step) as usize;
        let end = (((index + 1) as f64 * step) as usize).clamp(start + 1, samples.len());
        let window = &samples[start..end];
        let value = window.iter().sum::<f32>() / window.len() as f32;
        let sample = (value.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    pcm
}

/// Scales a 16-bit PCM WAV file's samples to `volume` percent, in place.
/// Other formats are left as they are.
#[cfg(any(windows, test))]
pub fn scale_volume(wav: &mut [u8], volume: u8) {
    if volume >= 100 || wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return;
    }
    let mut sixteen_bit_pcm = false;
    let mut offset = 12;
    while offset + 8 <= wav.len() {
        let id = [
            wav[offset],
            wav[offset + 1],
            wav[offset + 2],
            wav[offset + 3],
        ];
        let size = u32::from_le_bytes([
            wav[offset + 4],
            wav[offset + 5],
            wav[offset + 6],
            wav[offset + 7],
        ]) as usize;
        offset += 8;
        let end = offset.saturating_add(size).min(wav.len());
        match &id {
            b"fmt " if end - offset >= 16 => {
                let format = u16::from_le_bytes([wav[offset], wav[offset + 1]]);
                let bits = u16::from_le_bytes([wav[offset + 14], wav[offset + 15]]);
                sixteen_bit_pcm = format == 1 && bits == 16;
            }
            b"data" if sixteen_bit_pcm => {
                for sample in wav[offset..end].as_chunks_mut::<2>().0 {
                    let value = i16::from_le_bytes([sample[0], sample[1]]);
                    let scaled = (i32::from(value) * i32::from(volume) / 100) as i16;
                    sample.copy_from_slice(&scaled.to_le_bytes());
                }
                return;
            }
            _ => {}
        }
        offset = end + (size & 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_files_round_trip() {
        let samples: Vec<u8> = (0..=255).collect();
        assert_eq!(pcm(&encode(&samples)).unwrap(), samples);
    }

    #[test]
    fn an_unset_data_size_reads_to_the_end() {
        let mut wav = encode(&[1, 2, 3, 4]);
        wav[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(pcm(&wav).unwrap(), [1, 2, 3, 4]);
    }

    #[test]
    fn resampling_keeps_the_duration_and_level() {
        let samples = vec![0.5_f32; 48_000];
        let pcm = resample(&samples, 48_000);
        assert_eq!(pcm.len(), BYTES_PER_SECOND);
        assert_eq!(
            i16::from_le_bytes([pcm[0], pcm[1]]),
            (0.5 * f32::from(i16::MAX)) as i16
        );
        assert_eq!(resample(&vec![0.0; 44_100], 44_100).len(), BYTES_PER_SECOND);
    }

    #[test]
    fn volume_scales_sixteen_bit_samples() {
        let mut sound = encode(&1000_i16.to_le_bytes());
        scale_volume(&mut sound, 50);
        assert_eq!(pcm(&sound).unwrap(), 500_i16.to_le_bytes());
        let mut not_wav = b"not a wav file".to_vec();
        scale_volume(&mut not_wav, 50);
        assert_eq!(not_wav, b"not a wav file");
    }
}
