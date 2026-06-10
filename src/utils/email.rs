use async_trait::async_trait;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use lettre::message::header::ContentType;

use crate::config::EmailConfig;
use crate::error::AppError;

/// Escape user-supplied strings before interpolating into HTML email bodies.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Trait-object facade for outbound email so AppState can hold a
/// `Arc<dyn EmailSender>`. In production this is backed by `EmailClient`
/// (lettre SMTP); in integration tests it is backed by `MockEmailClient`
/// which records every send without hitting a real SMTP server.
#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send_email(
        &self,
        to: &str,
        subject: &str,
        body: String,
    ) -> Result<(), AppError>;

    async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), AppError>;

    async fn send_welcome(&self, to: &str, name: &str) -> Result<(), AppError>;
}

pub struct EmailClient {
    mailer: AsyncSmtpTransport<Tokio1Executor>,
    from_email: String,
    from_name: String,
}

impl EmailClient {
    pub fn new(config: &EmailConfig) -> Result<Self, AppError> {
        let creds = Credentials::new(
            config.smtp_username.clone(),
            config.smtp_password.clone(),
        );

        let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("SMTP relay error: {e}")))?
            .port(config.smtp_port)
            .credentials(creds)
            .build();

        Ok(Self {
            mailer,
            from_email: config.from_email.clone(),
            from_name: config.from_name.clone(),
        })
    }
}

#[async_trait]
impl EmailSender for EmailClient {
    async fn send_email(
        &self,
        to: &str,
        subject: &str,
        body: String,
    ) -> Result<(), AppError> {
        let from = format!("{} <{}>", self.from_name, self.from_email);

        let message = Message::builder()
            .from(from.parse().map_err(|e| {
                AppError::Internal(anyhow::anyhow!("invalid from address: {e}"))
            })?)
            .to(to.parse().map_err(|e| {
                AppError::Internal(anyhow::anyhow!("invalid to address: {e}"))
            })?)
            .subject(subject)
            .header(ContentType::TEXT_HTML)
            .body(body)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to build email: {e}")))?;

        self.mailer
            .send(message)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to send email: {e}")))?;

        Ok(())
    }

    async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), AppError> {
        let subject = "Dream Fly — Password Reset";
        let body = format!(
            r#"<html>
<body>
<h2>Password Reset Request</h2>
<p>You requested a password reset for your Dream Fly account.</p>
<p>Click the link below to reset your password:</p>
<p><a href="https://dreamfly.com/reset-password?token={token}">Reset Password</a></p>
<p>If you did not request this, please ignore this email.</p>
<p>This link will expire in 15 minutes.</p>
</body>
</html>"#
        );

        self.send_email(to, subject, body).await
    }

    async fn send_welcome(&self, to: &str, name: &str) -> Result<(), AppError> {
        let subject = "Welcome to Dream Fly!";
        let safe_name = html_escape(name);
        let body = format!(
            r#"<html>
<body>
<h2>Welcome, {safe_name}!</h2>
<p>Thank you for joining Dream Fly.</p>
<p>We're excited to have you on board. Start exploring our courses, book sessions with expert coaches, and take your skills to the next level.</p>
<p>If you have any questions, feel free to reach out to our support team.</p>
<p>Happy flying!</p>
<p>— The Dream Fly Team</p>
</body>
</html>"#
        );

        self.send_email(to, subject, body).await
    }
}
