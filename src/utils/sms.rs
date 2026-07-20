use reqwest::Client;

use crate::config::SmsConfig;
use crate::error::AppError;

/// Outbound SMS via the Twilio Messages API. The base URL is a config seam
/// (`SmsConfig::twilio_base_url`, mirroring `AuthConfig::google_token_url`):
/// production points it at the real `https://api.twilio.com`, integration
/// tests point it at a `wiremock` server instead of substituting a mock
/// trait implementation. `AppState` holds this as a concrete `Arc<SmsClient>`
/// — there is only ever one adapter, so no trait object is needed.
pub struct SmsClient {
    client: Client,
    account_sid: String,
    auth_token: String,
    from_number: String,
    base_url: String,
}

impl SmsClient {
    /// Build a new Twilio client using the provided HTTP client (which keeps
    /// its connection pool across requests).
    pub fn new(config: &SmsConfig, client: Client) -> Self {
        Self {
            client,
            account_sid: config.twilio_account_sid.clone(),
            auth_token: config.twilio_auth_token.clone(),
            from_number: config.twilio_from_number.clone(),
            base_url: config.twilio_base_url.clone(),
        }
    }

    pub async fn send_sms(&self, to: &str, message: &str) -> Result<(), AppError> {
        let url = format!(
            "{}/2010-04-01/Accounts/{}/Messages.json",
            self.base_url, self.account_sid
        );

        let params = [
            ("To", to),
            ("From", &self.from_number),
            ("Body", message),
        ];

        let response = self
            .client
            .post(&url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&params)
            .send()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to send SMS request: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            // Structured logging: keep Twilio's body out of the error chain
            // (it can contain PII like the destination phone number).
            tracing::error!(status = %status, body = %body, "Twilio API error");
            return Err(AppError::Internal(anyhow::anyhow!(
                "sms delivery failed with status {}",
                status
            )));
        }

        // Don't log raw phone numbers; log only that the send succeeded.
        tracing::info!("SMS sent successfully");
        Ok(())
    }

    pub async fn send_otp(&self, to: &str, code: &str) -> Result<(), AppError> {
        let message = format!(
            "Your Dream Fly verification code is: {}. Valid for 5 minutes.",
            code
        );
        self.send_sms(to, &message).await
    }
}
