#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::{available, synthesize, voices};

#[cfg(not(target_os = "macos"))]
mod stub {
    use x3_core::{Error, Result, Voice};

    pub(crate) fn available() -> bool {
        false
    }

    pub(crate) fn synthesize(
        _text: &str,
        _voice: Option<&str>,
        _speed: Option<f32>,
    ) -> Result<(Vec<f32>, u32)> {
        Err(Error::Unsupported(
            "apple/system (AVSpeechSynthesizer) requires macOS".into(),
        ))
    }

    pub(crate) fn voices() -> Result<Vec<Voice>> {
        Ok(Vec::new())
    }
}
#[cfg(not(target_os = "macos"))]
pub(crate) use stub::{available, synthesize, voices};
