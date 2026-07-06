use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;

use super::dto::{EnrolmentResponse, MyEnrolmentResponse};
use super::service;

/// This user's enrolments, newest first (plain array, not paginated), each
/// with `attended`/`total` attendance stats.
#[tracing::instrument(skip_all)]
pub async fn me(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<MyEnrolmentResponse>>, AppError> {
    let enrolments = service::list_my_enrolments(&state.db, auth.user_id).await?;
    Ok(Json(enrolments))
}

/// Cancel an enrolment (owner or admin).
#[tracing::instrument(skip_all)]
pub async fn cancel(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<EnrolmentResponse>, AppError> {
    let updated = service::cancel_enrolment(&state.db, &auth, id).await?;
    Ok(Json(updated))
}
