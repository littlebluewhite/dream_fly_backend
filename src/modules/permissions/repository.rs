use sqlx::PgPool;
use uuid::Uuid;

use crate::modules::auth::access::AccessDirty;

use super::model::Role;

/// Idempotent (`ON CONFLICT DO NOTHING`) role assignment — the owner that
/// every role-granting call site (`auth::provisioning::create_account`,
/// `auth::service::google_auth`, `coaches::service::create_coach`, the seed
/// binary, and the integration-test fixtures) converges on, replacing what
/// used to be a hand-rolled `INSERT` duplicated at each site.
///
/// `Role` is a closed set, but its row lives in the `roles` table: if that
/// row is missing the CTE selects nothing and this returns
/// `Err(sqlx::Error::RowNotFound)` instead of silently granting nothing. A
/// repeated grant of a role the user already holds is still `Ok`.
///
/// Takes a bare `&mut PgConnection` rather than the `_tx` suffix — this
/// repo's `_tx` convention means "takes a `Transaction`", but this function's
/// callers include the seed binary, which has no ambient transaction. A
/// transactional caller passes `&mut tx` (deref-coerces); a plain-pool
/// caller passes a pool-`acquire`d connection. Naming follows the
/// executor-typed convention of `auth::session::start`.
///
/// Returns an [`AccessDirty`] witness — the caller MUST `.flush(cache)` it
/// after `tx.commit()` (or immediately, for a non-transactional caller with
/// no commit boundary) so the next request doesn't keep serving the user's
/// pre-assignment role set out of the access cache.
pub async fn assign_role(
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
    role: Role,
) -> Result<AccessDirty, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        WITH target_role AS (SELECT id FROM roles WHERE name = $2),
             granted AS (
                 INSERT INTO user_roles (user_id, role_id)
                 SELECT $1, id FROM target_role
                 ON CONFLICT DO NOTHING
             )
        SELECT id FROM target_role
        "#,
    )
    .bind(user_id)
    .bind(role.as_str())
    .fetch_one(conn)
    .await?;
    Ok(AccessDirty::new(user_id))
}

/// Role names only (no id/description/created_at) — used to populate
/// `roles` on `UserResponse` (auth and users DTOs). Also the DB fallback of
/// `auth::access::resolve` (what the `AuthUser` extractor reads for RBAC),
/// so JWT-derived access and response payloads never disagree on a user's
/// roles.
pub async fn find_role_names_by_user(
    executor: impl sqlx::PgExecutor<'_>,
    user_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT r.name FROM roles r JOIN user_roles ur ON ur.role_id = r.id WHERE ur.user_id = $1 ORDER BY r.name"
    ).bind(user_id).fetch_all(executor).await?;
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
