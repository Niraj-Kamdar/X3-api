//! Request guards: DNS-rebinding Host check and bearer-token auth.

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use x3_core::{Error, Task};

use crate::{db::TokenInfo, error::ApiError, AppState};

/// The authenticated caller, inserted into request extensions by
/// [`require_token`]. MCP tools read it from the HTTP request parts.
#[derive(Debug, Clone)]
pub struct Principal {
    pub token_id: String,
    pub label: String,
    pub allowed_providers: Option<Vec<String>>,
    pub allowed_tasks: Option<Vec<Task>>,
    pub spend_cap_usd: Option<f64>,
}

impl From<TokenInfo> for Principal {
    fn from(t: TokenInfo) -> Self {
        Self {
            token_id: t.id,
            label: t.label,
            allowed_providers: t.allowed_providers,
            allowed_tasks: t.allowed_tasks,
            spend_cap_usd: t.spend_cap_usd,
        }
    }
}

impl Principal {
    /// Enforce the token's provider/task scopes.
    pub fn authorize(&self, provider: &str, task: Task) -> Result<(), Error> {
        if let Some(p) = &self.allowed_providers {
            if !p.iter().any(|x| x == provider) {
                return Err(Error::Forbidden(format!(
                    "token '{}' may not use provider '{provider}'",
                    self.label
                )));
            }
        }
        if let Some(t) = &self.allowed_tasks {
            if !t.contains(&task) {
                return Err(Error::Forbidden(format!(
                    "token '{}' may not use task '{task}'",
                    self.label
                )));
            }
        }
        Ok(())
    }

    /// A principal for in-process callers (the desktop UI's "test" buttons).
    pub fn local_app() -> Self {
        Self {
            token_id: "app".into(),
            label: "Voice Studio app".into(),
            allowed_providers: None,
            allowed_tasks: None,
            spend_cap_usd: None,
        }
    }
}

/// True if `host` (a Host header value) names the loopback interface.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    // Strip an optional port, handling bracketed IPv6.
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((h, tail)) if tail.is_empty() || tail.starts_with(':') => h,
            _ => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
            Some(_) => return false,
            None => host,
        }
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

/// Reject requests whose Host header is not loopback (DNS-rebinding guard),
/// and browser requests from non-loopback origins. CORS is never enabled.
pub async fn host_guard(req: Request, next: Next) -> Response {
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(is_loopback_host)
        .unwrap_or(false);
    if !host_ok {
        return (StatusCode::FORBIDDEN, "forbidden host").into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let ok = origin
            .to_str()
            .ok()
            .and_then(|o| o.split_once("://").map(|(_, rest)| rest.to_string()))
            .map(|rest| is_loopback_host(rest.trim_end_matches('/')))
            .unwrap_or(false);
        if !ok {
            return (StatusCode::FORBIDDEN, "forbidden origin").into_response();
        }
    }
    next.run(req).await
}

/// Require `Authorization: Bearer x3_live_…` on every request.
pub async fn require_token(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .map(str::to_string);
    let Some(token) = token else {
        return ApiError::unauthorized("missing bearer token").into_response();
    };
    match state.service.db().authenticate(&token).await {
        Ok(Some(info)) => {
            req.extensions_mut().insert(Principal::from(info));
            next.run(req).await
        }
        Ok(None) => ApiError::unauthorized("invalid or revoked token").into_response(),
        Err(e) => {
            tracing::error!("token lookup failed: {e:#}");
            ApiError::from(Error::Internal("token lookup failed".into())).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        for ok in [
            "localhost",
            "localhost:47800",
            "127.0.0.1",
            "127.0.0.1:1",
            "[::1]",
            "[::1]:47800",
            "LOCALHOST:1",
        ] {
            assert!(is_loopback_host(ok), "{ok}");
        }
        for bad in [
            "evil.com",
            "localhost.evil.com",
            "127.0.0.1.nip.io",
            "127.0.0.2",
            "0.0.0.0",
            "[::1]x",
            "localhost:abc",
            "",
        ] {
            assert!(!is_loopback_host(bad), "{bad}");
        }
    }

    #[test]
    fn scopes() {
        let p = Principal {
            token_id: "t".into(),
            label: "l".into(),
            allowed_providers: Some(vec!["apple".into()]),
            allowed_tasks: Some(vec![Task::Tts]),
            spend_cap_usd: None,
        };
        assert!(p.authorize("apple", Task::Tts).is_ok());
        assert!(p.authorize("elevenlabs", Task::Tts).is_err());
        assert!(p.authorize("apple", Task::Stt).is_err());
    }
}
