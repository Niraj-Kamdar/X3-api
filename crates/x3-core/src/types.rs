use serde::{Deserialize, Serialize};

use crate::audio::AudioFormat;

/// Text-to-speech request, already resolved to a concrete model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    pub text: String,
    /// Provider-specific voice ID. `None` = engine default.
    #[serde(default)]
    pub voice: Option<String>,
    pub format: AudioFormat,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// Speaking rate multiplier, 1.0 = normal.
    #[serde(default)]
    pub speed: Option<f32>,
    /// BCP-47 / ISO-639-1 language hint.
    #[serde(default)]
    pub language: Option<String>,
}

/// Synthesized audio.
#[derive(Debug, Clone)]
pub struct TtsAudio {
    pub bytes: Vec<u8>,
    pub format: AudioFormat,
    pub sample_rate: Option<u32>,
    pub duration_secs: Option<f64>,
}

/// Speech-to-text request.
#[derive(Debug, Clone)]
pub struct SttRequest {
    pub audio: Vec<u8>,
    /// Original file name; used for the extension/MIME hint.
    pub filename: String,
    pub language: Option<String>,
    pub word_timestamps: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub word: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Normalized transcript returned by every STT engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Transcript {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    /// Present only when word timestamps were requested and available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<Word>>,
}

impl Transcript {
    /// Build sentence-like segments from word timings: split after
    /// sentence-ending punctuation or on pauses longer than `max_gap` seconds.
    pub fn segments_from_words(words: &[Word], max_gap: f64) -> Vec<Segment> {
        let mut segments = Vec::new();
        let mut cur: Vec<&Word> = Vec::new();
        let flush = |cur: &mut Vec<&Word>, segments: &mut Vec<Segment>| {
            if let (Some(first), Some(last)) = (cur.first(), cur.last()) {
                let text = cur
                    .iter()
                    .map(|w| w.word.trim())
                    .filter(|w| !w.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                segments.push(Segment {
                    start: first.start,
                    end: last.end,
                    text,
                });
            }
            cur.clear();
        };
        for w in words {
            if let Some(prev) = cur.last() {
                if w.start - prev.end > max_gap {
                    flush(&mut cur, &mut segments);
                }
            }
            cur.push(w);
            let t = w.word.trim_end();
            if t.ends_with('.') || t.ends_with('?') || t.ends_with('!') {
                flush(&mut cur, &mut segments);
            }
        }
        flush(&mut cur, &mut segments);
        segments
    }
}

/// A voice offered by an engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voice {
    /// Provider-specific voice ID (pass as `voice`).
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// e.g. `default`, `enhanced`, `premium` for Apple voices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(word: &str, start: f64, end: f64) -> Word {
        Word {
            word: word.into(),
            start,
            end,
        }
    }

    #[test]
    fn segments_split_on_punctuation_and_gaps() {
        let words = vec![
            w("Hello", 0.0, 0.4),
            w("world.", 0.5, 0.9),
            w("How", 1.0, 1.2),
            w("are", 1.3, 1.4),
            w("you", 3.0, 3.2),
        ];
        let segs = Transcript::segments_from_words(&words, 1.0);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].text, "Hello world.");
        assert_eq!(segs[1].text, "How are");
        assert_eq!(segs[2].start, 3.0);
    }
}
