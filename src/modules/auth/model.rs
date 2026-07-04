use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Internal user row. Intentionally NOT `Serialize` — it carries
/// `password_hash` and `google_id` which must never leak through any response
/// type. All external user surfaces must go through a DTO (e.g.
/// `auth::dto::UserResponse` or `users::dto::UserResponse`).
#[derive(Debug, sqlx::FromRow, Clone)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub phone: Option<String>,
    pub phone_verified: bool,
    pub avatar_url: Option<String>,
    pub password_hash: Option<String>,
    pub google_id: Option<String>,
    pub is_active: bool,
    pub last_login: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Added by the commerce migration (`users.points_balance BIGINT NOT NULL
    /// DEFAULT 0`). Every `query_as::<_, User>` call in this codebase selects
    /// via `SELECT *` / `RETURNING *`, so adding this field here is safe
    /// (verified by grep — Task 18).
    pub points_balance: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub struct RefreshToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
}
