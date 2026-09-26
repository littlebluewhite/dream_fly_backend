//! 帳號存取(Account Access)——「這個 user id 現在能不能動作、帶哪些角色」
//! 的單一 owner。`AuthUser` extractor 讀它([`resolve`]),每個改變答案的寫入
//! 經它回報([`AccessDirty`])。快取的 key 格式、TTL、空角色 sentinel、
//! 快取錯誤一律當 miss(fail-open)全部私有於此,呼叫端看不到 Redis。
//!
//! 快取本身是 [`AccessCache`] seam:只存字串、回 `Result`,策略全在本模組。
//! 正式環境是 [`RedisAccessCache`];整合測試換 in-memory adapter
//! (`tests/common/mocks.rs`)。

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::modules::permissions::repository as permissions_repository;

use super::session;

/// Sentinel value stored in the role cache when a user has *no* roles.
/// Distinguishes "cache miss" (key absent) from "cache hit, no roles"
/// (key present with this marker), so guest users don't repeatedly miss
/// the cache and re-query the DB.
const EMPTY_ROLES_SENTINEL: &str = "\0";

/// TTL for a populated role cache entry (15 minutes).
const ROLE_CACHE_TTL_SECONDS: u64 = 900;

/// TTL for the is_active flag. Short so a deactivation that somehow skipped
/// [`AccessDirty::flush`] still takes effect within one minute.
const ACTIVE_CACHE_TTL_SECONDS: u64 = 60;

/// Storage port for the account access cache — plain string get / set-with-
/// TTL / multi-key delete. Adapters report failures as `Err`; what a failure
/// *means* (fail-open miss, swallowed eviction) is decided in this module,
/// not by the adapter.
#[async_trait]
pub trait AccessCache: Send + Sync {
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>>;
    async fn set_ex(&self, key: &str, val: &str, ttl_secs: u64) -> anyhow::Result<()>;
    async fn del(&self, keys: &[String]) -> anyhow::Result<()>;
}

/// Production adapter over the shared Redis connection manager.
pub struct RedisAccessCache(ConnectionManager);

impl RedisAccessCache {
    pub fn new(conn: ConnectionManager) -> Self {
        Self(conn)
    }
}

#[async_trait]
impl AccessCache for RedisAccessCache {
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        // `ConnectionManager` is a cheap multiplexed handle; commands need
        // `&mut`, so each call works on its own clone.
        let mut conn = self.0.clone();
        Ok(conn.get(key).await?)
    }

    async fn set_ex(&self, key: &str, val: &str, ttl_secs: u64) -> anyhow::Result<()> {
        let mut conn = self.0.clone();
        conn.set_ex::<_, _, ()>(key, val, ttl_secs).await?;
        Ok(())
    }

    async fn del(&self, keys: &[String]) -> anyhow::Result<()> {
        let mut conn = self.0.clone();
        conn.del::<_, ()>(keys).await?;
        Ok(())
    }
}

fn role_cache_key(user_id: Uuid) -> String {
    format!("user_roles:{user_id}")
}

fn active_cache_key(user_id: Uuid) -> String {
    format!("user_active:{user_id}")
}

/// Resolve a user's current access: `Ok(None)` = may not act (deactivated or
/// unknown id), `Ok(Some(roles))` = active with these roles.
///
/// The steady-state hot path is two cache GETs. Either value missing falls
/// back to the DB and refills the cache with `SET EX` (atomic value + TTL).
/// Cache errors are treated as a miss (fail-open onto the DB), never as an
/// error — a Redis outage must not lock every user out.
pub async fn resolve(
    db: &PgPool,
    cache: &dyn AccessCache,
    user_id: Uuid,
) -> Result<Option<Vec<String>>, sqlx::Error> {
    let active_key = active_cache_key(user_id);
    let active_cached = cache.get(&active_key).await.ok().flatten();

    let is_active = match active_cached.as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => {
            let row: Option<(bool,)> = sqlx::query_as("SELECT is_active FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(db)
                .await?;
            let active = row.map(|r| r.0).unwrap_or(false);
            let flag = if active { "1" } else { "0" };
            let _ = cache
                .set_ex(&active_key, flag, ACTIVE_CACHE_TTL_SECONDS)
                .await;
            active
        }
    };

    if !is_active {
        return Ok(None);
    }

    // Encoding: a STRING (not a set) so the write is natively atomic. The
    // value is a newline-separated list of role names, or
    // [`EMPTY_ROLES_SENTINEL`] when the user has no roles.
    let role_key = role_cache_key(user_id);
    let cached = cache.get(&role_key).await.ok().flatten();

    let roles = match cached.as_deref() {
        Some(EMPTY_ROLES_SENTINEL) => Vec::new(),
        Some(encoded) if !encoded.is_empty() => {
            encoded.split('\n').map(|s| s.to_string()).collect()
        }
        // Either the key is absent (true cache miss) or it's present but
        // empty — in both cases fall back to the DB and repopulate.
        _ => {
            let db_roles = permissions_repository::find_role_names_by_user(db, user_id).await?;
            let encoded = if db_roles.is_empty() {
                EMPTY_ROLES_SENTINEL.to_string()
            } else {
                db_roles.join("\n")
            };
            let _ = cache
                .set_ex(&role_key, &encoded, ROLE_CACHE_TTL_SECONDS)
                .await;
            db_roles
        }
    };

    Ok(Some(roles))
}

/// Deactivate an account: `is_active = false` plus the end of every session
/// (`session::end_all`), in the caller's tx — user row first, the same lock
/// order as `session::rotate` and `reset_password`. Ending the sessions here
/// means a later [`reactivate_tx`] cannot bring old refresh tokens back.
/// Flush the returned witness after commit so live access tokens stop
/// working on their very next request.
pub async fn deactivate_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<AccessDirty, sqlx::Error> {
    set_active_tx(tx, user_id, false).await?;
    session::end_all(tx, user_id).await?;
    Ok(AccessDirty::new(user_id))
}

/// Reactivate an account (`is_active = true`). Sessions ended by
/// [`deactivate_tx`] stay ended — the user has to log in again. Flush the
/// returned witness after commit.
pub async fn reactivate_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<AccessDirty, sqlx::Error> {
    set_active_tx(tx, user_id, true).await?;
    Ok(AccessDirty::new(user_id))
}

async fn set_active_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    is_active: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET is_active = $2, updated_at = NOW() WHERE id = $1")
        .bind(user_id)
        .bind(is_active)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Witness that a write changing a user's access (a `user_roles` row, the
/// `is_active` flag) has not yet been reflected in the access cache. Every
/// such write returns this instead of `()`, so "forgot to invalidate the
/// cache" becomes a `#[must_use]` compiler warning instead of a silent
/// stale-cache bug.
///
/// Call [`flush`](Self::flush) AFTER the writing transaction commits, never
/// before: invalidating pre-commit would let a request racing between the
/// DEL and the commit re-populate the cache with the pre-write value.
///
/// Known residual race (out of scope for this type): a concurrent
/// [`resolve`] can read the DB for the old answer and `SET EX` it back
/// *after* `flush()` has already run its DEL. The stale entry then lives
/// out its TTL (60s active flag / 900s roles) before self-correcting.
#[must_use]
pub struct AccessDirty(Uuid);

impl AccessDirty {
    /// `pub(crate)`: repository functions in sibling modules
    /// (`permissions::repository`) build one after a `user_roles` write.
    pub(crate) fn new(user_id: Uuid) -> Self {
        Self(user_id)
    }

    /// Consume the witness and drop both of the user's cache entries in one
    /// DEL. Best-effort: a cache error is logged and swallowed — eviction
    /// failure must never fail the access change itself.
    pub async fn flush(self, cache: &dyn AccessCache) {
        let user_id = self.0;
        let keys = [role_cache_key(user_id), active_cache_key(user_id)];
        if let Err(e) = cache.del(&keys).await {
            tracing::warn!(%user_id, error = %e, "failed to invalidate access cache");
        }
    }

    /// Consume the witness without touching the cache — only for a user id
    /// that provably has no cache entry (e.g. a row created in this very
    /// transaction). A named no-op so every such site is greppable.
    pub fn assume_uncached(self) {}
}
