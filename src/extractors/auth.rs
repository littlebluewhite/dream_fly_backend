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
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
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
