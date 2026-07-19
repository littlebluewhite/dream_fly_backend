use std::sync::Arc;

use rdkafka::producer::FutureProducer;
use sqlx::PgPool;

use crate::config::AppConfig;
use crate::utils::clock::Clock;
use crate::utils::email::EmailSender;
use crate::utils::google_oauth::JwksCache;
use crate::utils::sms::SmsSender;

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
    /// Shared outbound SMS sender. Held as a trait object so integration
    /// tests can substitute an in-memory recorder without touching Twilio.
    pub sms_client: Arc<dyn SmsSender>,
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
}
