use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::extractors::auth::RoleCacheDirty;

use super::model::{RefreshToken, User};

pub async fn create_user_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    email: &str,
    name: &str,
    password_hash: &str,
) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (id, email, name, password_hash, phone_verified, is_active, created_at, updated_at)
        VALUES ($1, $2, $3, $4, false, true, NOW(), NOW())
        RETURNING *
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(password_hash)
    .fetch_one(&mut **tx)
    .await
}

pub async fn find_user_by_email(
    executor: impl sqlx::PgExecutor<'_>,
    email: &str,
) -> Result<Option<User>, sqlx::Error> {
    // The email column is CITEXT — comparisons are already case-insensitive,
    // so we match directly without LOWER() to let the UNIQUE index be used.
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(executor)
        .await
}

pub async fn find_user_by_google_id(
    db: &PgPool,
    google_id: &str,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE google_id = $1")
        .bind(google_id)
        .fetch_optional(db)
        .await
}

pub async fn update_last_login(
    executor: impl sqlx::PgExecutor<'_>,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET last_login = NOW(), updated_at = NOW() WHERE id = $1")
        .bind(user_id)
        .execute(executor)
        .await?;
    Ok(())
}

pub async fn save_refresh_token(
    executor: impl sqlx::PgExecutor<'_>,
    user_id: Uuid,
    token_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at, revoked, created_at)
        VALUES ($1, $2, $3, $4, false, NOW())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(executor)
    .await?;
    Ok(())
}

pub async fn find_refresh_token_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    token_hash: &str,
) -> Result<Option<RefreshToken>, sqlx::Error> {
    sqlx::query_as::<_, RefreshToken>(
        "SELECT * FROM refresh_tokens WHERE token_hash = $1 FOR UPDATE",
    )
    .bind(token_hash)
    .fetch_optional(&mut **tx)
    .await
}

pub async fn revoke_refresh_token(
    executor: impl sqlx::PgExecutor<'_>,
    token_hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE token_hash = $1")
        .bind(token_hash)
        .execute(executor)
        .await?;
    Ok(())
}

pub async fn revoke_all_user_tokens_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE user_id = $1 AND revoked = false")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn find_user_by_id_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
}

pub async fn update_phone_verified(
    db: &PgPool,
    user_id: Uuid,
    phone: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET phone_verified = true, phone = $2, updated_at = NOW() WHERE id = $1",
    )
    .bind(user_id)
    .bind(phone)
    .execute(db)
    .await?;
    Ok(())
}

/// Assign `role_name` to `user_id` inside an already-open transaction.
/// Returns a [`RoleCacheDirty`] witness — the caller MUST `.flush(redis)` it
/// after `tx.commit()` so the next request doesn't keep serving the user's
/// pre-assignment role set out of the Redis cache.
pub async fn assign_role_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    role_name: &str,
) -> Result<RoleCacheDirty, sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO user_roles (user_id, role_id)
        SELECT $1, id FROM roles WHERE name = $2
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(role_name)
    .execute(&mut **tx)
    .await?;
    Ok(RoleCacheDirty::new(user_id))
}

pub async fn update_password_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    password_hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = NOW() WHERE id = $1")
        .bind(user_id)
        .bind(password_hash)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub async fn create_or_update_google_user_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    email: &str,
    name: &str,
    google_id: &str,
    avatar_url: Option<&str>,
) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (id, email, name, google_id, avatar_url, phone_verified, is_active, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, false, true, NOW(), NOW())
        ON CONFLICT (google_id) DO UPDATE
        SET email = EXCLUDED.email,
            name = EXCLUDED.name,
            avatar_url = COALESCE(EXCLUDED.avatar_url, users.avatar_url),
            updated_at = NOW()
        RETURNING *
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(name)
    .bind(google_id)
    .bind(avatar_url)
    .fetch_one(&mut **tx)
    .await
}

pub async fn link_google_account_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    google_id: &str,
    avatar_url: Option<&str>,
) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "UPDATE users SET google_id = $2, avatar_url = COALESCE($3, avatar_url), \
         updated_at = NOW() WHERE id = $1 RETURNING *",
    )
    .bind(user_id)
    .bind(google_id)
    .bind(avatar_url)
    .fetch_one(&mut **tx)
    .await
}

pub async fn delete_expired_tokens(db: &PgPool) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM refresh_tokens WHERE expires_at < NOW() OR revoked = true")
            .execute(db)
            .await?;
    Ok(result.rows_affected())
}
