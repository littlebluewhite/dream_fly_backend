use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tokio_util::task::TaskTracker;

use crate::config::AppConfig;
use crate::health::{HealthProbe, RedisHealthProbe};
use crate::modules::auth::access::{AccessCache, RedisAccessCache};
use crate::utils::clock::{Clock, SystemClock};
use crate::utils::email::{EmailClient, EmailSender};
use crate::utils::ephemeral::{EphemeralStore, RedisEphemeralStore};
use crate::utils::google_oauth::{GoogleIdentityProvider, GoogleOAuthClient};
use crate::utils::sms::SmsClient;
use crate::utils::studio_clock::StudioNow;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    /// Account access cache (`auth::access`) — the is_active/role cache the
    /// `AuthUser` extractor reads. Held as a trait object so integration
    /// tests can substitute an in-memory adapter.
    pub access_cache: Arc<dyn AccessCache>,
    /// Short-lived state (`utils::ephemeral`) — route rate-limit buckets,
    /// login/forgot-password counters, OTP, password-reset tokens. Held as a
    /// trait object so integration tests can substitute an in-memory adapter.
    pub ephemeral: Arc<dyn EphemeralStore>,
    /// `/health` 的依賴探針(`health`):Redis PING 與開機時的 Kafka 旗標。
    /// Held as a trait object so integration tests can substitute
    /// `StaticHealthProbe` and run without Redis.
    pub health: Arc<dyn HealthProbe>,
    pub config: Arc<AppConfig>,
    /// Shared outbound email sender. Built once at startup to avoid rebuilding
    /// the TLS stack on every password-reset request. Held as a trait object
    /// so integration tests can substitute an in-memory recorder.
    pub email_client: Arc<dyn EmailSender>,
    /// Shared outbound SMS client (Twilio via reqwest). Concrete type (single
    /// implementation): the base URL is a config seam (`SmsConfig::
    /// twilio_base_url`) that integration tests point at a `wiremock` server
    /// instead of swapping the implementation — mirrors `AuthConfig::
    /// google_token_url`/`google_jwks_url`.
    pub sms_client: Arc<SmsClient>,
    /// Source of "now" for handler-sampled wall-clock decisions. Held as a
    /// trait object so integration tests can pin or advance it via
    /// `MockClock` instead of racing the real system clock.
    pub clock: Arc<dyn Clock>,
    /// Google identity verification (`utils::google_oauth`): authorization
    /// code in, verified `GoogleIdentity` out. Held as a trait object so
    /// service-level tests can substitute a fake; production and HTTP tests
    /// use `GoogleOAuthClient`, whose token/JWKS URLs are config seams
    /// pointed at `wiremock` in tests.
    pub google_identity: Arc<dyn GoogleIdentityProvider>,
    /// Tracks fire-and-forget background tasks spawned during request
    /// handling (currently: the password-reset email send in
    /// `auth::service::forgot_password`).
    ///
    /// - Spawn semantics unchanged: `background_tasks.spawn(..)` is a
    ///   drop-in replacement for `tokio::spawn(..)` — the task still runs
    ///   detached from the request/response cycle.
    /// - Shutdown drain: `main` closes the tracker and awaits `wait()`
    ///   (under a bounded sub-budget) so an in-flight send gets a chance to
    ///   finish instead of being silently abandoned at process exit.
    /// - Test quiescence: `tests/common/http.rs`'s `TestApp::drain_background`
    ///   closes, awaits `wait()`, then reopens the tracker so a test can
    ///   deterministically assert on what a background send did (or didn't
    ///   do) without a fixed `sleep`.
    /// - Tracker identity: [`AppState::new`] is the only place a tracker is
    ///   created. Whoever needs to drain it (`main` / the test harness)
    ///   takes `state.background_tasks.clone()` **before**
    ///   `startup::build_router` moves `state` away; `TaskTracker` is an
    ///   `Arc`-style handle, so the clone shares the same underlying task
    ///   set. Never create a second `TaskTracker::new()` for draining — it
    ///   would wait on an empty tracker while the router's accumulates the
    ///   spawns (silently false-green).
    ///
    /// Never spawn a long-running/daemon loop (refresh-token cleanup, Kafka
    /// consumer/outbox dispatcher, ...) on this tracker: `wait()` only
    /// returns once the tracker is both closed AND empty, so a loop that
    /// never exits would make shutdown/drain hang forever.
    pub background_tasks: TaskTracker,
}

/// 需要連線的 seam——沒有預設:呼叫端必須明確選 adapter(正式環境
/// [`Infra::redis`],測試 harness 選 in-memory 與 `StaticHealthProbe`)。
pub struct Infra {
    pub access_cache: Arc<dyn AccessCache>,
    pub ephemeral: Arc<dyn EphemeralStore>,
    pub health: Arc<dyn HealthProbe>,
}

impl Infra {
    /// 正式組裝:三個 seam 共用同一條 Redis 連線(`ConnectionManager` 是
    /// 便宜的 multiplexed handle);`kafka_connected` 是開機時 producer 是否
    /// 建成,`/health` 照報。
    pub fn redis(conn: ConnectionManager, kafka_connected: bool) -> Self {
        Self {
            access_cache: Arc::new(RedisAccessCache::new(conn.clone())),
            ephemeral: Arc::new(RedisEphemeralStore::new(conn.clone())),
            health: Arc::new(RedisHealthProbe::new(conn, kafka_connected)),
        }
    }
}

/// 建構時不做 IO 的 seam:`None` = 正式 adapter,`Some` = 測試替身。
#[derive(Default)]
pub struct Overrides {
    pub email: Option<Arc<dyn EmailSender>>,
    pub clock: Option<Arc<dyn Clock>>,
    pub google_identity: Option<Arc<dyn GoogleIdentityProvider>>,
}

impl AppState {
    /// `AppState` 唯一建構點(`main` 與測試 harness 共用)。
    pub fn new(
        db: PgPool,
        config: Arc<AppConfig>,
        infra: Infra,
        o: Overrides,
    ) -> anyhow::Result<Self> {
        // Build a shared HTTP client (connection pooling). Keep the global
        // timeout short — handlers that need longer can set per-call timeouts.
        // A 10s global blocks async tasks + DB connections when Google/Twilio
        // stalls; 5s is a reasonable cap.
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;

        // Build the shared SMTP client once (TLS + connection pooling) —
        // only when no override was supplied.
        let email_client: Arc<dyn EmailSender> = match o.email {
            Some(email) => email,
            None => Arc::new(
                EmailClient::new(&config.email)
                    .context("failed to build email client — check APP__EMAIL__* settings")?,
            ),
        };

        // Twilio client — reuses the HTTP connection pool. Always real: test
        // substitution goes through `SmsConfig::twilio_base_url` instead (see
        // `AppState::sms_client`).
        let sms_client = Arc::new(SmsClient::new(&config.sms, http_client.clone()));

        // Google identity (code exchange + JWKS verification) — also reuses
        // the HTTP connection pool.
        let google_identity = o
            .google_identity
            .unwrap_or_else(|| Arc::new(GoogleOAuthClient::new(&config.auth, http_client)));

        let clock = o.clock.unwrap_or_else(|| Arc::new(SystemClock));

        Ok(Self {
            db,
            access_cache: infra.access_cache,
            ephemeral: infra.ephemeral,
            health: infra.health,
            config,
            email_client,
            sms_client,
            clock,
            google_identity,
            background_tasks: TaskTracker::new(),
        })
    }

    /// Single production construction point for [`StudioNow`]: samples
    /// `now` once via the clock seam and pairs it with the configured
    /// studio timezone. Handlers call this once and pass the result down,
    /// rather than each service re-reading `config.server.studio_timezone`
    /// and re-sampling `clock.now()` separately.
    pub fn studio_now(&self) -> StudioNow {
        StudioNow {
            tz: self.config.server.studio_timezone,
            now: self.clock.now(),
        }
    }
}
