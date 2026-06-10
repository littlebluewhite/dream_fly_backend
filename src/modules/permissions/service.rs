use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::invalidate_role_cache;

use super::dto::{PermissionResponse, RoleResponse, RoleWithPermissionsResponse};
use super::repository;

pub async fn list_roles(db: &PgPool) -> Result<Vec<RoleResponse>, AppError> {
    let roles = repository::find_all_roles(db).await?;
    Ok(roles
        .into_iter()
        .map(|r| RoleResponse {
            id: r.id,
            name: r.name,
            description: r.description,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn get_role_with_permissions(
    db: &PgPool,
    role_id: Uuid,
) -> Result<RoleWithPermissionsResponse, AppError> {
    let role = repository::find_role_by_id(db, role_id)
        .await?
        .ok_or_else(|| AppError::NotFound("role not found".into()))?;

    let permissions = repository::find_permissions_for_role(db, role_id).await?;

    Ok(RoleWithPermissionsResponse {
        id: role.id,
        name: role.name,
        description: role.description,
        permissions: permissions
            .into_iter()
            .map(|p| PermissionResponse {
                id: p.id,
                resource: p.resource,
                action: p.action,
            })
            .collect(),
    })
}

pub async fn create_role(
    db: &PgPool,
    name: &str,
    description: Option<&str>,
) -> Result<RoleResponse, AppError> {
    let role = repository::create_role(db, name, description)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.constraint() == Some("roles_name_key") {
                    return AppError::Conflict(format!("role '{}' already exists", name));
                }
            }
            AppError::Database(e)
        })?;

    Ok(RoleResponse {
        id: role.id,
        name: role.name,
        description: role.description,
        created_at: role.created_at,
    })
}

pub async fn assign_role_to_user(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    user_id: Uuid,
    role_id: Uuid,
) -> Result<(), AppError> {
    // Verify role exists
    repository::find_role_by_id(db, role_id)
        .await?
        .ok_or_else(|| AppError::NotFound("role not found".into()))?;

    repository::assign_role_to_user(db, user_id, role_id).await?;

    // Invalidate the Redis role cache so the next request reloads from DB.
    invalidate_role_cache(redis, user_id).await;
    Ok(())
}

pub async fn remove_role_from_user(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    user_id: Uuid,
    role_id: Uuid,
) -> Result<(), AppError> {
    repository::remove_role_from_user(db, user_id, role_id).await?;

    // Invalidate the Redis role cache so the next request reloads from DB.
    invalidate_role_cache(redis, user_id).await;
    Ok(())
}
