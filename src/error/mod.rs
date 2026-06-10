use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;

/// Generic JSON response with a single `message` field. Used across
/// modules for operations that return a confirmation string rather
/// than a domain entity.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("database error")]
    Database(#[from] sqlx::Error),

    #[error("redis error")]
    Redis(#[from] redis::RedisError),

    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

/// PostgreSQL SQLSTATE for `unique_violation`. Centralized here so the
/// `Database` arm can auto-map common constraint errors without every
/// service having to hand-translate them.
const PG_UNIQUE_VIOLATION: &str = "23505";

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Validation(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.clone()),
            AppError::Database(err) => {
                // A unique-constraint violation that reached IntoResponse
                // means no service layer intercepted it — map to 409 so
                // clients get a meaningful status instead of an opaque 500.
                // We only surface the SQLSTATE code, not the constraint name
                // or bound parameter values, to avoid leaking schema shape.
                if let Some(db_err) = err.as_database_error() {
                    if db_err.code().as_deref() == Some(PG_UNIQUE_VIOLATION) {
                        tracing::warn!(sqlstate = PG_UNIQUE_VIOLATION, "unique violation");
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({ "error": "resource already exists" })),
                        )
                            .into_response();
                    }
                }
                // `%err` uses the short Display form; `{:?}` would leak
                // constraint names, table/column metadata, and any bound
                // parameters included in sqlx's Debug impl.
                tracing::error!(error = %err, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            AppError::Redis(err) => {
                tracing::error!(error = %err, "redis error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
            AppError::Internal(err) => {
                tracing::error!(error = %err, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_string(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
