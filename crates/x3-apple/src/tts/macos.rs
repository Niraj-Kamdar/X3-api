//! AVSpeechSynthesizer via `objc2-avf-audio`. UNVERIFIED: only compiles on
//! macOS; written against objc2-avf-audio 0.3.2 generated bindings.
//!
//! TODO(mac): verify on a real Mac
//! - that `writeUtterance:toBufferCallback:` delivers buffers without the
//!   main run loop being pumped (we call it from a tokio blocking thread;
//!   in the Tauri app the main run loop is running, in `x3 serve` it is not);
//! - the buffer sample format (we handle Float32 and Int16, mono,
//!   non-interleaved; first channel only);
//! - that a zero-length buffer marks the end of the utterance (Apple docs say
//!   the callback receives an empty buffer when synthesis is complete).

use std::{ptr::NonNull, sync::mpsc, time::Duration};

use block2::RcBlock;
use objc2::runtime::AnyObject;
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioCommonFormat, AVAudioPCMBuffer, AVSpeechSynthesisVoice,
    AVSpeechSynthesisVoiceQuality, AVSpeechSynthesizer, AVSpeechUtterance,
    AVSpeechUtteranceDefaultSpeechRate, AVSpeechUtteranceMaximumSpeechRate,
    AVSpeechUtteranceMinimumSpeechRate,
};
use objc2_foundation::NSString;
use x3_core::{Error, Result, Voice};

use crate::PROVIDER;

enum Msg {
    Chunk(Vec<f32>, u32),
    Done,
}

pub(crate) fn available() -> bool {
    true
}

fn quality_name(q: AVSpeechSynthesisVoiceQuality) -> &'static str {
    if q == AVSpeechSynthesisVoiceQuality::Premium {
        "premium"
    } else if q == AVSpeechSynthesisVoiceQuality::Enhanced {
        "enhanced"
    } else {
        "default"
    }
}

fn quality_rank(q: AVSpeechSynthesisVoiceQuality) -> i64 {
    q.0 as i64
}

pub(crate) fn voices() -> Result<Vec<Voice>> {
    let mut out = Vec::new();
    // SAFETY: class methods/properties on AVSpeechSynthesisVoice are safe to
    // call from any thread per Apple docs (read-only voice metadata).
    unsafe {
        for v in AVSpeechSynthesisVoice::speechVoices().iter() {
            out.push(Voice {
                id: v.identifier().to_string(),
                name: v.name().to_string(),
                provider: PROVIDER.into(),
                language: Some(v.language().to_string()),
                quality: Some(quality_name(v.quality()).into()),
                description: None,
            });
        }
    }
    // Best voices first.
    out.sort_by_key(|v| match v.quality.as_deref() {
        Some("premium") => 0,
        Some("enhanced") => 1,
        _ => 2,
    });
    Ok(out)
}

/// Pick the highest-quality installed voice for the user's language.
unsafe fn default_voice() -> Option<objc2::rc::Retained<AVSpeechSynthesisVoice>> {
    let lang = AVSpeechSynthesisVoice::currentLanguageCode().to_string();
    let primary = lang.split(['-', '_']).next().unwrap_or("en").to_string();
    let mut best: Option<objc2::rc::Retained<AVSpeechSynthesisVoice>> = None;
    let mut best_score = i64::MIN;
    for v in AVSpeechSynthesisVoice::speechVoices().iter() {
        let vl = v.language().to_string();
        let lang_score = if vl == lang {
            2
        } else if vl.starts_with(&primary) {
            1
        } else {
            continue;
        };
        let score = lang_score * 10 + quality_rank(v.quality());
        if score > best_score {
            best_score = score;
            best = Some(v.clone());
        }
    }
    best.or_else(|| AVSpeechSynthesisVoice::voiceWithLanguage(Some(&NSString::from_str(&lang))))
}

pub(crate) fn synthesize(
    text: &str,
    voice: Option<&str>,
    speed: Option<f32>,
) -> Result<(Vec<f32>, u32)> {
    if text.trim().is_empty() {
        return Err(Error::InvalidRequest("text is empty".into()));
    }
    let (tx, rx) = mpsc::channel::<Msg>();

    // SAFETY: objects are created and used on this thread; the callback only
    // reads the buffer it is handed and sends owned data over a channel.
    let synth = unsafe {
        let synth = AVSpeechSynthesizer::new();
        let utterance = AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(text));

        let v = match voice {
            Some(id) => Some(
                AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(id)).ok_or_else(
                    || Error::InvalidRequest(format!("Apple voice '{id}' is not installed")),
                )?,
            ),
            None => default_voice(),
        };
        utterance.setVoice(v.as_deref());

        if let Some(s) = speed {
            let rate = (AVSpeechUtteranceDefaultSpeechRate * s.max(0.0)).clamp(
                AVSpeechUtteranceMinimumSpeechRate,
                AVSpeechUtteranceMaximumSpeechRate,
            );
            utterance.setRate(rate);
        }

        let block = RcBlock::new(move |buf: NonNull<AVAudioBuffer>| {
            let buf: &AVAudioBuffer = buf.as_ref();
            let any: &AnyObject = buf;
            let Some(pcm) = any.downcast_ref::<AVAudioPCMBuffer>() else {
                return;
            };
            let frames = pcm.frameLength() as usize;
            if frames == 0 {
                let _ = tx.send(Msg::Done);
                return;
            }
            let format = pcm.format();
            let rate = format.sampleRate() as u32;
            let samples: Vec<f32> = match format.commonFormat() {
                AVAudioCommonFormat::PCMFormatFloat32 => {
                    let chans = pcm.floatChannelData();
                    if chans.is_null() {
                        return;
                    }
                    std::slice::from_raw_parts((*chans).as_ptr(), frames).to_vec()
                }
                AVAudioCommonFormat::PCMFormatInt16 => {
                    let chans = pcm.int16ChannelData();
                    if chans.is_null() {
                        return;
                    }
                    std::slice::from_raw_parts((*chans).as_ptr(), frames)
                        .iter()
                        .map(|s| *s as f32 / 32768.0)
                        .collect()
                }
                _ => return,
            };
            let _ = tx.send(Msg::Chunk(samples, rate));
        });
        synth.writeUtterance_toBufferCallback(&utterance, RcBlock::as_ptr(&block));
        // Keep the synthesizer and block alive until we are done.
        (synth, block)
    };

    // Generous timeout: ~1 minute plus 100 ms per character.
    let deadline = Duration::from_secs(60) + Duration::from_millis(100 * text.len() as u64);
    let start = std::time::Instant::now();
    let mut samples = Vec::new();
    let mut rate = 0u32;
    loop {
        let left = deadline.saturating_sub(start.elapsed());
        match rx.recv_timeout(left) {
            Ok(Msg::Chunk(chunk, r)) => {
                rate = r;
                samples.extend_from_slice(&chunk);
            }
            Ok(Msg::Done) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(Error::Internal(
                    "AVSpeechSynthesizer timed out producing audio".into(),
                ))
            }
        }
    }
    drop(synth);
    if samples.is_empty() || rate == 0 {
        return Err(Error::Internal(
            "AVSpeechSynthesizer produced no audio".into(),
        ));
    }
    Ok((samples, rate))
}
