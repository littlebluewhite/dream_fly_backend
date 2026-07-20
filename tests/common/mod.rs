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
pub mod http;
pub mod mocks;
pub mod twilio;

use chrono::{Duration, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::config::{AuthConfig, ServerConfig};
use dream_fly_backend::extractors::auth::AuthUser;
use dream_fly_backend::utils::password;

/// Pin the server config tests use to UTC so naïve date+time arithmetic
/// in booking/cancel tests lines up with `Utc::now()` regardless of the
/// host's configured timezone.
pub fn test_server_config() -> ServerConfig {
    ServerConfig {
        host: "0.0.0.0".into(),
        port: 3000,
        allowed_origins: vec![],
        trust_proxy: false,
        studio_timezone: "UTC".into(),
    }
}

/// Build a Redis connection for tests. Expects a locally running Redis
/// (docker-compose up) — override via `TEST_REDIS_URL` if needed.
pub async fn test_redis() -> redis::aio::ConnectionManager {
    let url = std::env::var("TEST_REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
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

/// Insert a member user with a pre-hashed password. Returns the new user's id.
pub async fn seed_member(db: &PgPool, email: &str, plaintext_password: &str) -> Uuid {
    let id = Uuid::now_v7();
    let hash = password::hash_password(plaintext_password.to_string())
        .await
        .expect("hash password");

    sqlx::query(
        r#"
        INSERT INTO users (id, email, name, password_hash, phone_verified, is_active, created_at, updated_at)
        VALUES ($1, $2, $3, $4, false, true, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(email)
    .bind("Test Member")
    .bind(&hash)
    .execute(db)
    .await
    .expect("insert user");

    // Attach the `member` role (seeded by migration 00002).
    sqlx::query(
        r#"
        INSERT INTO user_roles (user_id, role_id)
        SELECT $1, id FROM roles WHERE name = 'member'
        "#,
    )
    .bind(id)
    .execute(db)
    .await
    .expect("assign member role");

    id
}

/// Insert a time slot scheduled for tomorrow at 10:00–11:00.
/// Returns the slot id. Used by bookings tests.
pub async fn seed_time_slot(db: &PgPool, capacity: i32) -> Uuid {
    seed_time_slot_on(db, capacity, (Utc::now() + Duration::days(2)).date_naive()).await
}

/// Insert a time slot on the given date. Lets callers control whether a slot
/// falls inside or outside the 24-hour cancellation window.
pub async fn seed_time_slot_on(db: &PgPool, capacity: i32, date: NaiveDate) -> Uuid {
    let start = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
    seed_time_slot_on_with_start(db, capacity, date, start).await
}

/// Insert a time slot on an exact (date, start_time). Used by tests that
/// need to place the slot at a very specific offset from `now()` — e.g.
/// the 24-hour cancellation test needs the slot strictly in the future
/// but strictly inside the 24h window.
pub async fn seed_time_slot_on_with_start(
    db: &PgPool,
    capacity: i32,
    date: NaiveDate,
    start: NaiveTime,
) -> Uuid {
    let id = Uuid::now_v7();
    // `overflowing_add_signed` wraps at midnight, which would violate the
    // `time_slots_time_order CHECK (end_time > start_time)` whenever `start`
    // lands in the last hour of the day (callers pass wall-clock-derived
    // starts, so any test run between 20:00 and 21:00 UTC used to hit this)
    // — clamp to end-of-day instead of wrapping. Tests only compare against
    // `start_time`, so the exact clamped end value is inconsequential.
    let (end, carry) = start.overflowing_add_signed(chrono::Duration::hours(1));
    let end = if carry != 0 {
        NaiveTime::from_hms_micro_opt(23, 59, 59, 999_999).unwrap()
    } else {
        end
    };

    sqlx::query(
        r#"
        INSERT INTO time_slots (id, date, start_time, end_time, capacity, booked, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, 0, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(date)
    .bind(start)
    .bind(end)
    .bind(capacity)
    .execute(db)
    .await
    .expect("insert time_slot");

    id
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

/// Fetch the current `booked` count of a time slot.
pub async fn slot_booked(db: &PgPool, slot_id: Uuid) -> i32 {
    sqlx::query_scalar::<_, i32>("SELECT booked FROM time_slots WHERE id = $1")
        .bind(slot_id)
        .fetch_one(db)
        .await
        .expect("fetch booked count")
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
