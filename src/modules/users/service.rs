use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;

use super::dto::{UpdateProfileRequest, UserListResponse, UserResponse};
use super::repository;

pub async fn get_me(db: &PgPool, user_id: Uuid) -> Result<UserResponse, AppError> {
    let user = repository::find_by_id(db, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    Ok(UserResponse::from(user))
}

pub async fn update_me(
    db: &PgPool,
    user_id: Uuid,
    req: UpdateProfileRequest,
) -> Result<UserResponse, AppError> {
    let user = repository::update_profile(
        db,
        user_id,
        req.name.as_deref(),
        req.phone.as_deref(),
        req.avatar_url.as_deref(),
    )
    .await?;

    Ok(UserResponse::from(user))
}

pub async fn list_users(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<UserListResponse, AppError> {
    let limit = pagination.limit() as i64;
    let offset = pagination.offset() as i64;

    let users = repository::find_all(db, limit, offset).await?;
    let total = repository::count_all(db).await?;

    Ok(UserListResponse {
        users: users.into_iter().map(UserResponse::from).collect(),
        total,
        page: pagination.page,
        per_page: pagination.limit(),
    })
}

pub async fn get_user(db: &PgPool, user_id: Uuid) -> Result<UserResponse, AppError> {
    let user = repository::find_by_id(db, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    Ok(UserResponse::from(user))
}
