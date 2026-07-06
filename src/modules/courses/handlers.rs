use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{CourseDetailResponse, CourseListResponse, CreateCourseRequest, UpdateCourseRequest};
use super::service;

#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<CourseListResponse>, AppError> {
    let result = service::list_courses(&state.db, &params).await?;
    Ok(Json(result))
}

#[tracing::instrument(skip_all)]
pub async fn get_by_slug_or_id(
    State(state): State<AppState>,
    Path(param): Path<String>,
) -> Result<Json<CourseDetailResponse>, AppError> {
    let course = service::get_course_by_slug_or_id(&state.db, &param).await?;
    Ok(Json(course))
}

#[tracing::instrument(skip_all)]
pub async fn create(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateCourseRequest>,
) -> Result<Json<CourseDetailResponse>, AppError> {
    auth.require_role("admin")?;
    let course = service::create_course(&state.db, req).await?;
    Ok(Json(course))
}

#[tracing::instrument(skip_all)]
pub async fn update(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id_str): Path<String>,
    ValidatedJson(req): ValidatedJson<UpdateCourseRequest>,
) -> Result<Json<CourseDetailResponse>, AppError> {
    auth.require_role("admin")?;
    let id: Uuid = id_str
        .parse()
        .map_err(|_| AppError::BadRequest("invalid course id".into()))?;
    let course = service::update_course(&state.db, id, req).await?;
    Ok(Json(course))
}
