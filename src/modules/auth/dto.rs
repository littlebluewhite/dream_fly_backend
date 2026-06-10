use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::User;

#[derive(Debug, Deserialize, Validate)]
pub struct RegisterRequest {
    #[validate(email, length(max = 256))]
    pub email: String,
    #[validate(length(min = 2, max = 100))]
    pub name: String,
    // Upper bound prevents a 10MB password from DOSing Argon2. HIBP-style
    // pwned-password checks should live at the DTO layer if enabled.
    #[validate(length(min = 8, max = 128))]
    pub password: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email)]
    pub email: String,
    #[validate(length(min = 1, max = 256))]
    pub password: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct GoogleAuthRequest {
    #[validate(length(min = 1, max = 2048))]
    pub code: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct RefreshRequest {
    #[validate(length(min = 1, max = 4096))]
    pub refresh_token: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct OtpSendRequest {
    #[validate(length(min = 8, max = 20))]
    pub phone: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct OtpVerifyRequest {
    #[validate(length(min = 8, max = 20))]
    pub phone: String,
    #[validate(length(equal = 6))]
    pub code: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct ForgotPasswordRequest {
    #[validate(email)]
    pub email: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct ResetPasswordRequest {
    #[validate(length(min = 1, max = 512))]
    pub token: String,
    #[validate(length(min = 8, max = 128))]
    pub new_password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserResponse,
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
    pub created_at: DateTime<Utc>,
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
            created_at: user.created_at,
        }
    }
}

// Re-export the shared MessageResponse for backwards-compatible imports.
pub use crate::error::MessageResponse;
