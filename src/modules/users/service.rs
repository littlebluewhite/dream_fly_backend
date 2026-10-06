use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;
use crate::modules::auth::access::{self, AccessCache};
use crate::modules::auth::provisioning as auth_provisioning;
use crate::modules::permissions::repository as permissions_repository;
use crate::utils::password;
use crate::utils::studio_clock::StudioNow;
use crate::utils::validation;

use super::dto::{
    birth_date_range_error, CreateUserRequest, UpdateProfileRequest, UpdateUserRequest,
    UserListResponse, UserResponse,
};
use super::repository;

pub async fn get_me(db: &PgPool, user_id: Uuid) -> Result<UserResponse, AppError> {
    let user = repository::find_by_id(db, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let roles = permissions_repository::find_role_names_by_user(db, user_id).await?;

    Ok(UserResponse::new(user, roles))
}

pub async fn update_me(
    db: &PgPool,
    user_id: Uuid,
    req: UpdateProfileRequest,
    at: StudioNow,
) -> Result<UserResponse, AppError> {
    // `birth_date`'s double-option can't be range-checked via `#[validate]`
    // (validator can't express nested `Option` cleanly — see
    // `dto::UpdateProfileRequest`'s doc comment), so it's checked here
    // instead. Only the "set to a date" branch is checked — clearing to
    // NULL (`Some(None)`) is always allowed.
    if let Some(Some(date)) = req.birth_date
        && let Some(msg) = birth_date_range_error(date, at.today()) {
            return Err(AppError::Validation(msg.to_string()));
        }

    let user = repository::update_profile(
        db,
        user_id,
        req.name.as_deref(),
        req.phone.as_deref(),
        req.avatar_url.as_deref(),
        req.preferences.as_ref(),
        req.birth_date,
    )
    .await?;

    let roles = permissions_repository::find_role_names_by_user(db, user_id).await?;

    Ok(UserResponse::new(user, roles))
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
            UserResponse::new(u, roles)
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

    Ok(UserResponse::new(user, roles))
}

/// `POST /users` (admin). Builds the account through the same owner
/// `auth::service::register` uses (`auth::provisioning::create_account`) —
/// Argon2 hash, `is_active = true`, assign the `member` role, and (Task 6C)
/// queue a `user_registered` outbox event, all inside one transaction so a
/// partial failure can never leave an orphaned/inconsistent row. Unlike
/// `register`, this endpoint still does not issue a session or send a
/// welcome notification — an admin-created account has no session of its
/// own to hand back, and the account wasn't the user's own registration
/// action to welcome them for.
pub async fn create_user(
    db: &PgPool,
    req: CreateUserRequest,
    correlation_id: Option<String>,
    at: StudioNow,
) -> Result<UserResponse, AppError> {
    if let Some(date) = req.birth_date
        && let Some(msg) = birth_date_range_error(date, at.today()) {
            return Err(validation::field_error("birth_date", msg));
        }
    let hashed = password::hash_for_storage(req.password.clone()).await?;

    let mut tx = db.begin().await?;

    let user = auth_provisioning::create_account(
        &mut tx,
        auth_provisioning::NewAccount {
            email: &req.email,
            name: &req.name,
            phone: req.phone.as_deref(),
            birth_date: req.birth_date,
            password_hash: &hashed,
        },
        correlation_id,
        at.now,
    )
    .await
    .map_err(|e| AppError::conflict_on_unique(e, "Email 已被使用"))?;

    let roles = permissions_repository::find_role_names_by_user(&mut *tx, user.id).await?;

    tx.commit().await?;

    Ok(UserResponse::new(user, roles))
}

/// `PATCH /users/{id}` (admin). `name`/`phone`/`is_active` only — `email`,
/// roles, and `password` are out of v1 scope for this endpoint and simply
/// aren't fields on `UpdateUserRequest`, so a body that includes them is
/// silently ignored rather than rejected.
///
/// `is_active` goes through `auth::access::{deactivate_tx, reactivate_tx}`
/// in the same tx as the field update; the returned witness is flushed after
/// commit, so a disable revokes the user's live access tokens on their very
/// next request. Deactivation also ends the whole refresh-token family, so
/// reactivating the account later does not bring the old refresh tokens
/// back.
pub async fn admin_update_user(
    db: &PgPool,
    cache: &dyn AccessCache,
    user_id: Uuid,
    req: UpdateUserRequest,
) -> Result<UserResponse, AppError> {
    if req.name.is_none() && req.phone.is_none() && req.is_active.is_none() {
        return Err(AppError::Validation("至少提供一個欄位".into()));
    }

    let mut tx = db.begin().await?;

    // Before `admin_update`, so its `RETURNING *` already carries the new
    // `is_active`. An unknown id updates nothing here and 404s just below,
    // rolling the tx back.
    let dirty = match req.is_active {
        Some(false) => Some(access::deactivate_tx(&mut tx, user_id).await?),
        Some(true) => Some(access::reactivate_tx(&mut tx, user_id).await?),
        None => None,
    };

    let user =
        repository::admin_update(&mut *tx, user_id, req.name.as_deref(), req.phone.as_deref())
            .await?
            .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    tx.commit().await?;

    if let Some(dirty) = dirty {
        dirty.flush(cache).await;
    }

    let roles = permissions_repository::find_role_names_by_user(db, user_id).await?;

    Ok(UserResponse::new(user, roles))
}
