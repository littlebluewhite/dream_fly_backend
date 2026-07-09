use chrono::NaiveDate;
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
/// which self-registration does not) and an optional `birth_date` (Task
/// P4-B2 — also not on self-registration, see `users::dto::CreateUserRequest`).
pub async fn create_user_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    email: &str,
    name: &str,
    phone: Option<&str>,
    password_hash: &str,
    birth_date: Option<NaiveDate>,
) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (id, email, name, phone, password_hash, phone_verified, is_active, birth_date, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, false, true, $6, NOW(), NOW())
        RETURNING *
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(phone)
    .bind(password_hash)
    .bind(birth_date)
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

/// `PATCH /users/me` (self-service) partial update. Rewritten to a dynamic
/// `QueryBuilder` (Task P4-B2 — backend convention is "no COALESCE", see
/// `courses::repository::update`/`venues::repository::update`) so
/// `birth_date`, the one genuinely clearable field here, can distinguish
/// "don't touch" (Rust `None`) from "clear to NULL" (Rust `Some(None)`).
/// `name`/`phone`/`avatar_url`/`preferences` keep their pre-existing
/// "`None` = don't touch, `Some(v)` = set to `v`" semantics — none of them
/// support clearing to NULL (no product requirement for that today).
pub async fn update_profile(
    db: &PgPool,
    user_id: Uuid,
    name: Option<&str>,
    phone: Option<&str>,
    avatar_url: Option<&str>,
    preferences: Option<&serde_json::Value>,
    birth_date: Option<Option<NaiveDate>>,
) -> Result<User, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE users SET updated_at = now()");

    if let Some(v) = name {
        qb.push(", name = ").push_bind(v);
    }
    if let Some(v) = phone {
        // A real phone change resets `phone_verified` so the user has to
        // re-verify via OTP — stops a malicious actor from overwriting a
        // verified phone number to anything they want via /users/me. This
        // branch only runs when a phone value was actually submitted, so
        // `v` is never a SQL NULL here; `phone` on the right of `CASE`
        // reads the *pre-update* row (Postgres evaluates every SET
        // expression against the old row, regardless of clause order).
        qb.push(", phone = ").push_bind(v);
        qb.push(", phone_verified = CASE WHEN phone IS DISTINCT FROM ")
            .push_bind(v)
            .push(" THEN false ELSE phone_verified END");
    }
    if let Some(v) = avatar_url {
        qb.push(", avatar_url = ").push_bind(v);
    }
    if let Some(v) = preferences {
        qb.push(", preferences = ").push_bind(v);
    }
    if let Some(v) = birth_date {
        qb.push(", birth_date = ").push_bind(v);
    }

    qb.push(" WHERE id = ").push_bind(user_id);
    qb.push(" RETURNING *");

    qb.build_query_as::<User>().fetch_one(db).await
}
