//! 帳號層級的限流策略 owner——登入失敗鎖定(每 email 10 次、15 分鐘)與忘
//! 記密碼請求計數(每 email 每小時 3 次)。key 格式、門檻、TTL 都私有於此,
//! 呼叫端(`service.rs`)只問「鎖了沒」「還能不能寄」,看不到儲存。
//!
//! 兩者都 **fail-open**:短期狀態儲存故障時,鎖定檢查當作沒鎖、失敗計數與
//! 清除靜默放棄、忘記密碼當作還沒超量——儲存故障不能讓所有人都登不進來。
//! OTP 的限流不在這裡:它是 fail-closed(會花錢),和碼本身一起住在
//! `otp.rs`。

use crate::utils::ephemeral::EphemeralStore;

/// Max failed login attempts per email before temporary lockout.
const LOGIN_MAX_ATTEMPTS: i64 = 10;
/// Lockout window after hitting the threshold (seconds).
const LOGIN_LOCKOUT_TTL: u64 = 900; // 15 minutes

/// Password-reset emails a single account may trigger per window.
const FORGOT_MAX_REQUESTS: i64 = 3;
/// Forgot-password rate-limit window in seconds.
const FORGOT_WINDOW_SECS: u64 = 3600;

fn login_fail_key(email: &str) -> String {
    format!("login_fail:{email}")
}

fn forgot_rate_key(email: &str) -> String {
    format!("forgot_rate:{email}")
}

/// Whether `email` (already normalized) has hit the failed-login threshold.
/// Fail-open: an unreadable (or unparsable) counter counts as 0.
pub(super) async fn login_locked_out(store: &dyn EphemeralStore, email: &str) -> bool {
    let failures: i64 = store
        .get(&login_fail_key(email))
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if failures >= LOGIN_MAX_ATTEMPTS {
        // A defender inspecting logs will see the counter; the caller answers
        // with the same 401 as bad credentials.
        tracing::warn!(%email, failures, "login blocked: lockout threshold reached");
        return true;
    }
    false
}

/// Count one failed login for `email`. Best-effort: store outages must not
/// prevent authentication entirely.
pub(super) async fn record_login_failure(store: &dyn EphemeralStore, email: &str) {
    let _ = store
        .incr_with_ttl(&login_fail_key(email), LOGIN_LOCKOUT_TTL)
        .await;
}

/// Reset `email`'s failed-login count after a successful login. Errors are
/// swallowed — clearing is a best-effort cleanup, not a correctness
/// requirement.
pub(super) async fn clear_login_failures(store: &dyn EphemeralStore, email: &str) {
    let _ = store.del(&login_fail_key(email)).await;
}

/// Count one password-reset request for `email` and report whether it is
/// still within the per-account limit. Best-effort: on a store error the
/// count defaults to 0 (allowed).
pub(super) async fn forgot_allowed(store: &dyn EphemeralStore, email: &str) -> bool {
    let count = store
        .incr_with_ttl(&forgot_rate_key(email), FORGOT_WINDOW_SECS)
        .await
        .unwrap_or(0);
    count <= FORGOT_MAX_REQUESTS
}
