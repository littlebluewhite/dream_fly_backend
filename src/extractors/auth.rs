use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use redis::AsyncCommands;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;
use crate::utils::jwt;

/// Sentinel value stored in the role cache when a user has *no* roles.
/// Distinguishes "cache miss" (key absent) from "cache hit, no roles"
/// (key present with this marker), so guest users don't repeatedly miss
/// the cache and re-query the DB.
const EMPTY_ROLES_SENTINEL: &str = "\0";

/// TTL for a populated role cache entry (15 minutes). Changing requires
/// updating any external docs referencing the value.
const ROLE_CACHE_TTL_SECONDS: u64 = 900;

/// Cache key format for the RBAC Redis role cache. Both the extractor (reader)
/// and the permissions service (invalidator on role change) use this helper so
/// the key format lives in exactly one place.
pub fn role_cache_key(user_id: Uuid) -> String {
    format!("user_roles:{user_id}")
}

/// Cache key for whether a user is active. Kept short-TTL (60s) so a disabled
/// user's existing access tokens become useless within at most one minute —
/// combined with [`revoke_user`] for immediate propagation.
pub fn active_cache_key(user_id: Uuid) -> String {
    format!("user_active:{user_id}")
}

const ACTIVE_CACHE_TTL_SECONDS: i64 = 60;

/// Best-effort invalidation of a user's cached role set. Errors are swallowed
/// on purpose — cache eviction failure must never block an authz change.
pub async fn invalidate_role_cache(
    redis: &mut redis::aio::ConnectionManager,
    user_id: Uuid,
) {
    let key = role_cache_key(user_id);
    if let Err(e) = redis.del::<_, ()>(&key).await {
        tracing::warn!(%user_id, error = %e, "failed to invalidate role cache");
    }
}

/// Mark a user as revoked: clear their role and is_active caches so the next
/// request reloads from DB (and, if the user was deactivated, rejects).
/// Call this from any admin action that disables a user or forces logout.
pub async fn revoke_user(redis: &mut redis::aio::ConnectionManager, user_id: Uuid) {
    let keys: [String; 2] = [role_cache_key(user_id), active_cache_key(user_id)];
    for k in &keys {
        if let Err(e) = redis.del::<_, ()>(k).await {
            tracing::warn!(%user_id, key = %k, error = %e, "failed to invalidate user cache");
        }
    }
}

/// Witness that a `user_roles` write has not yet been reflected in the Redis
/// role cache. Every repository function that INSERTs/DELETEs a `user_roles`
/// row returns this instead of `()`, so "forgot to invalidate the cache
/// after a role change" — previously a convention a caller had to remember —
/// becomes a `#[must_use]` compiler warning instead of a silent stale-cache
/// bug (this happened once for real).
///
/// Call [`flush`](Self::flush) AFTER the writing transaction commits, never
/// before: invalidating pre-commit would let a request racing between the
/// DEL and the commit re-populate the cache with the pre-write value.
///
/// Known residual race (out of scope for this type): a concurrent request
/// can read the DB for the old role set and `SET EX` it back into the cache
/// *after* this type's `flush()` has already run its DEL (see the
/// read-then-refill branch in [`FromRequestParts for AuthUser`](AuthUser),
/// below). The stale entry then lives out its full TTL
/// (`ROLE_CACHE_TTL_SECONDS`, 900s) before self-correcting. This type only
/// closes the "nobody invalidated at all" class of bug, not that
/// read-refill window.
#[must_use]
pub struct RoleCacheDirty(Uuid);

impl RoleCacheDirty {
    /// Construct the witness. `pub(crate)` (not private) — `RoleCacheDirty`
    /// is returned by repository functions in sibling modules
    /// (`auth::repository`, `permissions::repository`), which need to build
    /// one after a successful `user_roles` write.
    pub(crate) fn new(user_id: Uuid) -> Self {
        Self(user_id)
    }

    /// Consume the witness and perform the actual cache invalidation.
    pub async fn flush(self, redis: &mut redis::aio::ConnectionManager) {
        invalidate_role_cache(redis, self.0).await;
    }
}

#[derive(Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub email: String,
    pub roles: Vec<String>,
}

impl AuthUser {
    pub fn require_role(&self, role: &str) -> Result<(), AppError> {
        if self.roles.iter().any(|r| r == role) {
            Ok(())
        } else {
            Err(AppError::Forbidden("insufficient permissions".into()))
        }
    }

    pub fn require_any_role(&self, roles: &[&str]) -> Result<(), AppError> {
        if roles.iter().any(|r| self.roles.iter().any(|ur| ur == r)) {
            Ok(())
        } else {
            Err(AppError::Forbidden("insufficient permissions".into()))
        }
    }

    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == "admin")
    }

    /// 資源所有權授權原語:呼叫者是 `owner_id` 本人或 admin 才放行,否則回傳
    /// `Err(AppError::Forbidden(forbidden_msg))`。文案由呼叫端傳入,讓各站點
    /// 收斂到同一判斷邏輯的同時仍保留各自逐字的錯誤訊息(先例見
    /// `coaches::service::require_course_coach`)。
    pub fn owns_or_admin(&self, owner_id: Uuid, forbidden_msg: &str) -> Result<(), AppError> {
        if self.user_id == owner_id || self.is_admin() {
            Ok(())
        } else {
            Err(AppError::Forbidden(forbidden_msg.into()))
        }
    }

    /// 非 owner 且非 admin → NotFound(not_found_msg):對外遮蔽資源存在性
    pub fn owns_or_admin_masked(&self, owner_id: Uuid, not_found_msg: &str) -> Result<(), AppError> {
        if self.user_id == owner_id || self.is_admin() {
            Ok(())
        } else {
            Err(AppError::NotFound(not_found_msg.into()))
        }
    }

    /// 僅資源本人可通過;admin 亦不可代 → 否則 Forbidden(forbidden_msg)
    pub fn owner_only(&self, owner_id: Uuid, forbidden_msg: &str) -> Result<(), AppError> {
        if self.user_id == owner_id {
            Ok(())
        } else {
            Err(AppError::Forbidden(forbidden_msg.into()))
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 0. 快路徑:`require_admin` 閘門已在 route 層驗證過 token/角色並把
        //    `AuthUser` 注入 request extensions。命中即 clone 回傳,handler 端
        //    的 extractor 不再重打一次 Redis/DB(閘門後所有 admin handler 皆
        //    走此路)。閘門外的端點 extensions 無此值,照常走下方完整流程。
        if let Some(cached) = parts.extensions.get::<AuthUser>() {
            return Ok(cached.clone());
        }

        // 1. Extract "Authorization: Bearer <token>" header
        let auth_header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let token = auth_header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthorized)?;

        // 2. Decode JWT (signature + exp + aud + iss)
        let claims = jwt::decode_access_token(&state.config.auth, token)?;

        // 3. Parse user_id from claims.sub
        let user_id: Uuid = claims.sub.parse().map_err(|_| AppError::Unauthorized)?;

        let mut redis_conn = state.redis.clone();

        // 4. Check is_active. We cache the flag for 60s so the steady-state
        //    hot path is a single Redis GET, but a disabled user still loses
        //    access within at most one minute (and immediately if an admin
        //    action calls `revoke_user`).
        let active_key = active_cache_key(user_id);
        let active_cached: Option<String> = redis_conn.get(&active_key).await.ok();

        let is_active = match active_cached.as_deref() {
            Some("1") => true,
            Some("0") => false,
            _ => {
                let row: Option<(bool,)> =
                    sqlx::query_as("SELECT is_active FROM users WHERE id = $1")
                        .bind(user_id)
                        .fetch_optional(&state.db)
                        .await
                        .map_err(AppError::Database)?;
                let active = row.map(|r| r.0).unwrap_or(false);
                let flag = if active { "1" } else { "0" };
                let _: Result<(), _> = redis_conn
                    .set_ex::<_, _, ()>(&active_key, flag, ACTIVE_CACHE_TTL_SECONDS as u64)
                    .await;
                active
            }
        };

        if !is_active {
            return Err(AppError::Unauthorized);
        }

        // 5. Load roles from Redis cache, falling back to DB.
        //
        // Encoding: the cache key is a STRING (not a set) so the write is
        // natively atomic — `SET ... EX` guarantees the TTL is applied in
        // one round trip. The value is a newline-separated list of role
        // names, or [`EMPTY_ROLES_SENTINEL`] when the user has no roles
        // (letting us distinguish "cache miss" from "cache hit, no roles").
        let cache_key = role_cache_key(user_id);
        let cached: Option<String> = redis_conn.get(&cache_key).await.ok();

        let roles = match cached.as_deref() {
            Some(EMPTY_ROLES_SENTINEL) => Vec::new(),
            Some(encoded) if !encoded.is_empty() => encoded
                .split('\n')
                .map(|s| s.to_string())
                .collect(),
            // Either the key is absent (true cache miss) or it's present
            // but empty — in both cases fall back to the DB and repopulate.
            _ => {
                let db_roles: Vec<String> = sqlx::query_scalar(
                    "SELECT r.name FROM roles r JOIN user_roles ur ON r.id = ur.role_id WHERE ur.user_id = $1",
                )
                .bind(user_id)
                .fetch_all(&state.db)
                .await
                .map_err(AppError::Database)?;

                let encoded = if db_roles.is_empty() {
                    EMPTY_ROLES_SENTINEL.to_string()
                } else {
                    db_roles.join("\n")
                };
                // `set_ex` is atomic: the TTL applies the moment the value
                // is written, closing the SADD+EXPIRE race the old code had.
                let _: Result<(), _> = redis_conn
                    .set_ex::<_, _, ()>(&cache_key, encoded, ROLE_CACHE_TTL_SECONDS)
                    .await;

                db_roles
            }
        };

        // 6. Return AuthUser
        Ok(AuthUser {
            user_id,
            email: claims.email,
            roles,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(user_id: Uuid, roles: &[&str]) -> AuthUser {
        AuthUser {
            user_id,
            email: "test@example.com".into(),
            roles: roles.iter().map(|r| (*r).to_string()).collect(),
        }
    }

    #[test]
    fn owner_non_admin_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["member"]);
        assert!(a.owns_or_admin(id, "nope").is_ok());
    }

    #[test]
    fn non_owner_admin_is_ok() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["admin"]);
        assert!(a.owns_or_admin(owner_id, "nope").is_ok());
    }

    #[test]
    fn neither_owner_nor_admin_is_forbidden_with_message() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["member"]);
        let err = a
            .owns_or_admin(owner_id, "you shall not pass")
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(ref m) if m == "you shall not pass"));
    }

    #[test]
    fn owner_and_admin_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["admin"]);
        assert!(a.owns_or_admin(id, "nope").is_ok());
    }

    #[test]
    fn masked_owner_non_admin_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["member"]);
        assert!(a.owns_or_admin_masked(id, "nope").is_ok());
    }

    #[test]
    fn masked_non_owner_admin_is_ok() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["admin"]);
        assert!(a.owns_or_admin_masked(owner_id, "nope").is_ok());
    }

    #[test]
    fn masked_neither_owner_nor_admin_is_not_found_with_message() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["member"]);
        let err = a
            .owns_or_admin_masked(owner_id, "resource not found")
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(ref m) if m == "resource not found"));
    }

    #[test]
    fn owner_only_owner_is_ok() {
        let id = Uuid::now_v7();
        let a = auth(id, &["member"]);
        assert!(a.owner_only(id, "nope").is_ok());
    }

    #[test]
    fn owner_only_admin_non_owner_is_forbidden() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["admin"]);
        let err = a.owner_only(owner_id, "僅本人可取消請假申請").unwrap_err();
        assert!(matches!(err, AppError::Forbidden(ref m) if m == "僅本人可取消請假申請"));
    }

    #[test]
    fn owner_only_neither_owner_nor_admin_is_forbidden_with_message() {
        let owner_id = Uuid::now_v7();
        let a = auth(Uuid::now_v7(), &["member"]);
        let err = a.owner_only(owner_id, "僅本人可預約補課").unwrap_err();
        assert!(matches!(err, AppError::Forbidden(ref m) if m == "僅本人可預約補課"));
    }
}
