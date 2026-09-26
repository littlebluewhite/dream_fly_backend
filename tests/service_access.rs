//! Integration tests for `auth::access` (Account Access).
//!
//! Every scenario is written once against `&dyn AccessCache` and run against
//! both adapters — the production `RedisAccessCache` and the in-memory one
//! the HTTP harness uses (`common::mocks::InMemoryAccessCache`) — so the two
//! stay observably interchangeable. `FailingAccessCache` pins the fail-open
//! policy separately.

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::modules::auth::access::{self, AccessCache, RedisAccessCache};
use dream_fly_backend::modules::permissions::repository as permissions_repository;

use common::mocks::{FailingAccessCache, InMemoryAccessCache};

async fn member(db: &PgPool) -> Uuid {
    let email = format!("access-{}@example.com", Uuid::now_v7());
    common::seed_member(db, &email, "Password!234").await
}

/// A `user_roles` write that deliberately skips the witness flush, to make
/// the cache observably stale.
async fn grant_behind_cache(db: &PgPool, user_id: Uuid, role: &str) {
    let mut conn = db.acquire().await.expect("acquire");
    permissions_repository::assign_role_by_name(&mut conn, user_id, role)
        .await
        .expect("grant")
        .assume_uncached();
}

async fn resolve(db: &PgPool, cache: &dyn AccessCache, user_id: Uuid) -> Option<Vec<String>> {
    access::resolve(db, cache, user_id).await.expect("resolve")
}

fn roles(names: &[&str]) -> Option<Vec<String>> {
    Some(names.iter().map(|n| n.to_string()).collect())
}

// --- scenarios ---------------------------------------------------------

/// An active user resolves to their roles, and a repeat is served from the
/// cache (a grant nothing flushed is not visible yet).
async fn active_user_roles_are_cached(db: &PgPool, cache: &dyn AccessCache) {
    let user_id = member(db).await;
    assert_eq!(resolve(db, cache, user_id).await, roles(&["member"]));

    grant_behind_cache(db, user_id, "coach").await;
    assert_eq!(resolve(db, cache, user_id).await, roles(&["member"]));
}

/// An id with no `users` row may not act.
async fn unknown_user_resolves_to_none(db: &PgPool, cache: &dyn AccessCache) {
    assert_eq!(resolve(db, cache, Uuid::now_v7()).await, None);
}

/// "No roles" is cached as such (the sentinel), not treated as a miss.
async fn empty_role_set_is_cached(db: &PgPool, cache: &dyn AccessCache) {
    let user_id = member(db).await;
    sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
        .bind(user_id)
        .execute(db)
        .await
        .expect("strip roles");

    assert_eq!(resolve(db, cache, user_id).await, roles(&[]));

    grant_behind_cache(db, user_id, "member").await;
    assert_eq!(resolve(db, cache, user_id).await, roles(&[]));
}

/// A flushed role grant is visible on the next resolve.
async fn flushed_role_grant_is_visible(db: &PgPool, cache: &dyn AccessCache) {
    let user_id = member(db).await;
    assert_eq!(resolve(db, cache, user_id).await, roles(&["member"]));

    let mut conn = db.acquire().await.expect("acquire");
    let dirty = permissions_repository::assign_role_by_name(&mut conn, user_id, "coach")
        .await
        .expect("grant");
    dirty.flush(cache).await;

    assert_eq!(
        resolve(db, cache, user_id).await,
        roles(&["coach", "member"])
    );
}

/// Deactivate + flush denies a warm-cached user at once; reactivate + flush
/// lets them back in.
async fn deactivate_and_reactivate_take_effect_on_flush(db: &PgPool, cache: &dyn AccessCache) {
    let user_id = member(db).await;
    assert_eq!(resolve(db, cache, user_id).await, roles(&["member"]));

    let mut tx = db.begin().await.expect("begin");
    let dirty = access::deactivate_tx(&mut tx, user_id)
        .await
        .expect("deactivate");
    tx.commit().await.expect("commit");
    dirty.flush(cache).await;
    assert_eq!(resolve(db, cache, user_id).await, None);

    let mut tx = db.begin().await.expect("begin");
    let dirty = access::reactivate_tx(&mut tx, user_id)
        .await
        .expect("reactivate");
    tx.commit().await.expect("commit");
    dirty.flush(cache).await;
    assert_eq!(resolve(db, cache, user_id).await, roles(&["member"]));
}

/// Why the witness exists: a committed deactivation whose witness is not
/// flushed leaves the warm cache answering "active".
async fn unflushed_deactivation_stays_stale(db: &PgPool, cache: &dyn AccessCache) {
    let user_id = member(db).await;
    assert_eq!(resolve(db, cache, user_id).await, roles(&["member"]));

    let mut tx = db.begin().await.expect("begin");
    access::deactivate_tx(&mut tx, user_id)
        .await
        .expect("deactivate")
        .assume_uncached();
    tx.commit().await.expect("commit");

    assert_eq!(resolve(db, cache, user_id).await, roles(&["member"]));
}

async fn redis_cache() -> RedisAccessCache {
    RedisAccessCache::new(common::test_redis().await)
}

macro_rules! both_adapters {
    ($($scenario:ident),* $(,)?) => {$(
        mod $scenario {
            use super::*;

            #[sqlx::test]
            async fn in_memory(db: PgPool) {
                super::$scenario(&db, &InMemoryAccessCache::new()).await;
            }

            #[sqlx::test]
            async fn redis(db: PgPool) {
                super::$scenario(&db, &redis_cache().await).await;
            }
        }
    )*};
}

both_adapters!(
    active_user_roles_are_cached,
    unknown_user_resolves_to_none,
    empty_role_set_is_cached,
    flushed_role_grant_is_visible,
    deactivate_and_reactivate_take_effect_on_flush,
    unflushed_deactivation_stays_stale,
);

// --- fail-open ---------------------------------------------------------

/// Every cache call failing degrades to reading the DB on each resolve —
/// never an error, never a lock-out — and a failing flush is swallowed.
#[sqlx::test]
async fn failing_cache_fails_open_onto_the_db(db: PgPool) {
    let cache = FailingAccessCache;
    let user_id = member(&db).await;

    assert_eq!(resolve(&db, &cache, user_id).await, roles(&["member"]));

    // No caching: a grant nothing flushed is visible straight away.
    grant_behind_cache(&db, user_id, "coach").await;
    assert_eq!(
        resolve(&db, &cache, user_id).await,
        roles(&["coach", "member"])
    );

    let mut tx = db.begin().await.expect("begin");
    let dirty = access::deactivate_tx(&mut tx, user_id)
        .await
        .expect("deactivate");
    tx.commit().await.expect("commit");
    dirty.flush(&cache).await;
    assert_eq!(resolve(&db, &cache, user_id).await, None);
}
