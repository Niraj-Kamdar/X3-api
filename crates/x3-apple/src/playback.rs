//! Speaker playback for the MCP `speak`/`play` tools.
//!
//! macOS uses `/usr/bin/afplay`, which ships with every macOS install and
//! plays wav/mp3/m4a/aiff. Raw `.pcm` and Ogg Opus are not playable by
//! afplay; callers should play wav/mp3.
//!
//! TODO(M1): replace with an in-process player (AVAudioPlayer or cpal) so we
//! can show a menu-bar indicator and support cancellation.

use std::path::Path;

use x3_core::{Error, Result};

pub async fn play_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        return Err(Error::InvalidRequest(format!(
            "no such file: {}",
            path.display()
        )));
    }
    imp::play(path).await
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub async fn play(path: &Path) -> Result<()> {
        let status = tokio::process::Command::new("/usr/bin/afplay")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::Internal(format!("afplay exited with {status}")))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub async fn play(_path: &Path) -> Result<()> {
        Err(Error::Unsupported(
            "speaker playback is only implemented on macOS".into(),
        ))
    }
}
