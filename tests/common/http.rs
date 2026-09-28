//! In-process Axum HTTP test harness.
//!
//! Every `tests/http_*.rs` file builds a `TestApp` with [`spawn_test_app`]
//! (or [`spawn_test_app_with`] when a test needs to tweak config such as
//! the Google token URL). The harness:
//!
//! - builds a minimal [`AppConfig`] pinned to UTC, a fixed JWT secret, and
//!   `trust_proxy = true` so the rate-limit middleware honors our synthetic
//!   `X-Forwarded-For` (each `TestApp` gets a unique IP; its buckets live
//!   in its own store anyway, see below)
//! - gives each `TestApp` its own `InMemoryEphemeralStore` for short-lived
//!   state (rate-limit buckets, login/forgot-password counters, OTP, reset
//!   tokens) and its own `InMemoryAccessCache` (`app.access_cache`) for the
//!   account access cache the `AuthUser` extractor reads, so HTTP tests
//!   never touch either Redis adapter — `tests/service_ephemeral.rs` and
//!   `tests/service_access.rs` cover both adapters of each seam directly.
//!   [`spawn_test_app_with_store`] swaps in another store (e.g.
//!   `FailingEphemeralStore`)
//! - answers `/health` from a `StaticHealthProbe` (`up()` by default), so no
//!   HTTP test needs Redis; [`spawn_test_app_with_health`] swaps in another
//!   probe (e.g. `StaticHealthProbe::redis_down()`)
//! - wraps the real production router (`startup::build_router`) so every
//!   test exercises the entire middleware stack + extractors + handlers
//! - exposes [`MockEmailClient`] via `app.email` so tests can assert on
//!   outbound email without hitting real SMTP. Outbound SMS has no
//!   equivalent recorder: tests point `SmsConfig::twilio_base_url` at a
//!   `wiremock` server directly (see `common::twilio`), the same seam used
//!   for the Google token/JWKS URLs below
//!
//! The harness does NOT mock the database — tests rely on the standard
//! `#[sqlx::test]` per-test fresh-database isolation, passing the supplied
//! `PgPool` directly into [`spawn_test_app`].

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum_test::TestServer;
use serde_json::json;
use sqlx::PgPool;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use dream_fly_backend::config::{
    AppConfig, AuthConfig, DatabaseConfig, EmailConfig, KafkaConfig, RedisConfig, ServerConfig,
    SmsConfig,
};
use dream_fly_backend::health::HealthProbe;
use dream_fly_backend::modules::auth::repository;
use dream_fly_backend::modules::permissions::repository as permissions_repository;
use dream_fly_backend::startup;
use dream_fly_backend::state::{AppState, Infra, Overrides};
use dream_fly_backend::utils::ephemeral::EphemeralStore;

use super::mocks::{
    InMemoryAccessCache, InMemoryEphemeralStore, MockClock, MockEmailClient, StaticHealthProbe,
};

/// Client IP counter so every `TestApp` gets a unique synthetic source IP.
/// Combined with `trust_proxy=true`, the rate-limit middleware keys each
/// `TestApp`'s buckets off its own IP.
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
            studio_timezone: "UTC".parse().unwrap(),
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
            // Reserved loopback port: refuses the connection immediately
            // instead of hanging if a test forgets to mount a `MockServer`
            // (see `common::twilio::mount_twilio`). Same convention as
            // `auth.google_token_url`/`google_jwks_url` above.
            twilio_base_url: "http://127.0.0.1:1".into(),
        },
    };
    adjust(&mut cfg);
    cfg
}

/// A fully-wired in-process Axum server backed by the given `PgPool`.
/// Constructed by [`spawn_test_app`] / [`spawn_test_app_with`]. Holds the
/// mock email client so tests can assert on outbound password-reset messages;
/// SMS assertions go through wiremock (see tests/common/twilio.rs).
pub struct TestApp {
    pub server: TestServer,
    pub db: PgPool,
    pub config: Arc<AppConfig>,
    pub email: Arc<MockEmailClient>,
    pub clock: Arc<MockClock>,
    /// The in-memory account access cache the router's `AuthUser` extractor
    /// reads (`AppState::access_cache`), so tests can drive
    /// `auth::access::resolve` against the same cache.
    pub access_cache: Arc<InMemoryAccessCache>,
    /// Synthetic source IP used as `X-Forwarded-For` on every request.
    pub client_ip: IpAddr,
    /// Clone of the same `TaskTracker` held by `AppState::background_tasks`
    /// inside the router this `TestApp` wraps — see `drain_background`.
    pub background: TaskTracker,
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

        RegisteredUser {
            user_id,
            email: email.to_string(),
            access_token: body["access_token"].as_str().expect("access_token").to_string(),
            refresh_token: body["refresh_token"].as_str().expect("refresh_token").to_string(),
        }
    }

    /// Seed a user directly in the DB with the named roles attached, and
    /// return `(user_id, access_token)`. Use this when a test needs an
    /// admin or coach without going through `/auth/register`.
    ///
    /// Owner: delegates to `auth::repository::create_user_tx` /
    /// `permissions::repository::assign_role_by_name` rather than
    /// hand-rolling the `INSERT` — see those for the real row shape.
    pub async fn seed_user_with_roles(
        &self,
        email: &str,
        roles: &[&str],
    ) -> (Uuid, String) {
        let hash = super::hashed("Password!234").await;

        let mut tx = self.db.begin().await.expect("begin tx");

        let user = repository::create_user_tx(&mut tx, email, "Seeded User", None, &hash, None)
            .await
            .expect("insert user");

        for role in roles {
            // The user row was created in this very tx, so no access-cache
            // entry can exist for it yet.
            permissions_repository::assign_role_by_name(&mut tx, user.id, role)
                .await
                .expect("assign role")
                .assume_uncached();
        }

        tx.commit().await.expect("commit seed_user_with_roles");

        let token = dream_fly_backend::utils::jwt::encode_access_token(
            &self.config.auth,
            user.id,
            email,
        )
        .expect("encode access token");

        (user.id, token)
    }

    /// Convenience for tests that want a ready-to-use admin account.
    pub async fn seed_admin(&self) -> (Uuid, String) {
        let email = format!("admin-{}@test.local", Uuid::now_v7());
        self.seed_user_with_roles(&email, &["admin"]).await
    }

    /// Deterministically wait for every background task spawned so far
    /// (e.g. the password-reset email send in `auth::service::
    /// forgot_password`) to finish, replacing a fixed `sleep` + poll.
    ///
    /// Closes the tracker, awaits `wait()`, then reopens it so the same
    /// `TestApp` can `drain_background` again later in the same test.
    ///
    /// Preconditions (caller's responsibility):
    /// - Call sequentially within a single test only — `close()` does not
    ///   prevent new spawns, so a concurrent request against an endpoint
    ///   that spawns onto `background` while a drain is in flight can race
    ///   with it.
    /// - Reopening after each drain is what makes repeated calls within the
    ///   same test safe.
    pub async fn drain_background(&self) {
        self.background.close();
        self.background.wait().await;
        self.background.reopen();
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
    spawn(
        db,
        adjust,
        Arc::new(InMemoryEphemeralStore::new()),
        Arc::new(StaticHealthProbe::up()),
    )
    .await
}

/// Spawn a `TestApp` over the given short-lived state store instead of a
/// fresh in-memory one — e.g. `FailingEphemeralStore` to pin what the
/// middleware does when the store is down.
pub async fn spawn_test_app_with_store(db: PgPool, ephemeral: Arc<dyn EphemeralStore>) -> TestApp {
    spawn(db, |_| {}, ephemeral, Arc::new(StaticHealthProbe::up())).await
}

/// Spawn a `TestApp` whose `/health` answers from the given probe instead of
/// `StaticHealthProbe::up()` — e.g. `StaticHealthProbe::redis_down()` to pin
/// the degraded body.
pub async fn spawn_test_app_with_health(db: PgPool, health: Arc<dyn HealthProbe>) -> TestApp {
    spawn(db, |_| {}, Arc::new(InMemoryEphemeralStore::new()), health).await
}

async fn spawn<F: FnOnce(&mut AppConfig)>(
    db: PgPool,
    adjust: F,
    ephemeral: Arc<dyn EphemeralStore>,
    health: Arc<dyn HealthProbe>,
) -> TestApp {
    let config = test_app_config(adjust);

    let email = Arc::new(MockEmailClient::new());
    let clock = Arc::new(MockClock::new());
    let access_cache = Arc::new(InMemoryAccessCache::new());
    let config_arc = Arc::new(config);

    // SMS and Google identity stay the real adapters, not fakes: SMS tests
    // redirect them to a `wiremock` server via `config.sms.twilio_base_url`
    // (see `common::twilio`), `/auth/google` HTTP tests via
    // `config.auth.google_token_url`/`google_jwks_url`.
    let state = AppState::new(
        db.clone(),
        config_arc.clone(),
        Infra {
            access_cache: access_cache.clone(),
            ephemeral,
            health,
        },
        Overrides {
            email: Some(email.clone()),
            clock: Some(clock.clone()),
            ..Default::default()
        },
    )
    .expect("build test AppState");
    // Taken before `build_router` moves `state` away — see the doc on
    // `AppState::background_tasks`.
    let background = state.background_tasks.clone();

    let router = startup::build_router(state);
    let mut server = TestServer::new(router);

    // With `trust_proxy=true` in the test config, the rate-limit middleware
    // reads this header as the client identity.
    let client_ip = next_client_ip();
    server.add_header("x-forwarded-for", client_ip.to_string());

    TestApp {
        server,
        db,
        config: config_arc,
        email,
        clock,
        access_cache,
        client_ip,
        background,
    }
}
