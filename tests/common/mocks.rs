//! In-memory stand-in for `EmailSender` (plus a pinnable `Clock`) so HTTP
//! tests can assert on outbound email without hitting real SMTP. SMS has no
//! mock here: `SmsClient` has no trait seam, only a config-URL + `wiremock`
//! seam — see `common::twilio`.
//!
//! `EmailSender` is a single-method trait (`send_password_reset`), so every
//! send recorded in the `Mutex<Vec<_>>` here is just `{to, token}` — the
//! rendered HTML body lives below the seam, covered by `utils::email`'s own
//! unit tests, not re-rendered here.

#![allow(dead_code)]

use std::sync::{Mutex, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use dream_fly_backend::error::AppError;
use dream_fly_backend::utils::clock::Clock;
use dream_fly_backend::utils::email::EmailSender;

#[derive(Debug, Clone)]
pub struct SentPasswordReset {
    pub to: String,
    pub token: String,
}

pub struct MockEmailClient {
    sent: Mutex<Vec<SentPasswordReset>>,
}

impl MockEmailClient {
    pub fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
        }
    }

    /// Take a snapshot of everything sent so far without clearing.
    pub fn sent(&self) -> Vec<SentPasswordReset> {
        self.sent.lock().unwrap().clone()
    }
}

impl Default for MockEmailClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmailSender for MockEmailClient {
    async fn send_password_reset(&self, to: &str, token: &str) -> Result<(), AppError> {
        self.sent.lock().unwrap().push(SentPasswordReset {
            to: to.to_string(),
            token: token.to_string(),
        });
        Ok(())
    }
}

// ---------------- Clock ----------------

/// In-memory stand-in for `Clock` so tests can pin or advance "now" without
/// racing the real system clock.
///
/// Defaults (`None`) to delegating every `now()` call to the real
/// `Utc::now()` — most HTTP tests never call `set`, and existing tests that
/// compute their own `Utc::now().date_naive()` around a request must keep
/// seeing the real wall clock. A "freeze at spawn" default would instead
/// reintroduce exactly the UTC-midnight flake that `studio_clock`'s "`now`
/// is always a parameter" design was meant to eliminate.
pub struct MockClock {
    pinned: RwLock<Option<DateTime<Utc>>>,
}

impl MockClock {
    pub fn new() -> Self {
        Self { pinned: RwLock::new(None) }
    }

    /// Pin the clock to `t`. Subsequent `now()` calls return `t` until the
    /// next `set`/`advance`.
    pub fn set(&self, t: DateTime<Utc>) {
        *self.pinned.write().unwrap() = Some(t);
    }

    /// Move the pinned instant forward (or backward) by `d`. Panics if the
    /// clock hasn't been `set` yet: advancing an unset (delegating) clock
    /// would silently start pinning it at `Utc::now() + d`, a surprising
    /// state change a caller almost certainly didn't intend.
    pub fn advance(&self, d: Duration) {
        let mut guard = self.pinned.write().unwrap();
        let current = guard.expect("MockClock::advance called before set — clock is not pinned");
        *guard = Some(current + d);
    }
}

impl Default for MockClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        self.pinned.read().unwrap().unwrap_or_else(Utc::now)
    }
}
