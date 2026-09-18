use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Sign in first.")]
    Unauthorized,
    #[error("You can't manage that.")]
    Forbidden,
    #[error("Not found.")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0} isn't available right now.")]
    Unavailable(&'static str),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
    #[error("database error")]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "authentication_required"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            Self::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::Internal(_) | Self::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        let message = match &self {
            Self::Internal(_) | Self::Database(_) => {
                tracing::error!(error = ?self, "request failed");
                "Something went wrong on our side.".to_owned()
            }
            other => other.to_string(),
        };
        (status, Json(json!({ "error": { "code": code, "message": message } }))).into_response()
    }
}
