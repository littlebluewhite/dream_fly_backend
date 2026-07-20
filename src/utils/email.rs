use async_trait::async_trait;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use lettre::message::header::ContentType;

use crate::config::EmailConfig;
use crate::error::AppError;

/// Trait-object facade for outbound email so AppState can hold a
/// `Arc<dyn EmailSender>`. In production this is backed by `EmailClient`
/// (lettre SMTP); in integration tests it is backed by `MockEmailClient`
/// which records every send without hitting a real SMTP server.
#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), AppError>;
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
    async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), AppError> {
        let message = password_reset_message(&self.from_name, &self.from_email, to, token)?;

        self.mailer
            .send(message)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to send email: {e}")))?;

        Ok(())
    }
}

/// Build a lettre `Message` from already-composed parts. No I/O — the only
/// failure mode is an unparsable `from`/`to` address.
fn build_message(
    from_name: &str,
    from_email: &str,
    to: &str,
    subject: &str,
    html_body: String,
) -> Result<Message, AppError> {
    let from = format!("{from_name} <{from_email}>");

    Message::builder()
        .from(from.parse().map_err(|e| {
            AppError::Internal(anyhow::anyhow!("invalid from address: {e}"))
        })?)
        .to(to.parse().map_err(|e| {
            AppError::Internal(anyhow::anyhow!("invalid to address: {e}"))
        })?)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(html_body)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed to build email: {e}")))
}

const PASSWORD_RESET_SUBJECT: &str = "Dream Fly — Password Reset";

fn password_reset_body(token: &str) -> String {
    format!(
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
    )
}

/// Compose the password-reset email: subject constant + body template +
/// [`build_message`]'s address/header wiring. This is the exact call
/// `EmailClient::send_password_reset` makes, so unit tests target this
/// function directly rather than `build_message`/`password_reset_body` in
/// isolation — that way a wiring mistake (wrong subject, wrong body helper,
/// wrong content type) fails the test instead of shipping silently.
fn password_reset_message(
    from_name: &str,
    from_email: &str,
    to: &str,
    token: &str,
) -> Result<Message, AppError> {
    build_message(
        from_name,
        from_email,
        to,
        PASSWORD_RESET_SUBJECT,
        password_reset_body(token),
    )
}

#[cfg(test)]
mod tests {
    use lettre::message::header::Subject;

    use super::*;

    /// Decode quoted-printable back to plain text. `password_reset_body`'s
    /// HTML always has a line (the reset-link anchor) past the 76-column
    /// soft-wrap threshold, so lettre always picks
    /// `Content-Transfer-Encoding: quoted-printable` for it — decoding
    /// before asserting checks the actual content instead of coupling the
    /// test to that transport-encoding detail (e.g. `=` becomes `=3D`, long
    /// lines get a `=\r\n` soft break).
    fn decode_body(formatted: &[u8]) -> String {
        let text = String::from_utf8(formatted.to_vec()).expect("message is valid utf-8");
        let (_headers, body) = text
            .split_once("\r\n\r\n")
            .expect("formatted message has a header/body separator");
        let decoded = quoted_printable::decode(body, quoted_printable::ParseMode::Robust)
            .expect("valid quoted-printable body");
        String::from_utf8(decoded).expect("decoded body is valid utf-8")
    }

    /// Targets `password_reset_message` — the actual line
    /// `EmailClient::send_password_reset` calls — rather than
    /// `build_message`/`password_reset_body` separately, so a wiring
    /// mistake (wrong subject, wrong body helper, wrong content type) fails
    /// this test instead of shipping silently.
    #[test]
    fn password_reset_message_wires_subject_body_and_headers() {
        let token = "tok-abc123";
        let msg = password_reset_message(
            "Dream Fly",
            "no-reply@dreamfly.com",
            "user@example.com",
            token,
        )
        .expect("valid from/to addresses build a message");

        assert_eq!(
            msg.envelope().from().expect("from is set").to_string(),
            "no-reply@dreamfly.com"
        );
        assert_eq!(msg.envelope().to().len(), 1);
        assert_eq!(msg.envelope().to()[0].to_string(), "user@example.com");

        let subject = msg.headers().get::<Subject>().expect("subject is set");
        assert_eq!(subject.as_ref(), PASSWORD_RESET_SUBJECT);

        assert_eq!(
            msg.headers().get::<ContentType>(),
            Some(ContentType::TEXT_HTML)
        );

        let body = decode_body(&msg.formatted());
        assert!(body.contains(&format!("reset-password?token={token}")));
        assert!(body.contains("15 minutes"));
    }

    #[test]
    fn build_message_rejects_invalid_address() {
        let result = build_message(
            "Dream Fly",
            "no-reply@dreamfly.com",
            "not-a-valid-address",
            PASSWORD_RESET_SUBJECT,
            "<p>body</p>".to_string(),
        );

        assert!(result.is_err());
    }
}
