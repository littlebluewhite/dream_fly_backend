use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::{Validate, ValidationError};

use super::model::User;
use crate::extractors::pagination::PageMeta;
use crate::utils::double_option::deserialize_some;
use crate::utils::url_validation::validate_stored_url;

/// Earliest/latest `birth_date` accepted by both `CreateUserRequest` and
/// `UpdateProfileRequest` — Round 4 Task P4-B2. Returns `Some(message)` when
/// `date` falls outside `[1900-01-01, today]`; `None` when it's in range.
/// Shared so the two call sites (a validator-crate custom function below for
/// the plain-`Option` create path, and a manual check in
/// `service::update_me` for the double-option patch path — `validator` can't
/// express nested `Option` cleanly, same limitation noted on
/// `venues::dto::UpdateVenueRequest`'s double-option fields) can't drift.
pub(crate) fn birth_date_range_error(date: NaiveDate) -> Option<&'static str> {
    let min = NaiveDate::from_ymd_opt(1900, 1, 1).expect("1900-01-01 is a valid date");
    if date < min {
        return Some("birth_date must not be before 1900-01-01");
    }
    if date > Utc::now().date_naive() {
        return Some("birth_date cannot be in the future");
    }
    None
}

fn validate_birth_date(date: &NaiveDate) -> Result<(), ValidationError> {
    match birth_date_range_error(*date) {
        Some(msg) => {
            let mut err = ValidationError::new("birth_date_out_of_range");
            err.message = Some(msg.into());
            Err(err)
        }
        None => Ok(()),
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateProfileRequest {
    #[validate(length(min = 2, max = 100))]
    pub name: Option<String>,
    #[validate(length(min = 8, max = 20))]
    pub phone: Option<String>,
    #[validate(custom(function = "validate_stored_url"))]
    pub avatar_url: Option<String>,
    /// Round 4 Task B7 — member preference bag (mobile settings toggles:
    /// `class_reminder`/`coach_msg`/`promo`/`dark`, see
    /// docs/api/integration-contract.md §3.2). Whole-object overwrite when
    /// present (no deep merge, no per-key validation — a generic bag);
    /// omitted (`None`) leaves the stored value untouched.
    pub preferences: Option<serde_json::Value>,
    /// Round 4 Task P4-B2 — member-editable birth date (feeds a future
    /// age-bracket report). `Option<Option<NaiveDate>>` double-option
    /// (paired with `deserialize_some`) distinguishes "don't touch"
    /// (`None`), "clear to NULL" (`Some(None)`), and "set to date"
    /// (`Some(Some(d))`) — mirrors `venues::dto::UpdateVenueRequest`. No
    /// `#[validate]` here (validator can't express nested `Option`
    /// cleanly); range-checked in `service::update_me` instead, only on the
    /// `Some(Some(_))` branch — clearing to NULL is always allowed and
    /// never range-checked.
    #[serde(default, deserialize_with = "deserialize_some")]
    pub birth_date: Option<Option<NaiveDate>>,
}

/// `POST /users` (admin) — creates a member account. Mirrors
/// `auth::dto::RegisterRequest`'s field bounds (email/name/password), plus
/// an optional `phone` the admin-creation flow supports that self-registration
/// does not.
#[derive(Debug, Deserialize, Validate)]
pub struct CreateUserRequest {
    #[validate(email, length(max = 256))]
    pub email: String,
    #[validate(length(min = 2, max = 100))]
    pub name: String,
    #[validate(length(min = 8, max = 20))]
    pub phone: Option<String>,
    #[validate(length(min = 8, max = 128))]
    pub password: String,
    /// Round 4 Task P4-B2 — optional at admin-creation time. Deliberately
    /// NOT on `auth::dto::RegisterRequest`: self-registration keeps this
    /// field out entirely to minimize signup friction (see
    /// docs/api/integration-contract.md §3.2). Range-validated 1900-01-01
    /// to today via `validate_birth_date`.
    #[validate(custom(function = "validate_birth_date"))]
    pub birth_date: Option<NaiveDate>,
}

/// `PATCH /users/{id}` (admin) — partial update of a member's own-profile
/// fields. Deliberately excludes `email`/`roles`/`password`: those are out
/// of v1 scope for this endpoint (see docs/api/integration-contract.md §3.2).
/// At least one field must be present — enforced in `service::admin_update_user`
/// since `validator` can't express "not all fields are None" declaratively.
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateUserRequest {
    #[validate(length(min = 2, max = 100))]
    pub name: Option<String>,
    #[validate(length(min = 8, max = 20))]
    pub phone: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub phone: Option<String>,
    pub phone_verified: bool,
    pub avatar_url: Option<String>,
    pub is_active: bool,
    pub last_login: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub roles: Vec<String>,
    pub points_balance: i64,
    /// Round 4 Task B7. `UserResponse` is the one response type the users
    /// module uses for both the self-service profile (`GET`/`PATCH
    /// /users/me`) and admin-facing views (`GET /users`, `GET /users/{id}`,
    /// `POST /users`, `PATCH /users/{id}`) — there is no separate admin-view
    /// DTO to exclude this from, so it appears in all of them. Only
    /// `PATCH /users/me` can write it.
    pub preferences: Option<serde_json::Value>,
    /// Round 4 Task P4-B2. `null` until set via `PATCH /users/me` or
    /// `POST /users` (admin) — self-registered accounts start out `null`
    /// since `POST /auth/register` never writes this column.
    pub birth_date: Option<NaiveDate>,
}

impl UserResponse {
    /// Roles are a required constructor argument rather than a
    /// `..Default::default()`-style fill-in: forgetting to load them now
    /// fails to compile instead of silently shipping an empty `roles: []`.
    pub fn new(user: User, roles: Vec<String>) -> Self {
        Self {
            id: user.id,
            email: user.email,
            name: user.name,
            phone: user.phone,
            phone_verified: user.phone_verified,
            avatar_url: user.avatar_url,
            is_active: user.is_active,
            last_login: user.last_login,
            created_at: user.created_at,
            roles,
            points_balance: user.points_balance,
            preferences: user.preferences,
            birth_date: user.birth_date,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UserListResponse {
    pub users: Vec<UserResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_user() -> User {
        User {
            id: Uuid::new_v4(),
            email: "owner-test@example.com".into(),
            name: "Owner Test".into(),
            phone: None,
            phone_verified: false,
            avatar_url: None,
            password_hash: None,
            google_id: None,
            is_active: true,
            last_login: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            points_balance: 0,
            preferences: None,
            birth_date: None,
        }
    }

    /// The invariant this constructor exists to guarantee: roles passed in
    /// must actually reach the serialized wire output, not just the struct
    /// field (a JSON-level assertion catches a `#[serde(skip)]`-style bug
    /// that a struct-field-only assertion would miss).
    #[test]
    fn new_puts_roles_into_serialized_output() {
        let response = UserResponse::new(test_user(), vec!["member".into(), "coach".into()]);

        let json = serde_json::to_value(&response).expect("serialize UserResponse");
        assert_eq!(json["roles"], serde_json::json!(["member", "coach"]));
    }
}
