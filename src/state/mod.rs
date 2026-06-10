use std::sync::Arc;

use rdkafka::producer::FutureProducer;
use sqlx::PgPool;

use crate::config::AppConfig;
use crate::utils::email::EmailSender;
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
}
