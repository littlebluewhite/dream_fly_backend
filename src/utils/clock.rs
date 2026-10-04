//! Clock seam — lets a handler's sampled "now" be threaded into services as
//! a plain parameter instead of each service calling `Utc::now()` itself, so
//! wall-clock-dependent business logic can be tested against a fixed
//! instant. This extends `utils::studio_clock`'s module philosophy one layer
//! up: `studio_clock`'s functions already take `now`/`tz` as parameters and
//! never read the clock internally (see its module doc); `Clock` is what
//! lets the handler layer above it sample "now" from a swappable source
//! instead of a bare `Utc::now()`. `Clock` is a trait object only at that
//! handler boundary (`AppState::clock: Arc<dyn Clock>`) — once sampled, the
//! instant flows down through services as an ordinary `now: DateTime<Utc>`
//! argument.
//!
//! Synchronous trait, not `async_trait`: reading the clock is not I/O
//! (unlike `EmailSender`/`SmsClient`, which perform real network sends and
//! so need `async fn`).
//!
//! This seam does **not** cover every wall-clock read in the codebase.
//! Pinning a `MockClock` in a test has no effect on any of the following:
//! - PostgreSQL `NOW()`——取的是資料庫伺服器自己的時鐘。ADR-0017:業務時間
//!   (會被讀取拿去分桶、或拿去跟取樣時刻比較的欄位)要把 handler 取樣的
//!   `StudioNow.now` 綁進 SQL;稽核時間(`updated_at` 等)刻意留給 `NOW()`,
//!   attendance `marked_at`、leave `decided_at`、`clock_records`、messages、
//!   waitlist `created_at` 這輪也一律當稽核時間。尚未改綁的業務時間站點:
//!   - `subscription_derived_status` 內的 `now()`(W7-7,延後到有測試需要時)。
//! - JWT `exp` — validated by the `jsonwebtoken` crate against the system
//!   clock.
//! - Short-lived state TTLs (`utils::ephemeral`: rate limits, OTP, reset
//!   tokens) — timed by the `EphemeralStore` adapter (Redis's own clock in
//!   production, `std::time::Instant` in the in-memory test adapter).
//! - Account access cache TTLs (`auth::access`, 60s is_active / 900s
//!   roles) — timed by the `AccessCache` adapter (Redis's own clock in
//!   production, `std::time::Instant` in the in-memory test adapter).

use chrono::{DateTime, Utc};

/// Trait-object facade for "now" so `AppState` can hold an `Arc<dyn Clock>`.
/// In production this is backed by [`SystemClock`] (`Utc::now()`); in
/// integration tests it is backed by `MockClock` (`tests/common/mocks.rs`),
/// which can pin the clock to a fixed instant or delegate to the real system
/// clock.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Production implementation — wraps `Utc::now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
