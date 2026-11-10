//! Cloud provider adapters (ElevenLabs, Cartesia).
//!
//! Each adapter implements [`x3_core::Engine`]. API keys are fetched from the
//! vault on every request, held as `SecretString`, and exposed only while
//! building the outgoing request header. Errors are mapped onto
//! [`x3_core::Error`]; provider error bodies are truncated and never include
//! request headers.

pub mod cartesia;
pub mod elevenlabs;
mod http;

pub use cartesia::CartesiaEngine;
pub use elevenlabs::ElevenLabsEngine;

/// A reqwest client with sane timeouts for provider calls.
pub fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .user_agent(concat!("x3/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}
