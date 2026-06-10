//! In-memory stand-ins for `EmailSender` and `SmsSender` so HTTP tests can
//! assert on outbound messages without hitting real SMTP / Twilio.
//!
//! Every send is recorded in a `Mutex<Vec<_>>` the test can inspect. The
//! mocks also expose a `fail_next()` switch for negative-path tests that
//! need to simulate an outage — though in practice most service-layer code
//! spawns the send fire-and-forget and swallows failures, so the switch is
//! mainly useful for middleware-style coverage.

#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;

use dream_fly_backend::error::AppError;
use dream_fly_backend::utils::email::EmailSender;
use dream_fly_backend::utils::sms::SmsSender;

#[derive(Debug, Clone)]
pub struct SentEmail {
    pub to: String,
    pub subject: String,
    pub body: String,
    pub kind: EmailKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailKind {
    Generic,
    PasswordReset { token: String },
    Welcome { name: String },
}

pub struct MockEmailClient {
    sent: Mutex<Vec<SentEmail>>,
    fail: Mutex<bool>,
}

impl MockEmailClient {
    pub fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            fail: Mutex::new(false),
        }
    }

    /// Take a snapshot of everything sent so far without clearing.
    pub fn sent(&self) -> Vec<SentEmail> {
        self.sent.lock().unwrap().clone()
    }

    /// Wait up to `max_ms` milliseconds for at least `n` emails to be recorded.
    /// Used by tests that exercise code paths which spawn the send onto a
    /// background task (e.g. `forgot_password`) and therefore do not complete
    /// the send before the handler returns.
    pub async fn wait_for(&self, n: usize, max_ms: u64) -> Vec<SentEmail> {
        let step = 10u64;
        let mut waited = 0u64;
        while waited < max_ms {
            {
                let guard = self.sent.lock().unwrap();
                if guard.len() >= n {
                    return guard.clone();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(step)).await;
            waited += step;
        }
        self.sent.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }

    pub fn fail_next(&self) {
        *self.fail.lock().unwrap() = true;
    }

    fn should_fail(&self) -> bool {
        let mut g = self.fail.lock().unwrap();
        let v = *g;
        *g = false;
        v
    }

    fn record(&self, email: SentEmail) -> Result<(), AppError> {
        if self.should_fail() {
            return Err(AppError::Internal(anyhow::anyhow!("mock email failure")));
        }
        self.sent.lock().unwrap().push(email);
        Ok(())
    }
}

impl Default for MockEmailClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmailSender for MockEmailClient {
    async fn send_email(
        &self,
        to: &str,
        subject: &str,
        body: String,
    ) -> Result<(), AppError> {
        self.record(SentEmail {
            to: to.to_string(),
            subject: subject.to_string(),
            body,
            kind: EmailKind::Generic,
        })
    }

    async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), AppError> {
        self.record(SentEmail {
            to: to.to_string(),
            subject: "Dream Fly — Password Reset".into(),
            body: String::new(),
            kind: EmailKind::PasswordReset {
                token: token.to_string(),
            },
        })
    }

    async fn send_welcome(&self, to: &str, name: &str) -> Result<(), AppError> {
        self.record(SentEmail {
            to: to.to_string(),
            subject: "Welcome to Dream Fly!".into(),
            body: String::new(),
            kind: EmailKind::Welcome {
                name: name.to_string(),
            },
        })
    }
}

// ---------------- SMS ----------------

#[derive(Debug, Clone)]
pub struct SentSms {
    pub to: String,
    pub message: String,
    pub kind: SmsKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmsKind {
    Generic,
    Otp { code: String },
}

pub struct MockSmsClient {
    sent: Mutex<Vec<SentSms>>,
    fail: Mutex<bool>,
}

impl MockSmsClient {
    pub fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            fail: Mutex::new(false),
        }
    }

    pub fn sent(&self) -> Vec<SentSms> {
        self.sent.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }

    pub fn fail_next(&self) {
        *self.fail.lock().unwrap() = true;
    }

    /// Return the OTP code from the most recent OTP send, if any. Used by
    /// flow tests that need to read the generated code out of the mock to
    /// then feed it back into `/auth/otp/verify`.
    pub fn last_otp_code(&self) -> Option<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|s| match &s.kind {
                SmsKind::Otp { code } => Some(code.clone()),
                _ => None,
            })
    }

    fn should_fail(&self) -> bool {
        let mut g = self.fail.lock().unwrap();
        let v = *g;
        *g = false;
        v
    }

    fn record(&self, s: SentSms) -> Result<(), AppError> {
        if self.should_fail() {
            return Err(AppError::Internal(anyhow::anyhow!("mock sms failure")));
        }
        self.sent.lock().unwrap().push(s);
        Ok(())
    }
}

impl Default for MockSmsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SmsSender for MockSmsClient {
    async fn send_sms(&self, to: &str, message: &str) -> Result<(), AppError> {
        self.record(SentSms {
            to: to.to_string(),
            message: message.to_string(),
            kind: SmsKind::Generic,
        })
    }

    async fn send_otp(&self, to: &str, code: &str) -> Result<(), AppError> {
        self.record(SentSms {
            to: to.to_string(),
            message: format!("OTP: {code}"),
            kind: SmsKind::Otp {
                code: code.to_string(),
            },
        })
    }
}
