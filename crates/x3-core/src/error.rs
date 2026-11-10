use crate::budget::BudgetScope;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One error type for every engine and provider.
///
/// Provider adapters map vendor-specific failures (HTTP 401, 429, quota
/// exhaustion, ...) onto these variants so the server and MCP layers can
/// report them uniformly. Messages must never contain secrets.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not supported: {0}")]
    Unsupported(String),

    #[error("unknown model or alias: {0}")]
    UnknownModel(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("no API key configured for provider '{0}'; add one in Voice Studio > Providers")]
    MissingKey(String),

    #[error("provider '{provider}' rejected the API key")]
    Unauthorized { provider: String },

    #[error("provider '{provider}' rate limited the request")]
    RateLimited {
        provider: String,
        retry_after_secs: Option<u64>,
    },

    #[error("provider '{provider}' quota or credits exhausted")]
    QuotaExceeded { provider: String },

    #[error(
        "budget exceeded for {scope}: spent ${spent:.4} + estimated ${estimate:.4} > cap ${cap:.4}"
    )]
    BudgetExceeded {
        scope: BudgetScope,
        spent: f64,
        estimate: f64,
        cap: f64,
    },

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("provider '{provider}' error (HTTP {status}): {message}")]
    Provider {
        provider: String,
        status: u16,
        message: String,
    },

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// HTTP status code the local server should use for this error.
    pub fn http_status(&self) -> u16 {
        match self {
            Error::Unsupported(_) => 400,
            Error::UnknownModel(_) => 404,
            Error::InvalidRequest(_) => 400,
            Error::MissingKey(_) => 424,
            Error::Unauthorized { .. } => 502,
            Error::RateLimited { .. } => 429,
            Error::QuotaExceeded { .. } => 402,
            Error::BudgetExceeded { .. } => 402,
            Error::Forbidden(_) => 403,
            Error::Provider { .. } => 502,
            Error::Io(_) | Error::Internal(_) => 500,
        }
    }

    /// Short machine-readable code, used in OpenAI-style error bodies.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Unsupported(_) => "unsupported",
            Error::UnknownModel(_) => "model_not_found",
            Error::InvalidRequest(_) => "invalid_request",
            Error::MissingKey(_) => "missing_provider_key",
            Error::Unauthorized { .. } => "provider_unauthorized",
            Error::RateLimited { .. } => "provider_rate_limited",
            Error::QuotaExceeded { .. } => "provider_quota_exceeded",
            Error::BudgetExceeded { .. } => "budget_exceeded",
            Error::Forbidden(_) => "forbidden",
            Error::Provider { .. } => "provider_error",
            Error::Io(_) => "io_error",
            Error::Internal(_) => "internal_error",
        }
    }
}
