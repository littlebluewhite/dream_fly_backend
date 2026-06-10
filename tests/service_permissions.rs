//! Integration tests for `permissions::service`.
//!
//! Covered paths:
//! - `list_roles` returns the four seeded roles in alphabetical order
//! - `get_role_with_permissions` returns NotFound for a random role id
//! - `create_role` returns Conflict on duplicate name (not an opaque 500)
//! - `assign_role_to_user` writes the user_roles row AND invalidates the
//!   Redis role cache so the next request reloads from DB
//! - `assign_role_to_user` with a nonexistent role returns NotFound
//! - `remove_role_from_user` is idempotent (removing a role the user
//!   never had does NOT error) and also clears the cache
//!
//! The Redis-facing assertions use the shared test Redis (db 15 by
//! default) and the exact cache key format owned by
//! `extractors::auth::role_cache_key` so the test breaks if that format
//! is accidentally changed in one place but not the other.

mod common;

use redis::AsyncCommands;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::auth::role_cache_key;
use dream_fly_backend::modules::permissions::service;

async fn seed_cache_entry(
    redis: &mut redis::aio::ConnectionManager,
    user_id: Uuid,
    payload: &str,
) {
    let key = role_cache_key(user_id);
    let _: () = redis
        .set_ex(&key, payload, 900)
        .await
        .expect("seed role cache");
}

async fn cache_exists(redis: &mut redis::aio::ConnectionManager, user_id: Uuid) -> bool {
    let key = role_cache_key(user_id);
    let exists: bool = redis.exists(&key).await.expect("exists check");
    exists
}

#[sqlx::test]
async fn list_roles_returns_four_seeded_roles(db: PgPool) {
    // Migration 20260410000001_init.sql seeds: admin, coach, member, guest.
    let roles = service::list_roles(&db).await.expect("list_roles");

    assert_eq!(roles.len(), 4, "expected 4 seeded roles, got {}", roles.len());
    let names: Vec<_> = roles.iter().map(|r| r.name.as_str()).collect();
    // Ordered alphabetically by repository query.
    assert_eq!(names, ["admin", "coach", "guest", "member"]);
}

#[sqlx::test]
async fn get_role_with_permissions_nonexistent_returns_not_found(db: PgPool) {
    let err = service::get_role_with_permissions(&db, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn create_role_duplicate_name_returns_conflict(db: PgPool) {
    // `admin` is already seeded. A second insert must surface as Conflict
    // (not the bare sqlx database error that would otherwise leak to the
    // client as a 500).
    let err = service::create_role(&db, "admin", Some("dup"))
        .await
        .unwrap_err();

    match err {
        AppError::Conflict(msg) => assert!(msg.contains("admin"), "msg: {msg}"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn create_role_with_fresh_name_succeeds(db: PgPool) {
    let role = service::create_role(&db, "reviewer", Some("reviews content"))
        .await
        .expect("create_role");

    assert_eq!(role.name, "reviewer");
    assert_eq!(role.description.as_deref(), Some("reviews content"));
}

#[sqlx::test]
async fn assign_role_persists_and_invalidates_redis_cache(db: PgPool) {
    let user_id = common::seed_member(&db, "perm@example.com", "hunter22-secret").await;
    let mut redis = common::test_redis().await;

    // Seed a stale cache entry for this user so we can observe its removal.
    seed_cache_entry(&mut redis, user_id, "stale-roles-json").await;
    assert!(cache_exists(&mut redis, user_id).await);

    // Look up the coach role id.
    let coach_id: Uuid = sqlx::query_scalar("SELECT id FROM roles WHERE name = 'coach'")
        .fetch_one(&db)
        .await
        .expect("fetch coach role id");

    service::assign_role_to_user(&db, &mut redis, user_id, coach_id)
        .await
        .expect("assign_role");

    // user_roles row exists.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_roles WHERE user_id = $1 AND role_id = $2",
    )
    .bind(user_id)
    .bind(coach_id)
    .fetch_one(&db)
    .await
    .expect("count");
    assert_eq!(count, 1);

    // Cache was invalidated.
    assert!(
        !cache_exists(&mut redis, user_id).await,
        "role cache should be cleared on assign"
    );
}

#[sqlx::test]
async fn assign_role_nonexistent_role_returns_not_found(db: PgPool) {
    let user_id = common::seed_member(&db, "nr@example.com", "hunter22-secret").await;
    let mut redis = common::test_redis().await;

    let err = service::assign_role_to_user(&db, &mut redis, user_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn remove_role_is_idempotent_and_clears_cache(db: PgPool) {
    let user_id = common::seed_member(&db, "rm@example.com", "hunter22-secret").await;
    let mut redis = common::test_redis().await;

    let admin_id: Uuid = sqlx::query_scalar("SELECT id FROM roles WHERE name = 'admin'")
        .fetch_one(&db)
        .await
        .unwrap();

    // Seed cache, then remove a role the user never had — must not error
    // and must still clear the cache (defense-in-depth: an admin action
    // always produces a cache flush).
    seed_cache_entry(&mut redis, user_id, "stale").await;

    service::remove_role_from_user(&db, &mut redis, user_id, admin_id)
        .await
        .expect("remove is idempotent");

    assert!(!cache_exists(&mut redis, user_id).await);
}
