use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::User;
use crate::extractors::pagination::PageMeta;
use crate::utils::url_validation::validate_stored_url;

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateProfileRequest {
    #[validate(length(min = 2, max = 100))]
    pub name: Option<String>,
    #[validate(length(min = 8, max = 20))]
    pub phone: Option<String>,
    #[validate(custom(function = "validate_stored_url"))]
    pub avatar_url: Option<String>,
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
}

impl From<User> for UserResponse {
    fn from(user: User) -> Self {
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
            roles: Vec::new(),
            points_balance: user.points_balance,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UserListResponse {
    pub users: Vec<UserResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}
