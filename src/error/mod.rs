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

/// PostgreSQL SQLSTATE for `exclusion_violation` (an `EXCLUDE USING gist`
/// constraint rejecting an overlapping row, e.g. double-booked time ranges).
/// Centralized here so the `Database` arm can auto-map it the same way it
/// does `PG_UNIQUE_VIOLATION`.
const PG_EXCLUSION_VIOLATION: &str = "23P01";

/// The violated constraint's name, if `e` is a `Database` error and the
/// driver reports one. Any other `sqlx::Error` variant → `None`.
pub fn constraint_name(e: &sqlx::Error) -> Option<&str> {
    e.as_database_error()?.constraint()
}

impl AppError {
    /// Translates a unique-constraint violation into a `Conflict(msg)`.
    /// `e` is SQLSTATE 23505 (`unique_violation`) → `Conflict(msg)`;
    /// anything else → `Database(e)` (500, same as an unhandled error).
    ///
    /// Same safety invariant as `IntoResponse`'s fallback below: `msg` is
    /// caller-supplied and is the only thing that reaches the HTTP body —
    /// no constraint name or bound value is ever surfaced.
    pub fn conflict_on_unique(e: sqlx::Error, msg: impl Into<String>) -> Self {
        if let Some(db_err) = e.as_database_error() {
            if db_err.code().as_deref() == Some(PG_UNIQUE_VIOLATION) {
                return AppError::Conflict(msg.into());
            }
        }
        AppError::Database(e)
    }

    /// Translates an exclusion-constraint violation into a `Conflict(msg)`.
    /// `e` is SQLSTATE 23P01 (`exclusion_violation`) → `Conflict(msg)`;
    /// anything else → `Database(e)` (500, same as an unhandled error).
    ///
    /// Same safety invariant as `IntoResponse`'s fallback below: `msg` is
    /// caller-supplied and is the only thing that reaches the HTTP body —
    /// no constraint name or bound value is ever surfaced.
    pub fn conflict_on_exclusion(e: sqlx::Error, msg: impl Into<String>) -> Self {
        if let Some(db_err) = e.as_database_error() {
            if db_err.code().as_deref() == Some(PG_EXCLUSION_VIOLATION) {
                return AppError::Conflict(msg.into());
            }
        }
        AppError::Database(e)
    }

    /// Like `conflict_on_unique`, but only matches when the violated
    /// constraint's name equals `constraint` — for sites where more than
    /// one unique constraint can fail on the same statement and each needs
    /// a different message. `e` is SQLSTATE 23505 *and* its constraint name
    /// is `constraint` → `Conflict(msg)`; anything else → `Database(e)`.
    pub fn conflict_on_constraint(e: sqlx::Error, constraint: &str, msg: impl Into<String>) -> Self {
        if let Some(db_err) = e.as_database_error() {
            if db_err.code().as_deref() == Some(PG_UNIQUE_VIOLATION)
                && db_err.constraint() == Some(constraint)
            {
                return AppError::Conflict(msg.into());
            }
        }
        AppError::Database(e)
    }
}

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
                    // Same rationale as the unique-violation branch above,
                    // but for an `EXCLUDE USING gist` constraint (e.g.
                    // overlapping time ranges) — any future EXCLUDE
                    // constraint automatically gets a 409 instead of a 500.
                    if db_err.code().as_deref() == Some(PG_EXCLUSION_VIOLATION) {
                        tracing::warn!(sqlstate = PG_EXCLUSION_VIOLATION, "exclusion violation");
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({ "error": "resource overlaps with an existing one" })),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `RowNotFound` is a real, common `sqlx::Error` variant that carries no
    /// `DatabaseError` — the representative "non-DB error" case for these
    /// tests. None of them touch a database.
    #[test]
    fn constraint_name_is_none_for_non_database_error() {
        assert_eq!(constraint_name(&sqlx::Error::RowNotFound), None);
    }

    #[test]
    fn conflict_on_unique_falls_back_to_database_for_non_database_error() {
        let err = AppError::conflict_on_unique(sqlx::Error::RowNotFound, "conflict");
        assert!(matches!(err, AppError::Database(sqlx::Error::RowNotFound)));
    }

    #[test]
    fn conflict_on_exclusion_falls_back_to_database_for_non_database_error() {
        let err = AppError::conflict_on_exclusion(sqlx::Error::RowNotFound, "conflict");
        assert!(matches!(err, AppError::Database(sqlx::Error::RowNotFound)));
    }

    #[test]
    fn conflict_on_constraint_falls_back_to_database_for_non_database_error() {
        let err =
            AppError::conflict_on_constraint(sqlx::Error::RowNotFound, "some_key", "conflict");
        assert!(matches!(err, AppError::Database(sqlx::Error::RowNotFound)));
    }
}
