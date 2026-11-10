use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use x3_core::Error;

/// OpenAI-style error body: `{"error": {"message", "type", "code"}}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn unauthorized(msg: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_token",
            message: msg.to_string(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: msg.into(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self {
            status: StatusCode::from_u16(e.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: e.code(),
            message: e.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let kind = if self.status.is_server_error() {
            "server_error"
        } else {
            "invalid_request_error"
        };
        (
            self.status,
            Json(serde_json::json!({
                "error": { "message": self.message, "type": kind, "code": self.code }
            })),
        )
            .into_response()
    }
}
