use sqlx::PgPool;
use uuid::Uuid;

use super::model::User;

pub async fn find_by_id(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn find_all(
    db: &PgPool,
    limit: i64,
    offset: i64,
) -> Result<Vec<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "SELECT * FROM users ORDER BY created_at DESC LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
}

pub async fn count_all(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(db)
        .await
}

/// `POST /users` (admin) insert. Same shape as `auth::repository::create_user_tx`
/// plus an optional `phone` column (the admin-creation body accepts `phone?`,
/// which self-registration does not).
pub async fn create_user_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    email: &str,
    name: &str,
    phone: Option<&str>,
    password_hash: &str,
) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (id, email, name, phone, password_hash, phone_verified, is_active, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, false, true, NOW(), NOW())
        RETURNING *
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(phone)
    .bind(password_hash)
    .fetch_one(&mut **tx)
    .await
}

/// `PATCH /users/{id}` (admin) partial update. Returns `None` when `id`
/// doesn't exist so the service layer can 404 without a separate
/// existence-check query (mirrors `courses::repository::update`).
///
/// Resetting `phone_verified` on a real phone change mirrors `update_profile`
/// below — an admin-set phone number is exactly as unverified as a
/// self-service one until OTP confirms it.
pub async fn admin_update(
    db: &PgPool,
    user_id: Uuid,
    name: Option<&str>,
    phone: Option<&str>,
    is_active: Option<bool>,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(
        r#"
        UPDATE users
        SET name = COALESCE($2, name),
            phone = COALESCE($3, phone),
            phone_verified = CASE
                WHEN $3 IS NOT NULL AND $3 IS DISTINCT FROM phone THEN false
                ELSE phone_verified
            END,
            is_active = COALESCE($4, is_active),
            updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(name)
    .bind(phone)
    .bind(is_active)
    .fetch_optional(db)
    .await
}

pub async fn update_profile(
    db: &PgPool,
    user_id: Uuid,
    name: Option<&str>,
    phone: Option<&str>,
    avatar_url: Option<&str>,
    preferences: Option<&serde_json::Value>,
) -> Result<User, sqlx::Error> {
    // When the user changes their phone number, `phone_verified` must be
    // reset so they go through OTP verification again. This prevents a
    // malicious user from overwriting their verified phone to anything they
    // want via /users/me.
    //
    // `preferences` follows the same COALESCE convention as the other
    // columns here: `NULL` (not provided) leaves the stored JSONB value
    // untouched; a provided value replaces the whole column (no deep merge
    // — see docs/api/integration-contract.md §3.2).
    sqlx::query_as::<_, User>(
        r#"
        UPDATE users
        SET name = COALESCE($2, name),
            phone = COALESCE($3, phone),
            phone_verified = CASE
                WHEN $3 IS NOT NULL AND $3 IS DISTINCT FROM phone THEN false
                ELSE phone_verified
            END,
            avatar_url = COALESCE($4, avatar_url),
            preferences = COALESCE($5, preferences),
            updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(name)
    .bind(phone)
    .bind(avatar_url)
    .bind(preferences)
    .fetch_one(db)
    .await
}
