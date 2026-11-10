//! SpeechAnalyzer bridge. The Swift side lives in `swift/` and is linked only
//! when build.rs emits `cfg(x3_swift)` (macOS, Swift toolchain present).

use std::path::Path;

use x3_core::{Result, Transcript};

#[cfg(all(target_os = "macos", x3_swift))]
mod ffi {
    use std::{
        ffi::{c_char, CStr, CString},
        path::Path,
    };

    use serde::Deserialize;
    use x3_core::{Error, Result, Segment, Transcript, Word};

    extern "C" {
        fn x3_apple_transcribe(
            path: *const c_char,
            locale: *const c_char,
            words: bool,
        ) -> *mut c_char;
        fn x3_apple_stt_available() -> bool;
        fn x3_apple_free(ptr: *mut c_char);
    }

    /// JSON emitted by `Transcribe.swift`.
    #[derive(Deserialize)]
    struct SwiftTranscript {
        #[serde(default)]
        text: String,
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        duration: Option<f64>,
        #[serde(default)]
        segments: Vec<Segment>,
        #[serde(default)]
        words: Option<Vec<Word>>,
        #[serde(default)]
        error: Option<String>,
    }

    pub(crate) fn available() -> bool {
        // SAFETY: no arguments; returns a plain bool.
        unsafe { x3_apple_stt_available() }
    }

    pub(crate) fn transcribe_file(
        path: &Path,
        locale: Option<&str>,
        words: bool,
    ) -> Result<Transcript> {
        let p = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| Error::InvalidRequest("path contains NUL".into()))?;
        let l = locale
            .map(CString::new)
            .transpose()
            .map_err(|_| Error::InvalidRequest("locale contains NUL".into()))?;
        // SAFETY: pointers are valid NUL-terminated strings for the duration
        // of the call; the returned string is freed with x3_apple_free.
        let json = unsafe {
            let out = x3_apple_transcribe(
                p.as_ptr(),
                l.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                words,
            );
            if out.is_null() {
                return Err(Error::Internal(
                    "SpeechAnalyzer bridge returned null".into(),
                ));
            }
            let s = CStr::from_ptr(out).to_string_lossy().into_owned();
            x3_apple_free(out);
            s
        };
        let r: SwiftTranscript = serde_json::from_str(&json)
            .map_err(|e| Error::Internal(format!("bridge json: {e}")))?;
        if let Some(e) = r.error {
            return Err(Error::Internal(format!("SpeechAnalyzer: {e}")));
        }
        Ok(Transcript {
            text: r.text,
            language: r.language,
            duration_secs: r.duration,
            segments: r.segments,
            words: r.words,
        })
    }
}

#[cfg(all(target_os = "macos", x3_swift))]
pub(crate) fn available() -> bool {
    ffi::available()
}

#[cfg(all(target_os = "macos", x3_swift))]
pub(crate) fn transcribe_file(
    path: &Path,
    locale: Option<&str>,
    words: bool,
) -> Result<Transcript> {
    ffi::transcribe_file(path, locale, words)
}

#[cfg(not(all(target_os = "macos", x3_swift)))]
pub(crate) fn available() -> bool {
    false
}

#[cfg(not(all(target_os = "macos", x3_swift)))]
pub(crate) fn transcribe_file(
    _path: &Path,
    _locale: Option<&str>,
    _words: bool,
) -> Result<Transcript> {
    Err(x3_core::Error::Unsupported(
        "apple/speech-transcriber requires macOS 26 and the Swift bridge (built on a Mac without X3_SKIP_SWIFT)"
            .into(),
    ))
}
