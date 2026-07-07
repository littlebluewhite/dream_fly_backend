use axum::{
    Json,
    extract::{FromRequestParts, Path, Query, State},
    http::{StatusCode, request::Parts},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{CreateWaitlistRequest, WaitlistQuery, WaitlistResponse};
use super::service;

/// Extracts `WaitlistQuery` directly (rather than the handler taking
/// `Query<WaitlistQuery>`) so a missing/invalid `course_id` maps to
/// `AppError::Validation` (422) instead of axum's default `QueryRejection`
/// (400) — the admin-list endpoint's contract requires 422.
impl<S: Send + Sync> FromRequestParts<S> for WaitlistQuery {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<Self>::from_request_parts(parts, state)
            .await
            .map(|Query(q)| q)
            .map_err(|_| AppError::Validation("course_id query parameter is required".into()))
    }
}

#[tracing::instrument(skip_all)]
pub async fn join(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateWaitlistRequest>,
) -> Result<Json<WaitlistResponse>, AppError> {
    let entry = service::join_waitlist(&state.db, auth.user_id, req.course_id).await?;
    Ok(Json(entry))
}

/// This user's waitlist entries, newest first (plain array, not paginated).
#[tracing::instrument(skip_all)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<WaitlistResponse>>, AppError> {
    let entries = service::list_my_waitlist(&state.db, auth.user_id).await?;
    Ok(Json(entries))
}

/// Cancel a waitlist entry (owner or admin).
#[tracing::instrument(skip_all)]
pub async fn cancel(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    service::cancel_waitlist_entry(&state.db, &auth, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Waiting entries for a course, oldest first (admin only).
#[tracing::instrument(skip_all)]
pub async fn list_for_course(
    State(state): State<AppState>,
    auth: AuthUser,
    params: WaitlistQuery,
) -> Result<Json<Vec<WaitlistResponse>>, AppError> {
    auth.require_role("admin")?;
    let entries = service::list_for_course(&state.db, params.course_id).await?;
    Ok(Json(entries))
}
