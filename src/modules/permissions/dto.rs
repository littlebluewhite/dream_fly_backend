use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Deserialize, Validate)]
pub struct CreateRoleRequest {
    #[validate(length(min = 1, max = 50))]
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, Validate)]
pub struct AssignRoleRequest {
    pub user_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct RoleResponse {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct RoleWithPermissionsResponse {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub permissions: Vec<PermissionResponse>,
}

#[derive(Debug, Serialize)]
pub struct PermissionResponse {
    pub id: Uuid,
    pub resource: String,
    pub action: String,
}

// Re-export the shared MessageResponse for backwards-compatible imports.
pub use crate::error::MessageResponse;
