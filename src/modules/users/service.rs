use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::revoke_user;
use crate::extractors::pagination::PaginationParams;
use crate::modules::auth::repository as auth_repository;
use crate::modules::permissions::repository as permissions_repository;
use crate::utils::password;

use super::dto::{CreateUserRequest, UpdateProfileRequest, UpdateUserRequest, UserListResponse, UserResponse};
use super::repository;

pub async fn get_me(db: &PgPool, user_id: Uuid) -> Result<UserResponse, AppError> {
    let user = repository::find_by_id(db, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let roles = permissions_repository::find_role_names_by_user(db, user_id).await?;

    Ok(UserResponse {
        roles,
        ..UserResponse::from(user)
    })
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

    let roles = permissions_repository::find_role_names_by_user(db, user_id).await?;

    Ok(UserResponse {
        roles,
        ..UserResponse::from(user)
    })
}

pub async fn list_users(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<UserListResponse, AppError> {
    let limit = pagination.limit() as i64;
    let offset = pagination.offset() as i64;

    let users = repository::find_all(db, limit, offset).await?;
    let total = repository::count_all(db).await?;

    // Single grouped query for the whole page instead of N+1 per-user lookups.
    let user_ids: Vec<Uuid> = users.iter().map(|u| u.id).collect();
    let mut roles_by_user =
        permissions_repository::find_role_names_for_users(db, &user_ids).await?;

    let users = users
        .into_iter()
        .map(|u| {
            let roles = roles_by_user.remove(&u.id).unwrap_or_default();
            UserResponse {
                roles,
                ..UserResponse::from(u)
            }
        })
        .collect();

    Ok(UserListResponse {
        users,
        meta: pagination.meta(total),
    })
}

pub async fn get_user(db: &PgPool, user_id: Uuid) -> Result<UserResponse, AppError> {
    let user = repository::find_by_id(db, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let roles = permissions_repository::find_role_names_by_user(db, user_id).await?;

    Ok(UserResponse {
        roles,
        ..UserResponse::from(user)
    })
}

/// `POST /users` (admin). Builds the account the same way
/// `auth::service::register` does — Argon2 hash, `is_active = true`, assign
/// the `member` role, all inside one transaction so a role-assignment
/// failure can never leave an orphaned user row — but skips the
/// tokens/outbox/welcome-notification side effects register does, since an
/// admin-created account has no session of its own to hand back.
pub async fn create_user(db: &PgPool, req: CreateUserRequest) -> Result<UserResponse, AppError> {
    let email = req.email.to_lowercase();

    let hashed = password::hash_password(req.password.clone())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("password hash error: {e}")))?;

    let mut tx = db.begin().await?;

    let user = match repository::create_user_tx(&mut tx, &email, &req.name, req.phone.as_deref(), &hashed)
        .await
    {
        Ok(u) => u,
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            return Err(AppError::Conflict("Email 已被使用".into()));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

    auth_repository::assign_role_tx(&mut tx, user.id, "member").await?;

    let roles = permissions_repository::find_role_names_by_user_tx(&mut tx, user.id).await?;

    tx.commit().await?;

    Ok(UserResponse {
        roles,
        ..UserResponse::from(user)
    })
}

/// `PATCH /users/{id}` (admin). `name`/`phone`/`is_active` only — `email`,
/// roles, and `password` are out of v1 scope for this endpoint and simply
/// aren't fields on `UpdateUserRequest`, so a body that includes them is
/// silently ignored rather than rejected.
///
/// When `is_active` is part of the request, invalidates the target user's
/// `user_active`/`user_roles` Redis cache (`extractors::auth::revoke_user`)
/// so a disable takes effect immediately instead of waiting out the
/// extractor's 60s cache TTL.
pub async fn admin_update_user(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    user_id: Uuid,
    req: UpdateUserRequest,
) -> Result<UserResponse, AppError> {
    if req.name.is_none() && req.phone.is_none() && req.is_active.is_none() {
        return Err(AppError::Validation("至少提供一個欄位".into()));
    }

    let user = repository::admin_update(
        db,
        user_id,
        req.name.as_deref(),
        req.phone.as_deref(),
        req.is_active,
    )
    .await?
    .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    if req.is_active.is_some() {
        revoke_user(redis, user_id).await;
    }

    let roles = permissions_repository::find_role_names_by_user(db, user_id).await?;

    Ok(UserResponse {
        roles,
        ..UserResponse::from(user)
    })
}
