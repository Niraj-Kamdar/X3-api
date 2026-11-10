use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Output/input audio container.
///
/// `Pcm` is raw signed 16-bit little-endian mono samples with no header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    Wav,
    Mp3,
    Opus,
    Pcm,
}

impl AudioFormat {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "wav" => Ok(Self::Wav),
            "mp3" => Ok(Self::Mp3),
            "opus" => Ok(Self::Opus),
            "pcm" => Ok(Self::Pcm),
            other => Err(Error::Unsupported(format!(
                "audio format '{other}' (supported: wav, mp3, opus, pcm)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Opus => "opus",
            Self::Pcm => "pcm",
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/ogg",
            Self::Pcm => "audio/pcm",
        }
    }

    pub fn extension(self) -> &'static str {
        self.as_str()
    }
}

/// Format plus optional sample rate (engines pick a default when `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSpec {
    pub format: AudioFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
}

/// Encode mono `f32` samples in [-1, 1] as a 16-bit PCM WAV file.
pub fn wav_from_f32(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| Error::Internal(format!("wav encode: {e}")))?;
        for s in samples {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer
                .write_sample(v)
                .map_err(|e| Error::Internal(format!("wav encode: {e}")))?;
        }
        writer
            .finalize()
            .map_err(|e| Error::Internal(format!("wav encode: {e}")))?;
    }
    Ok(cursor.into_inner())
}

/// Raw little-endian 16-bit PCM from `f32` samples.
pub fn pcm_s16le_from_f32(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Duration of a WAV file in seconds, if the bytes parse as WAV.
pub fn wav_duration_secs(bytes: &[u8]) -> Option<f64> {
    let reader = hound::WavReader::new(Cursor::new(bytes)).ok()?;
    let spec = reader.spec();
    if spec.sample_rate == 0 {
        return None;
    }
    Some(reader.duration() as f64 / spec.sample_rate as f64)
}

/// Duration of raw mono s16le PCM.
pub fn pcm_duration_secs(byte_len: usize, sample_rate: u32) -> Option<f64> {
    if sample_rate == 0 {
        return None;
    }
    Some((byte_len / 2) as f64 / sample_rate as f64)
}

/// Best-effort duration of an encoded file whose exact length we cannot
/// parse (MP3/Opus/etc.). Assumes `kbps` kilobits per second.
pub fn estimate_duration_from_bitrate(byte_len: usize, kbps: u32) -> f64 {
    if kbps == 0 {
        return 0.0;
    }
    (byte_len as f64 * 8.0) / (kbps as f64 * 1000.0)
}

/// Guess an audio duration for budgeting before a transcription call.
pub fn estimate_input_duration_secs(bytes: &[u8]) -> f64 {
    wav_duration_secs(bytes).unwrap_or_else(|| estimate_duration_from_bitrate(bytes.len(), 64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip_duration() {
        let samples = vec![0.0f32; 22_050];
        let wav = wav_from_f32(&samples, 22_050).unwrap();
        let d = wav_duration_secs(&wav).unwrap();
        assert!((d - 1.0).abs() < 1e-6);
    }

    #[test]
    fn parse_formats() {
        assert_eq!(AudioFormat::parse("MP3").unwrap(), AudioFormat::Mp3);
        assert!(AudioFormat::parse("aac").is_err());
    }

    #[test]
    fn pcm_len() {
        assert_eq!(pcm_s16le_from_f32(&[0.0, 1.0, -1.0]).len(), 6);
        assert_eq!(pcm_duration_secs(32_000, 16_000), Some(1.0));
    }
}
