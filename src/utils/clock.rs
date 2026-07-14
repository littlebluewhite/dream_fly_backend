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
//! (unlike `EmailSender`/`SmsSender`, which perform real network sends and
//! so need `async fn`).
//!
//! This seam does **not** cover every wall-clock read in the codebase.
//! Pinning a `MockClock` in a test has no effect on any of the following:
//! - PostgreSQL `NOW()` — `created_at`/`updated_at`/`marked_at` column
//!   defaults, and the coupon `expires_at > now()` predicate, all evaluate
//!   against the database server's own clock, not this seam.
//! - JWT `exp` — validated by the `jsonwebtoken` crate against the system
//!   clock.
//! - Rate-limit TTLs — Redis `EXPIRE`/`TTL`, timed by Redis's own clock.

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
