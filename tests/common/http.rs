//! In-process Axum HTTP test harness.
//!
//! Every `tests/http_*.rs` file builds a `TestApp` with [`spawn_test_app`]
//! (or [`spawn_test_app_with`] when a test needs to tweak config such as
//! the Google token URL). The harness:
//!
//! - builds a minimal [`AppConfig`] pinned to UTC, a fixed JWT secret, and
//!   `trust_proxy = true` so the rate-limit middleware honors our synthetic
//!   `X-Forwarded-For` (each test gets a unique IP, so rate limits are
//!   effectively isolated per test)
//! - connects to a shared Redis (db 15 by default) for JWT role cache +
//!   rate-limit counters, with a per-test prefix flush
//! - wraps the real production router (`startup::build_router`) so every
//!   test exercises the entire middleware stack + extractors + handlers
//! - exposes [`MockEmailClient`] / [`MockSmsClient`] via `app.email` /
//!   `app.sms` so tests can assert on outbound messages without hitting
//!   real SMTP / Twilio
//!
//! The harness does NOT mock the database — tests rely on the standard
//! `#[sqlx::test]` per-test fresh-database isolation, passing the supplied
//! `PgPool` directly into [`spawn_test_app`].

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum_test::TestServer;
use redis::AsyncCommands;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::config::{
    AppConfig, AuthConfig, DatabaseConfig, EmailConfig, KafkaConfig, RedisConfig, ServerConfig,
    SmsConfig,
};
use dream_fly_backend::startup;
use dream_fly_backend::state::AppState;
use dream_fly_backend::utils::clock::Clock;
use dream_fly_backend::utils::email::EmailSender;
use dream_fly_backend::utils::google_oauth::JwksCache;
use dream_fly_backend::utils::password;
use dream_fly_backend::utils::sms::SmsSender;

use super::mocks::{MockClock, MockEmailClient, MockSmsClient};

/// Client IP counter so every `TestApp` gets a unique synthetic source IP.
/// Combined with `trust_proxy=true`, this gives each test its own rate-limit
/// bucket in Redis even though they all share the same physical Redis DB.
static IP_COUNTER: AtomicU32 = AtomicU32::new(0);

fn next_client_ip() -> IpAddr {
    // 10.<n1>.<n2>.<n3> — gives us 2^24 buckets before wrapping, which is
    // orders of magnitude more than the whole test suite will ever allocate.
    let n = IP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let b1 = ((n >> 16) & 0xff) as u8;
    let b2 = ((n >> 8) & 0xff) as u8;
    let b3 = (n & 0xff) as u8;
    IpAddr::V4(Ipv4Addr::new(10, b1, b2, b3))
}

/// Build a test-tuned `AppConfig`. Callers can pass an `adjust` closure to
/// override arbitrary fields (e.g. set `auth.google_token_url` to a
/// wiremock server).
pub fn test_app_config<F: FnOnce(&mut AppConfig)>(adjust: F) -> AppConfig {
    let mut cfg = AppConfig {
        server: ServerConfig {
            host: "0.0.0.0".into(),
            port: 0,
            allowed_origins: vec![],
            // Enable so the rate limit middleware honors our synthetic XFF
            // header and gives each test its own bucket.
            trust_proxy: true,
            studio_timezone: "UTC".into(),
        },
        database: DatabaseConfig {
            // `db` in AppState is passed separately by `spawn_test_app`, so
            // this URL is never actually opened — it just needs to parse.
            url: "postgres://unused".into(),
            max_connections: 1,
            min_connections: 1,
        },
        redis: RedisConfig {
            url: std::env::var("TEST_REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379/15".into()),
        },
        kafka: KafkaConfig {
            brokers: "localhost:9092".into(),
            group_id: "dreamfly_test".into(),
            enabled: false,
        },
        auth: AuthConfig {
            jwt_secret: "test-secret-at-least-32-chars-long-1234".into(),
            jwt_access_expiration_minutes: 15,
            jwt_refresh_expiration_days: 30,
            google_client_id: "test-client".into(),
            google_client_secret: "test-secret".into(),
            google_redirect_url: "http://localhost/oauth/callback".into(),
            google_token_url: "http://127.0.0.1:1/oauth/token".into(),
            google_jwks_url: "http://127.0.0.1:1/certs".into(),
        },
        email: EmailConfig {
            smtp_host: "localhost".into(),
            smtp_port: 25,
            smtp_username: String::new(),
            smtp_password: String::new(),
            from_email: "test@example.com".into(),
            from_name: "Test".into(),
        },
        sms: SmsConfig {
            twilio_account_sid: "test-sid".into(),
            twilio_auth_token: "test-token".into(),
            twilio_from_number: "+10000000000".into(),
        },
    };
    adjust(&mut cfg);
    cfg
}

/// A fully-wired in-process Axum server backed by the given `PgPool`.
/// Constructed by [`spawn_test_app`] / [`spawn_test_app_with`]. Holds the
/// mock email & sms clients so tests can assert on outbound messages.
pub struct TestApp {
    pub server: TestServer,
    pub db: PgPool,
    pub config: Arc<AppConfig>,
    pub email: Arc<MockEmailClient>,
    pub sms: Arc<MockSmsClient>,
    pub clock: Arc<MockClock>,
    /// Synthetic source IP used as `X-Forwarded-For` on every request.
    pub client_ip: IpAddr,
}

impl TestApp {
    /// Return a `POST` builder pre-decorated with auth + synthetic XFF.
    pub fn post(&self, path: &str) -> axum_test::TestRequest {
        self.server.post(path)
    }
    pub fn get(&self, path: &str) -> axum_test::TestRequest {
        self.server.get(path)
    }
    pub fn patch(&self, path: &str) -> axum_test::TestRequest {
        self.server.patch(path)
    }
    pub fn put(&self, path: &str) -> axum_test::TestRequest {
        self.server.put(path)
    }
    pub fn delete(&self, path: &str) -> axum_test::TestRequest {
        self.server.delete(path)
    }

    /// Register a brand-new member (no admin role) via `/auth/register` and
    /// return `(user_id, access_token, refresh_token)`. Callers can pass
    /// the access token via `.authorization_bearer(..)` on subsequent
    /// requests.
    pub async fn register_member(&self, email: &str, password: &str) -> RegisteredUser {
        let resp = self
            .post("/api/v1/auth/register")
            .json(&json!({
                "email": email,
                "name": "Test Member",
                "password": password,
            }))
            .await;
        resp.assert_status_ok();
        let body: serde_json::Value = resp.json();
        let user_id = Uuid::parse_str(body["user"]["id"].as_str().expect("user.id"))
            .expect("parse user id");

        // Close any cached role/active entry seeded by the auth extractor
        // during register itself. Test-to-test UUID collision under v7 is
        // astronomically improbable, but it costs us two Redis DELs and
        // guarantees the next test can't see this user's cache.
        self.clear_user_cache(user_id).await;

        RegisteredUser {
            user_id,
            email: email.to_string(),
            access_token: body["access_token"].as_str().expect("access_token").to_string(),
            refresh_token: body["refresh_token"].as_str().expect("refresh_token").to_string(),
        }
    }

    /// Delete the role and active-flag cache entries for a single user.
    /// Called from both `register_member` and `seed_user_with_roles` so
    /// every test exit path leaves the cache empty for the users it touched.
    async fn clear_user_cache(&self, user_id: Uuid) {
        let mut r = self.redis_conn().await;
        let _: Result<(), _> = r.del::<_, ()>(format!("user_roles:{user_id}")).await;
        let _: Result<(), _> = r.del::<_, ()>(format!("user_active:{user_id}")).await;
    }

    /// Seed a user directly in the DB with the named roles attached, and
    /// return `(user_id, access_token)`. Use this when a test needs an
    /// admin or coach without going through `/auth/register`.
    pub async fn seed_user_with_roles(
        &self,
        email: &str,
        roles: &[&str],
    ) -> (Uuid, String) {
        let id = Uuid::now_v7();
        let hashed = password::hash_password("Password!234".to_string())
            .await
            .expect("hash");
        sqlx::query(
            r#"
            INSERT INTO users (id, email, name, password_hash, phone_verified, is_active, created_at, updated_at)
            VALUES ($1, $2, $3, $4, false, true, NOW(), NOW())
            "#,
        )
        .bind(id)
        .bind(email)
        .bind("Seeded User")
        .bind(&hashed)
        .execute(&self.db)
        .await
        .expect("insert user");

        for role in roles {
            sqlx::query(
                r#"
                INSERT INTO user_roles (user_id, role_id)
                SELECT $1, id FROM roles WHERE name = $2
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(id)
            .bind(*role)
            .execute(&self.db)
            .await
            .expect("assign role");
        }

        // Invalidate any cached role set under this id (should be empty since
        // the id is freshly generated, but belt + braces).
        let mut r = self.redis_conn().await;
        let _: Result<(), _> = r.del::<_, ()>(format!("user_roles:{id}")).await;
        let _: Result<(), _> = r.del::<_, ()>(format!("user_active:{id}")).await;

        let token = dream_fly_backend::utils::jwt::encode_access_token(
            &self.config.auth,
            id,
            email,
        )
        .expect("encode access token");

        (id, token)
    }

    /// Convenience for tests that want a ready-to-use admin account.
    pub async fn seed_admin(&self) -> (Uuid, String) {
        let email = format!("admin-{}@test.local", Uuid::now_v7());
        self.seed_user_with_roles(&email, &["admin"]).await
    }

    /// Fresh Redis connection for test setup/teardown assertions.
    pub async fn redis_conn(&self) -> redis::aio::ConnectionManager {
        let client = redis::Client::open(self.config.redis.url.as_str()).expect("redis");
        redis::aio::ConnectionManager::new(client)
            .await
            .expect("connect redis")
    }
}

/// Identifying metadata for a user created via `register_member`.
pub struct RegisteredUser {
    pub user_id: Uuid,
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
}

/// Spawn a `TestApp` backed by the given sqlx test pool. Uses the default
/// test config; for tests that need to override config (e.g. point the
/// Google token URL at wiremock) use [`spawn_test_app_with`].
pub async fn spawn_test_app(db: PgPool) -> TestApp {
    spawn_test_app_with(db, |_| {}).await
}

/// Spawn a `TestApp`, allowing the caller to mutate the `AppConfig` before
/// the router is built. Typical use: redirecting Google OAuth / Twilio.
pub async fn spawn_test_app_with<F: FnOnce(&mut AppConfig)>(db: PgPool, adjust: F) -> TestApp {
    let config = test_app_config(adjust);
    let redis_url = config.redis.url.clone();

    // Connect to Redis. If this fails the test literally cannot run (rate
    // limit + auth extractor both require Redis) — panic with a clear msg.
    let redis_client = redis::Client::open(redis_url.as_str())
        .expect("failed to build test redis client (is redis running on TEST_REDIS_URL?)");
    let mut redis = redis::aio::ConnectionManager::new(redis_client)
        .await
        .expect("failed to connect to test redis");

    // Each test gets a unique synthetic IP (see next_client_ip below), but
    // different test binaries restart the counter at 0 — a stale
    // `rate_limit:auth:10.0.0.0` key from a previous run would instantly
    // trip the 429 on the very first request. Clear both buckets for the
    // IP we're about to hand out before constructing the router.
    let ip_preview = next_client_ip();
    for prefix in ["rate_limit:global:", "rate_limit:auth:"] {
        let key = format!("{prefix}{ip_preview}");
        let _: Result<(), _> = redis::AsyncCommands::del::<_, ()>(&mut redis, key).await;
    }

    let email = Arc::new(MockEmailClient::new());
    let sms = Arc::new(MockSmsClient::new());
    let email_state: Arc<dyn EmailSender> = email.clone();
    let sms_state: Arc<dyn SmsSender> = sms.clone();
    let clock = Arc::new(MockClock::new());
    let clock_state: Arc<dyn Clock> = clock.clone();

    let http_client = reqwest::Client::new();

    let config_arc = Arc::new(config);
    let state = AppState {
        db: db.clone(),
        redis,
        kafka_producer: None,
        config: config_arc.clone(),
        http_client,
        email_client: email_state,
        sms_client: sms_state,
        clock: clock_state,
        jwks_cache: Arc::new(JwksCache::new()),
    };

    let router = startup::build_router(state);
    let mut server = TestServer::new(router);

    // Use the IP we already cleared in Redis above. With `trust_proxy=true`
    // in the test config, the rate-limit middleware reads this header and
    // maintains a separate bucket for this test run.
    let client_ip = ip_preview;
    server.add_header("x-forwarded-for", client_ip.to_string());

    TestApp {
        server,
        db,
        config: config_arc,
        email,
        sms,
        clock,
        client_ip,
    }
}
