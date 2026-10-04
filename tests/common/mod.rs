//! Shared test fixtures for integration tests.
//!
//! Each test spawned by `#[sqlx::test]` runs against a brand-new throwaway
//! database that has had `./migrations/` applied. These helpers insert
//! minimum-viable rows directly via SQL (bypassing the service layer) so a
//! test can focus on the single service call it's exercising.
//!
//! `#[allow(dead_code)]` is applied liberally: each integration test file
//! compiles this module independently, so a helper that is only used by
//! `tests/orders.rs` will look unused from `tests/auth.rs`'s perspective
//! and trigger warnings otherwise.

#![allow(dead_code)]

pub mod fixtures;
pub mod google;
pub mod http;
pub mod mocks;
pub mod twilio;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::config::AuthConfig;
use dream_fly_backend::extractors::auth::AuthUser;
use dream_fly_backend::modules::auth::access;
use dream_fly_backend::modules::auth::provisioning::{self, NewAccount};
use dream_fly_backend::modules::permissions::model::Role;
use dream_fly_backend::modules::permissions::repository as permissions_repository;
use dream_fly_backend::utils::password;
use dream_fly_backend::utils::studio_clock::StudioNow;

use self::mocks::InMemoryAccessCache;

/// Per-plaintext argon2 hash cache, shared across every test in this binary.
/// Argon2 hashing costs ~50-100ms; a lot of tests seed a user with the same
/// fixed password (`"Password!234"`, `seed_member`'s callers, ...), so
/// caching by plaintext avoids re-paying that cost on every call.
static PASSWORD_HASH_CACHE: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Hash `pw` with argon2, consulting/populating [`PASSWORD_HASH_CACHE`] by
/// plaintext. The lock is never held across `.await`: `hash_password` is
/// async, so each lock/unlock brackets only the synchronous `HashMap`
/// lookup/insert. A race where two callers both miss the cache for the same
/// `pw` and each hash it is harmless — both hashes are equally valid, and
/// only the second `insert` wins.
pub async fn hashed(pw: &str) -> String {
    if let Some(hash) = PASSWORD_HASH_CACHE
        .lock()
        .expect("password hash cache lock")
        .get(pw)
    {
        return hash.clone();
    }

    let hash = password::hash_password(pw.to_string())
        .await
        .expect("hash password");

    PASSWORD_HASH_CACHE
        .lock()
        .expect("password hash cache lock")
        .insert(pw.to_string(), hash.clone());

    hash
}

/// Build a `StudioNow` pinned to UTC for the given instant — the test-side
/// mechanical replacement for `&common::test_server_config(), <now>`.
pub fn studio_now_utc(now: chrono::DateTime<Utc>) -> StudioNow {
    StudioNow {
        tz: chrono_tz::UTC,
        now,
    }
}

/// Today's date with the studio timezone pinned to UTC (the default test
/// config) — for service-layer tests that have no `TestApp`.
pub fn today_utc() -> chrono::NaiveDate {
    studio_now_utc(Utc::now()).today()
}

/// Build a Redis connection for tests (db 15, clear of the dev db 0).
/// Expects a locally running Redis (docker-compose up) — override via
/// `TEST_REDIS_URL` if needed. Only the Redis adapter tests use it
/// (`service_access`, `service_ephemeral`, `http_health`).
pub async fn test_redis() -> redis::aio::ConnectionManager {
    let url =
        std::env::var("TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379/15".to_string());
    let client = redis::Client::open(url).expect("build redis client");
    redis::aio::ConnectionManager::new(client)
        .await
        .expect("connect to test redis")
}

/// Config pinned for tests. A deterministic, long-enough JWT secret so
/// `jwt::encode_*` and `jwt::decode_*` round-trip cleanly.
///
/// `google_token_url`/`google_jwks_url` default to non-routable placeholders;
/// HTTP-level tests that exercise `/auth/google` override them to a wiremock
/// base URL via [`crate::common::http::spawn_test_app_with`] before
/// constructing the router.
pub fn test_auth_config() -> AuthConfig {
    AuthConfig {
        jwt_secret: "test-secret-at-least-32-chars-long-1234".into(),
        jwt_access_expiration_minutes: 15,
        jwt_refresh_expiration_days: 30,
        google_client_id: "test-client".into(),
        google_client_secret: "test-secret".into(),
        google_redirect_url: "http://localhost/oauth/callback".into(),
        google_token_url: "http://127.0.0.1:1/oauth/token".into(),
        google_jwks_url: "http://127.0.0.1:1/certs".into(),
    }
}

/// Build an `AuthUser` for a pre-seeded user id, carrying the given roles.
/// Email is synthesized as `{user_id}@example.com` — safe because no
/// service under test reads `AuthUser::email`.
pub fn auth_with_roles(user_id: Uuid, roles: &[&str]) -> AuthUser {
    AuthUser {
        user_id,
        email: format!("{user_id}@example.com"),
        roles: roles.iter().map(|r| (*r).to_string()).collect(),
    }
}

/// Convenience wrapper: a single `member`-role `AuthUser`.
pub fn member_auth(user_id: Uuid) -> AuthUser {
    auth_with_roles(user_id, &["member"])
}

/// Convenience wrapper: a single `coach`-role `AuthUser`.
pub fn coach_auth(user_id: Uuid) -> AuthUser {
    auth_with_roles(user_id, &["coach"])
}

/// Convenience wrapper: a single `admin`-role `AuthUser`.
pub fn admin_auth(user_id: Uuid) -> AuthUser {
    auth_with_roles(user_id, &["admin"])
}

/// Build the `AuthUser` the extractor would produce for a pre-seeded user:
/// roles come from `access::resolve` (the production path, over a fresh
/// `InMemoryAccessCache` so it always reads the DB) and the email from the
/// `users` row. Panics if the user is missing or inactive.
pub async fn auth_for(db: &PgPool, user_id: Uuid) -> AuthUser {
    let roles = access::resolve(db, &InMemoryAccessCache::new(), user_id)
        .await
        .expect("resolve access")
        .expect("user exists and is active");
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .expect("load user email");
    AuthUser {
        user_id,
        email,
        roles,
    }
}

/// Seed a user directly in the DB and return its id. Every user is born a
/// `member` (via `seed_member`'s owner, `create_account`, as in production);
/// `extra` roles are granted on top.
pub async fn seed_user_with_roles(db: &PgPool, email: &str, extra: &[Role]) -> Uuid {
    let user_id = seed_member(db, email, "Password!234").await;

    for role in extra {
        let mut conn = db.acquire().await.expect("acquire conn");
        // The user was created moments ago and has made no request, so
        // no access-cache entry can exist for it yet.
        permissions_repository::assign_role(&mut conn, user_id, *role)
            .await
            .expect("assign role")
            .assume_uncached();
    }

    user_id
}

/// Insert a member user with a pre-hashed password. Returns the new user's id.
///
/// Owner: delegates to `auth::provisioning::create_account` — the same
/// birth path as `register` (lowercased email, `member` role, outbox event).
pub async fn seed_member(db: &PgPool, email: &str, plaintext_password: &str) -> Uuid {
    let hash = hashed(plaintext_password).await;

    let mut tx = db.begin().await.expect("begin tx");

    let user = provisioning::create_account(
        &mut tx,
        NewAccount {
            email,
            name: "Test Member",
            phone: None,
            birth_date: None,
            password_hash: &hash,
        },
        None,
    )
    .await
    .expect("create account");

    tx.commit().await.expect("commit seed_member");

    user.id
}

/// Insert a product. `stock = None` means unlimited (tickets/memberships);
/// `Some(n)` means finite inventory.
pub async fn seed_product(
    db: &PgPool,
    slug: &str,
    price_cents: i64,
    stock: Option<i32>,
) -> Uuid {
    let id = Uuid::now_v7();

    sqlx::query(
        r#"
        INSERT INTO products (
            id, name, slug, product_type, price_cents, features,
            is_highlighted, stock, is_active, created_at, updated_at
        )
        VALUES ($1, $2, $3, 'merchandise'::product_type, $4, '{}'::text[], false, $5, true, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(format!("Test Product {}", slug))
    .bind(slug)
    .bind(price_cents)
    .bind(stock)
    .execute(db)
    .await
    .expect("insert product");

    id
}

/// Add a single item to a user's cart.
pub async fn add_to_cart(db: &PgPool, user_id: Uuid, product_id: Uuid, quantity: i32) {
    sqlx::query(
        r#"
        INSERT INTO cart_items (id, user_id, product_id, quantity, created_at, updated_at)
        VALUES ($1, $2, $3, $4, NOW(), NOW())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(product_id)
    .bind(quantity)
    .execute(db)
    .await
    .expect("insert cart_item");
}

/// Add a course to a user's cart (course lines are always quantity 1).
/// Mirrors `add_to_cart` above but targets `course_id` with `item_type =
/// 'course'` instead of the default product line.
pub async fn add_course_to_cart(db: &PgPool, user_id: Uuid, course_id: Uuid) {
    sqlx::query(
        r#"
        INSERT INTO cart_items (id, user_id, item_type, course_id, quantity, created_at, updated_at)
        VALUES ($1, $2, 'course'::cart_item_type, $3, 1, NOW(), NOW())
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(course_id)
    .execute(db)
    .await
    .expect("insert course cart_item");
}

/// Fetch the current `stock` of a product.
pub async fn product_stock(db: &PgPool, product_id: Uuid) -> Option<i32> {
    sqlx::query_scalar::<_, Option<i32>>("SELECT stock FROM products WHERE id = $1")
        .bind(product_id)
        .fetch_one(db)
        .await
        .expect("fetch stock")
}

/// Fetch a time slot's `booked` count through the production reader
/// (`schedule::repository::find_by_id`, counted from `occupying_bookings`).
pub async fn slot_booked(db: &PgPool, slot_id: Uuid) -> i32 {
    dream_fly_backend::modules::schedule::repository::find_by_id(db, slot_id)
        .await
        .expect("fetch time slot")
        .expect("time slot exists")
        .booked
}

/// Fetch the newest `(title, message)` of a user's notifications matching
/// `notification_type` (e.g. `"booking_confirmed"`, `"order_status"`).
/// `None` if no matching row exists.
pub async fn latest_notification(
    db: &PgPool,
    user_id: Uuid,
    notification_type: &str,
) -> Option<(String, String)> {
    sqlx::query_as(
        "SELECT title, message FROM notifications \
         WHERE user_id = $1 AND type = $2::notification_type \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(notification_type)
    .fetch_optional(db)
    .await
    .expect("query latest_notification")
}

/// Count a user's `orders` rows (any status). For a database-wide total
/// (e.g. cross-user race assertions) query `orders` directly instead.
pub async fn order_count(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM orders WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .expect("count orders")
}

/// Count a user's `cart_items` rows (product and course lines combined).
pub async fn cart_count(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM cart_items WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .expect("count cart_items")
}

/// Fetch a user's current `points_balance`.
pub async fn points_balance_of(db: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT points_balance FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
        .expect("fetch points_balance")
}

/// `pg_backend_pid()` of the connection `tx` runs on — the lock holder's pid
/// for [`wait_for_lock_waiter`].
pub async fn backend_pid(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut **tx)
        .await
        .expect("select pg_backend_pid")
}

/// Poll until some backend is waiting on a heavyweight lock held by the
/// backend `holder_pid` (5s cap) — proves the task under test reached its
/// blocked point instead of trusting a bare sleep. Matching on
/// `pg_blocking_pids` (not just "any backend in a Lock wait") keeps an
/// unrelated waiter from satisfying the poll.
pub async fn wait_for_lock_waiter(db: &PgPool, holder_pid: i32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
             WHERE wait_event_type = 'Lock' AND $1 = ANY(pg_blocking_pids(pid)))",
        )
        .bind(holder_pid)
        .fetch_one(db)
        .await
        .expect("poll pg_stat_activity");
        if waiting {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no backend ever blocked on a lock held by pid {holder_pid}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
