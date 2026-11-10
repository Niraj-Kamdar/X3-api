//! Shared types and traits for Voice Studio (`x3`).
//!
//! This crate is platform-independent. It defines:
//! - request/response types for text-to-speech and speech-to-text,
//! - the [`Engine`] / [`LoadedModel`] traits implemented by every backend
//!   (Apple built-in speech, ElevenLabs, Cartesia, and later local models),
//! - namespaced [`ModelId`]s and the user-editable [`AliasTable`],
//! - the [`ModelRouter`] that resolves an alias or model ID to an engine,
//! - usage records, pricing estimates and budget checks.

pub mod alias;
pub mod audio;
pub mod budget;
pub mod engine;
pub mod error;
pub mod model;
pub mod router;
pub mod types;
pub mod usage;

pub use alias::AliasTable;
pub use audio::{AudioFormat, AudioSpec};
pub use budget::{BudgetCheck, BudgetScope, Budgets};
pub use engine::{Engine, LoadedModel};
pub use error::{Error, Result};
pub use model::{ModelId, ModelInfo, Pricing, PricingUnit, Task};
pub use router::{ModelRouter, Resolved};
pub use types::{Segment, SttRequest, Transcript, TtsAudio, TtsRequest, Voice, Word};
pub use usage::{UsageRecord, UsageStatus};
