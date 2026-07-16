use sqlx::PgPool;
use uuid::Uuid;

use crate::extractors::auth::RoleCacheDirty;

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

/// Returns a [`RoleCacheDirty`] witness — the caller MUST `.flush(redis)` it
/// so the next request doesn't keep serving the user's pre-assignment role
/// set out of the Redis cache.
pub async fn assign_role_to_user(
    db: &PgPool,
    user_id: Uuid,
    role_id: Uuid,
) -> Result<RoleCacheDirty, sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(role_id)
    .execute(db)
    .await?;
    Ok(RoleCacheDirty::new(user_id))
}

/// Returns a [`RoleCacheDirty`] witness — see [`assign_role_to_user`].
pub async fn remove_role_from_user(
    db: &PgPool,
    user_id: Uuid,
    role_id: Uuid,
) -> Result<RoleCacheDirty, sqlx::Error> {
    sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2")
        .bind(user_id)
        .bind(role_id)
        .execute(db)
        .await?;
    Ok(RoleCacheDirty::new(user_id))
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

/// Role names only (no id/description/created_at) — used to populate
/// `roles` on `UserResponse` (auth and users DTOs). Mirrors the query the
/// `AuthUser` extractor uses for RBAC so JWT-derived access and response
/// payloads never disagree on a user's roles.
pub async fn find_role_names_by_user(db: &PgPool, user_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT r.name FROM roles r JOIN user_roles ur ON ur.role_id = r.id WHERE ur.user_id = $1 ORDER BY r.name"
    ).bind(user_id).fetch_all(db).await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Transactional variant of [`find_role_names_by_user`] — reads roles
/// assigned earlier in the same transaction (e.g. right after
/// `assign_role_tx`) without waiting for commit.
pub async fn find_role_names_by_user_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT r.name FROM roles r JOIN user_roles ur ON ur.role_id = r.id WHERE ur.user_id = $1 ORDER BY r.name"
    ).bind(user_id).fetch_all(&mut **tx).await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Batched roles lookup for a page of users (e.g. the admin user list) — one
/// query instead of N+1. Users with no roles are simply absent from the
/// returned map.
pub async fn find_role_names_for_users(
    db: &PgPool,
    user_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Vec<String>>, sqlx::Error> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT ur.user_id, r.name FROM roles r JOIN user_roles ur ON ur.role_id = r.id WHERE ur.user_id = ANY($1) ORDER BY ur.user_id, r.name"
    ).bind(user_ids).fetch_all(db).await?;

    let mut roles_by_user: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (user_id, role_name) in rows {
        roles_by_user.entry(user_id).or_default().push(role_name);
    }
    Ok(roles_by_user)
}
