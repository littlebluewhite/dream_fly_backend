use chrono::{DateTime, NaiveDate, Utc};
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
    /// Added by Round 4 Task B7 (`users.preferences JSONB`, nullable, no
    /// default). Free-form member preference bag (mobile settings toggles)
    /// — `NULL` until the user's first `PATCH /users/me` that includes it.
    pub preferences: Option<serde_json::Value>,
    /// Added by Round 4 Task P4-B1 migration (`users.birth_date DATE`,
    /// nullable, no default); write path wired up in Task P4-B2
    /// (`users::dto::CreateUserRequest`/`UpdateProfileRequest`). Every
    /// `query_as::<_, User>` call in this codebase selects via `SELECT *` /
    /// `RETURNING *`, so adding this field here is safe (verified by grep,
    /// same as `points_balance` above). `POST /auth/register` deliberately
    /// never writes this column — it stays `NULL` for self-registered
    /// accounts until they set it via `PATCH /users/me`.
    pub birth_date: Option<NaiveDate>,
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

/// Normalizes an account-identity email for case-insensitive lookup and
/// storage. Used by every call site where email IS the account identity:
/// `register`, `login`, `google_auth`, `forgot_password` (all in
/// `auth::service`), and `create_user` (`users::service`). Deliberately NOT
/// used by `contact::model` — an inquiry's email is not an account identity
/// and is left un-normalized on purpose.
///
/// Deliberately does NOT trim surrounding whitespace — see
/// `normalize_email_does_not_trim_whitespace` below for the executable
/// documentation of that choice.
pub fn normalize_email(email: &str) -> String {
    email.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_email_lowercases_mixed_case() {
        assert_eq!(normalize_email("User@Example.COM"), "user@example.com");
    }

    #[test]
    fn normalize_email_is_idempotent_on_already_normalized_input() {
        let normalized = normalize_email("already@lower.case");
        assert_eq!(normalize_email(&normalized), normalized);
    }

    /// Executable documentation of the "no trim" decision called out in the
    /// doc comment above: surrounding whitespace survives normalization
    /// unchanged, matching every call site (none of which trims either).
    #[test]
    fn normalize_email_does_not_trim_whitespace() {
        assert_eq!(
            normalize_email("  User@Example.com  "),
            "  user@example.com  "
        );
    }
}
