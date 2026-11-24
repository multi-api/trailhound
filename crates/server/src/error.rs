use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// An error in the shape Meilisearch clients expect.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub typ: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        let typ = match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "auth",
            s if s.is_server_error() => "internal",
            _ => "invalid_request",
        };
        ApiError { status, code: code.to_string(), typ, message: message.into() }
    }

    pub fn bad(code: &str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn not_found(code: &str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    pub fn index_not_found(uid: &str) -> Self {
        Self::not_found("index_not_found", format!("Index `{uid}` not found."))
    }

    pub fn to_json(&self) -> Value {
        json!({
            "message": self.message,
            "code": self.code,
            "type": self.typ,
            "link": format!("https://docs.meilisearch.com/errors#{}", self.code),
        })
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.to_json())).into_response()
    }
}

impl From<trailhound_engine::Error> for ApiError {
    fn from(e: trailhound_engine::Error) -> Self {
        match e {
            trailhound_engine::Error::User { code, message } => {
                let status = if code.ends_with("_not_found") { StatusCode::NOT_FOUND } else { StatusCode::BAD_REQUEST };
                ApiError::new(status, code, message)
            }
            other => ApiError::internal(other.to_string()),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::internal(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::internal(e.to_string())
    }
}
