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

pub async fn update_profile(
    db: &PgPool,
    user_id: Uuid,
    name: Option<&str>,
    phone: Option<&str>,
    avatar_url: Option<&str>,
) -> Result<User, sqlx::Error> {
    // When the user changes their phone number, `phone_verified` must be
    // reset so they go through OTP verification again. This prevents a
    // malicious user from overwriting their verified phone to anything they
    // want via /users/me.
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
            updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(user_id)
    .bind(name)
    .bind(phone)
    .bind(avatar_url)
    .fetch_one(db)
    .await
}
