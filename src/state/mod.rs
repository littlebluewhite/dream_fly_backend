use std::sync::Arc;

use sqlx::PgPool;
use tokio_util::task::TaskTracker;

use crate::config::AppConfig;
use crate::health::HealthProbe;
use crate::modules::auth::access::AccessCache;
use crate::utils::clock::Clock;
use crate::utils::email::EmailSender;
use crate::utils::ephemeral::EphemeralStore;
use crate::utils::google_oauth::GoogleIdentityProvider;
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

impl AppState {
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
