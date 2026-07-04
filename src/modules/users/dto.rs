use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::User;
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
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}
