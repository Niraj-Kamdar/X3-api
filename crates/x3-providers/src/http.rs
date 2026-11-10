use std::sync::Arc;

use x3_core::Error;
use x3_vault::{SecretStore, SecretString};

/// Fetch the provider key from the vault or fail with `MissingKey`.
pub(crate) fn key(store: &Arc<dyn SecretStore>, provider: &str) -> Result<SecretString, Error> {
    store
        .get(provider)
        .map_err(|e| Error::Internal(format!("vault: {e}")))?
        .ok_or_else(|| Error::MissingKey(provider.to_string()))
}

pub(crate) fn transport_error(provider: &str, e: reqwest::Error) -> Error {
    // reqwest errors include the URL but never headers, so no key leaks.
    Error::Provider {
        provider: provider.to_string(),
        status: e.status().map(|s| s.as_u16()).unwrap_or(0),
        message: e.without_url().to_string(),
    }
}

/// Map a non-success provider response onto our error type.
///
/// `quota_markers` are lowercase substrings that, when present in the body,
/// indicate exhausted credits/quota rather than a bad key.
pub(crate) async fn map_error(
    provider: &str,
    resp: reqwest::Response,
    quota_markers: &[&str],
) -> Error {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let body = resp.text().await.unwrap_or_default();
    let lower = body.to_ascii_lowercase();
    let is_quota = quota_markers.iter().any(|m| lower.contains(m));
    match status {
        402 => Error::QuotaExceeded {
            provider: provider.into(),
        },
        401 | 403 if is_quota => Error::QuotaExceeded {
            provider: provider.into(),
        },
        429 if is_quota => Error::QuotaExceeded {
            provider: provider.into(),
        },
        401 | 403 => Error::Unauthorized {
            provider: provider.into(),
        },
        429 => Error::RateLimited {
            provider: provider.into(),
            retry_after_secs: retry_after,
        },
        _ => Error::Provider {
            provider: provider.into(),
            status,
            message: truncate(&body, 300),
        },
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

pub(crate) fn mime_for_filename(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "wav" => "audio/wav",
        "mp3" | "mpga" | "mpeg" => "audio/mpeg",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "webm" => "audio/webm",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}
