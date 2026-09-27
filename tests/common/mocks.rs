//! In-memory stand-in for `EmailSender` (plus a pinnable `Clock`) so HTTP
//! tests can assert on outbound email without hitting real SMTP. SMS has no
//! mock here: `SmsClient` has no trait seam, only a config-URL + `wiremock`
//! seam — see `common::twilio`.
//!
//! `EmailSender` is a single-method trait (`send_password_reset`), so every
//! send recorded in the `Mutex<Vec<_>>` here is just `{to, token}` — the
//! rendered HTML body lives below the seam, covered by `utils::email`'s own
//! unit tests, not re-rendered here.
//!
//! `InMemoryAccessCache`/`FailingAccessCache` are the test adapters for the
//! `auth::access::AccessCache` seam; `tests/service_access.rs` runs the same
//! scenarios against them and the production Redis adapter.
//!
//! `InMemoryEphemeralStore`/`FailingEphemeralStore` do the same for the
//! `utils::ephemeral::EphemeralStore` seam (`tests/service_ephemeral.rs`).
//!
//! `FakeGoogleIdentity` stands in for `utils::google_oauth::
//! GoogleIdentityProvider` in service-level login-rule tests; the real
//! adapter is covered against `wiremock` in `tests/google_identity.rs`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::auth::access::AccessCache;
use dream_fly_backend::utils::clock::Clock;
use dream_fly_backend::utils::email::EmailSender;
use dream_fly_backend::utils::ephemeral::EphemeralStore;
use dream_fly_backend::utils::google_oauth::{GoogleIdentity, GoogleIdentityProvider};

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

/// In-memory `AccessCache`: a `Mutex<HashMap>` of value + deadline, expired
/// lazily on read (same observable semantics as Redis `SET EX`/`GET`).
#[derive(Default)]
pub struct InMemoryAccessCache {
    entries: Mutex<HashMap<String, (String, Instant)>>,
}

impl InMemoryAccessCache {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AccessCache for InMemoryAccessCache {
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let mut entries = self.entries.lock().expect("access cache lock");
        match entries.get(key) {
            Some((val, deadline)) if Instant::now() < *deadline => Ok(Some(val.clone())),
            Some(_) => {
                entries.remove(key);
                Ok(None)
            }
            None => Ok(None),
        }
    }

    async fn set_ex(&self, key: &str, val: &str, ttl_secs: u64) -> anyhow::Result<()> {
        let deadline = Instant::now() + std::time::Duration::from_secs(ttl_secs);
        self.entries
            .lock()
            .expect("access cache lock")
            .insert(key.to_string(), (val.to_string(), deadline));
        Ok(())
    }

    async fn del(&self, keys: &[String]) -> anyhow::Result<()> {
        let mut entries = self.entries.lock().expect("access cache lock");
        for key in keys {
            entries.remove(key);
        }
        Ok(())
    }
}

/// `AccessCache` whose every call fails — pins the fail-open policy in
/// `auth::access` (errors are a miss, never an error to the caller).
pub struct FailingAccessCache;

#[async_trait]
impl AccessCache for FailingAccessCache {
    async fn get(&self, _key: &str) -> anyhow::Result<Option<String>> {
        Err(anyhow::anyhow!("access cache unavailable"))
    }

    async fn set_ex(&self, _key: &str, _val: &str, _ttl_secs: u64) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("access cache unavailable"))
    }

    async fn del(&self, _keys: &[String]) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("access cache unavailable"))
    }
}

/// In-memory `EphemeralStore`: a `Mutex<HashMap>` of value + deadline,
/// expired lazily on access. `incr_with_ttl` sets the deadline only when it
/// creates the key — the same semantics as the Redis adapter's Lua script.
#[derive(Default)]
pub struct InMemoryEphemeralStore {
    entries: Mutex<HashMap<String, (String, Instant)>>,
}

impl InMemoryEphemeralStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the map with every expired entry already dropped.
    fn live(&self) -> std::sync::MutexGuard<'_, HashMap<String, (String, Instant)>> {
        let mut entries = self.entries.lock().expect("ephemeral store lock");
        let now = Instant::now();
        entries.retain(|_, (_, deadline)| now < *deadline);
        entries
    }
}

#[async_trait]
impl EphemeralStore for InMemoryEphemeralStore {
    async fn incr_with_ttl(&self, key: &str, ttl_secs: u64) -> anyhow::Result<i64> {
        let mut entries = self.live();
        match entries.get_mut(key) {
            Some((val, _deadline)) => {
                let count = val.parse::<i64>()? + 1;
                *val = count.to_string();
                Ok(count)
            }
            None => {
                let deadline = Instant::now() + std::time::Duration::from_secs(ttl_secs);
                entries.insert(key.to_string(), ("1".to_string(), deadline));
                Ok(1)
            }
        }
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.live().get(key).map(|(val, _)| val.clone()))
    }

    async fn set_ex(&self, key: &str, val: &str, ttl_secs: u64) -> anyhow::Result<()> {
        let deadline = Instant::now() + std::time::Duration::from_secs(ttl_secs);
        self.live()
            .insert(key.to_string(), (val.to_string(), deadline));
        Ok(())
    }

    async fn del(&self, key: &str) -> anyhow::Result<()> {
        self.live().remove(key);
        Ok(())
    }

    async fn getdel(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.live().remove(key).map(|(val, _)| val))
    }
}

/// `EphemeralStore` whose every call fails — pins each owner's fail-open /
/// fail-closed policy.
pub struct FailingEphemeralStore;

#[async_trait]
impl EphemeralStore for FailingEphemeralStore {
    async fn incr_with_ttl(&self, _key: &str, _ttl_secs: u64) -> anyhow::Result<i64> {
        Err(anyhow::anyhow!("ephemeral store unavailable"))
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<String>> {
        Err(anyhow::anyhow!("ephemeral store unavailable"))
    }

    async fn set_ex(&self, _key: &str, _val: &str, _ttl_secs: u64) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("ephemeral store unavailable"))
    }

    async fn del(&self, _key: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("ephemeral store unavailable"))
    }

    async fn getdel(&self, _key: &str) -> anyhow::Result<Option<String>> {
        Err(anyhow::anyhow!("ephemeral store unavailable"))
    }
}

/// `GoogleIdentityProvider` that ignores the code and either returns a fixed
/// verified identity or rejects like the real adapter does on a bad code.
pub struct FakeGoogleIdentity {
    identity: Option<GoogleIdentity>,
}

impl FakeGoogleIdentity {
    /// Every code verifies as `sub`/`email` (no name, no picture).
    pub fn verified(sub: &str, email: &str) -> Self {
        Self {
            identity: Some(GoogleIdentity {
                sub: sub.to_string(),
                email: email.to_string(),
                name: None,
                picture: None,
            }),
        }
    }

    /// Every code is refused with the adapter's generic BadRequest.
    pub fn rejecting() -> Self {
        Self { identity: None }
    }
}

#[async_trait]
impl GoogleIdentityProvider for FakeGoogleIdentity {
    async fn verify_code(&self, _code: &str) -> Result<GoogleIdentity, AppError> {
        self.identity
            .clone()
            .ok_or_else(|| AppError::BadRequest("Google authentication failed".into()))
    }
}
