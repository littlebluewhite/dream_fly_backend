use std::sync::Arc;

use rdkafka::producer::FutureProducer;
use sqlx::PgPool;
use tokio_util::task::TaskTracker;

use crate::config::AppConfig;
use crate::utils::clock::Clock;
use crate::utils::email::EmailSender;
use crate::utils::google_oauth::JwksCache;
use crate::utils::sms::SmsClient;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub redis: redis::aio::ConnectionManager,
    pub kafka_producer: Option<Arc<FutureProducer>>,
    pub config: Arc<AppConfig>,
    /// Shared HTTP client with connection pooling (Google OAuth, Twilio, etc.).
    pub http_client: reqwest::Client,
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
    /// Per-app cache of Google's JWKS for id_token signature verification.
    /// Concrete type (single implementation): each app instance — including
    /// every test — owns its own cache, so the previous process-global slot's
    /// cross-instance sharing is gone. Test substitution is per-app: a fresh
    /// cache plus the `google_jwks_url` config seam pointed at wiremock.
    pub jwks_cache: Arc<JwksCache>,
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
    /// - Tracker identity: this field holds a **clone** — the original
    ///   binding is kept by whoever constructs `AppState` (`main` / the test
    ///   harness), specifically so shutdown/drain code can still reach it
    ///   after `AppState` itself has been moved into `startup::build_router`.
    ///   `TaskTracker` is an `Arc`-style handle, so the clone shares the same
    ///   underlying task set as the original.
    ///
    /// Never spawn a long-running/daemon loop (refresh-token cleanup, Kafka
    /// consumer/outbox dispatcher, ...) on this tracker: `wait()` only
    /// returns once the tracker is both closed AND empty, so a loop that
    /// never exits would make shutdown/drain hang forever.
    pub background_tasks: TaskTracker,
}
