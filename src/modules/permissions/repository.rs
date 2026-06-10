use sqlx::PgPool;
use uuid::Uuid;

use super::model::{Permission, Role};

pub async fn find_all_roles(db: &PgPool) -> Result<Vec<Role>, sqlx::Error> {
    sqlx::query_as::<_, Role>("SELECT id, name, description, created_at FROM roles ORDER BY name")
        .fetch_all(db)
        .await
}

pub async fn find_role_by_id(db: &PgPool, id: Uuid) -> Result<Option<Role>, sqlx::Error> {
    sqlx::query_as::<_, Role>(
        "SELECT id, name, description, created_at FROM roles WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn create_role(
    db: &PgPool,
    name: &str,
    description: Option<&str>,
) -> Result<Role, sqlx::Error> {
    sqlx::query_as::<_, Role>(
        "INSERT INTO roles (id, name, description, created_at) \
         VALUES (gen_random_uuid(), $1, $2, NOW()) RETURNING *",
    )
    .bind(name)
    .bind(description)
    .fetch_one(db)
    .await
}

pub async fn find_permissions_for_role(
    db: &PgPool,
    role_id: Uuid,
) -> Result<Vec<Permission>, sqlx::Error> {
    sqlx::query_as::<_, Permission>(
        "SELECT p.id, p.resource, p.action, p.created_at \
         FROM permissions p \
         JOIN role_permissions rp ON p.id = rp.permission_id \
         WHERE rp.role_id = $1 \
         ORDER BY p.resource, p.action",
    )
    .bind(role_id)
    .fetch_all(db)
    .await
}

pub async fn assign_role_to_user(
    db: &PgPool,
    user_id: Uuid,
    role_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(role_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn remove_role_from_user(
    db: &PgPool,
    user_id: Uuid,
    role_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2")
        .bind(user_id)
        .bind(role_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn find_user_roles(db: &PgPool, user_id: Uuid) -> Result<Vec<Role>, sqlx::Error> {
    sqlx::query_as::<_, Role>(
        "SELECT r.id, r.name, r.description, r.created_at \
         FROM roles r \
         JOIN user_roles ur ON r.id = ur.role_id \
         WHERE ur.user_id = $1 \
         ORDER BY r.name",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}
