use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;

use super::dto::{
    AdjustPointsRequest, LedgerEntryResponse, PointsAdjustmentResponse, PointsMeResponse,
};
use super::model::PointReason;
use super::repository;

/// 原子調整點數並寫 ledger；餘額不足（結果 < 0）→ AppError::Conflict("點數不足")。
///
/// `delta == 0` is rejected up front (a zero-delta ledger row would be
/// pure noise) without touching the database. Otherwise the balance update
/// and the ledger insert happen in the caller's transaction: on any error
/// returned here, the caller is responsible for rolling back (or simply
/// not committing) `tx` — this function never commits.
pub async fn apply_delta_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    delta: i64,
    reason: PointReason,
    order_id: Option<Uuid>,
) -> Result<i64, AppError> {
    if delta == 0 {
        return Err(AppError::Validation("delta must be non-zero".into()));
    }

    let balance_after = match repository::adjust_balance_tx(tx, user_id, delta).await {
        Ok(Some(balance)) => balance,
        Ok(None) => return Err(AppError::NotFound("user not found".into())),
        // Scoped to the balance constraint by name: `users` carries other
        // CHECK constraints (`users_has_auth_method` today, possibly more
        // later), and only a `users_points_balance_check` violation means
        // "點數不足". Any other check violation falls through
        // to the generic Database arm.
        Err(sqlx::Error::Database(ref db_err))
            if db_err.is_check_violation()
                && db_err.constraint() == Some("users_points_balance_check") =>
        {
            return Err(AppError::Conflict("點數不足".into()));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

    repository::insert_ledger_tx(tx, user_id, delta, balance_after, reason, order_id).await?;

    Ok(balance_after)
}

/// Witness that `lock_balance_tx` has taken this user's `users`-row `FOR
/// UPDATE` lock inside the caller's still-open transaction — the user-first
/// half of the checkout/refund lock order (ADR-0007 決策 5) collapses from a
/// doc-comment-only invariant into the type system: `cart::service`'s
/// `find_cart_items_for_checkout_tx` now takes `&BalanceLock` instead of a
/// bare `user_id`, so the cart it reads can never be paired with a
/// different user's lock.
///
/// Fields are private; only `lock_balance_tx` can construct one. Read-only
/// access via `user_id()`/`balance()` — `user_id` is the caller's own
/// input, `balance` is read under the very `FOR UPDATE` this witness
/// attests to, so holding one backs both "this user's row is locked" and
/// "this is that user's balance at lock time" (the same pairing guarantee
/// `courses::seats`'s `SessionLock` gives `session_id`/`course_id`).
///
/// Lives flat in this module rather than behind a private `mod tx_witness`
/// wrapper (contrast `orders::service`'s `TxReleased`): that extra layer
/// guards against the *same file*'s other functions hand-building a fake
/// witness to bypass the constructor. Here the governed caller
/// (`cart::service`) sits in a different module entirely and simply has no
/// access to these private fields regardless — plain field privacy is
/// already the whole defense, exactly as with `SessionLock`.
///
/// **Why unconditional and first** — the argument `orders::service::checkout`
/// and its `compensate_order_artifacts_tx` refund counterpart both point
/// here for: each locks this row as literally its first statement, even
/// when the caller doesn't need the balance value itself (`use_points=false`,
/// a zero-flow refund):
///   checkout: users -> cart_items/products/courses (SHARE) -> products
///             (UPDATE, product_id asc) -> courses (asc) -> enrolments ->
///             subscriptions
///   refund:   orders -> users -> products (UPDATE, asc) -> enrolments ->
///             subscriptions
/// Taking `users` first on both paths is what makes it *the* unconditional
/// first lock. If either path deferred it, that path could end up holding a
/// downstream lock (e.g. checkout's cart-read `FOR SHARE` on products)
/// while waiting on `users`, while the other path simultaneously holds
/// `users` while waiting on that same downstream lock — a deadlock cycle.
/// Cross-buyer dimension, the pre-existing SHARE→UPDATE risk, and the full
/// regression list live in ADR-0007 決策 5 and its Addendum recording this
/// witness migration.
///
/// Deliberately not `#[must_use]`: the witness is permission to reach the
/// governed seam, not an obligation to consume the balance —
/// `compensate_order_artifacts_tx` locks purely to hold the row lock for
/// the refund's duration and legitimately drops the returned witness
/// unused. Dropping it does NOT release the row lock: the lock lives on the
/// still-open `tx` until the caller commits or rolls back.
#[derive(Debug)]
pub struct BalanceLock {
    user_id: Uuid,
    balance: i64,
}

impl BalanceLock {
    pub fn user_id(&self) -> Uuid {
        self.user_id
    }

    pub fn balance(&self) -> i64 {
        self.balance
    }
}

/// Lock + read a user's points balance inside the caller's transaction,
/// returning a [`BalanceLock`] witness rather than the bare balance. `None`
/// (no matching user) maps to `AppError::NotFound("user not found")`, the
/// same mapping `get_my_points`'s own (unlocked) balance read below uses —
/// unchanged by the witness wrapping.
pub async fn lock_balance_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<BalanceLock, AppError> {
    let balance = repository::lock_balance_tx(tx, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    Ok(BalanceLock { user_id, balance })
}

/// Lock the balance, compare against `cost`, and spend it atomically: lock
/// → compare → `apply_delta_tx`, all under the one row lock `lock_balance_tx`
/// takes. Returns the resulting balance (`balance_after`).
///
/// `cost <= 0` is rejected up front, before taking the lock —
/// `AppError::Validation`. This guards a sign-flip footgun (a negative cost
/// would silently *credit* the account instead of spending from it), not a
/// speculative check: `apply_delta_tx` would itself reject the resulting
/// zero delta when `cost == 0`, but that message talks about "delta", which
/// would be a confusing error to surface from a "spend" call.
///
/// Insufficient balance → `AppError::Conflict("點數不足")` — this exact
/// Chinese text is pinned byte-for-byte by `tests/service_rewards.rs:107`.
/// `apply_delta_tx`'s own `Conflict("點數不足")`, which fires
/// from the `users_points_balance_check` CHECK-constraint violation, is a
/// DB-level backstop for callers that adjust `users.points_balance` without
/// pre-checking (e.g. admin adjustments via `apply_delta_tx` directly);
/// under `try_spend_tx`'s lock-then-compare protocol that path is
/// unreachable — the comparison below already rejects any spend that would
/// drive the balance negative before `apply_delta_tx`'s UPDATE ever runs.
pub async fn try_spend_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    cost: i64,
    reason: PointReason,
    order_id: Option<Uuid>,
) -> Result<i64, AppError> {
    if cost <= 0 {
        return Err(AppError::Validation("cost must be positive".into()));
    }

    let balance = lock_balance_tx(tx, user_id).await?.balance();

    if balance < cost {
        return Err(AppError::Conflict("點數不足".into()));
    }

    apply_delta_tx(tx, user_id, -cost, reason, order_id).await
}

/// Passthrough to `repository::find_order_flow_sums_tx` — the ADR-0005 seam
/// `orders::service`'s refund/cancel compensation (Step 10e) reads through,
/// so `orders` never imports this module's repository directly.
pub async fn find_order_flow_sums_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<(i64, i64), AppError> {
    repository::find_order_flow_sums_tx(tx, order_id)
        .await
        .map_err(AppError::Database)
}

/// Current balance + paginated ledger (newest first) for `/points/me`.
pub async fn get_my_points(
    db: &PgPool,
    user_id: Uuid,
    pagination: &PaginationParams,
) -> Result<PointsMeResponse, AppError> {
    let balance = repository::find_balance(db, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let entries =
        repository::find_ledger_by_user(db, user_id, pagination.limit(), pagination.offset())
            .await?;
    let total = repository::count_ledger_by_user(db, user_id).await?;

    Ok(PointsMeResponse {
        balance,
        ledger: entries.into_iter().map(LedgerEntryResponse::from).collect(),
        meta: pagination.meta(total),
    })
}

/// `POST /points/adjustments` (admin-only, Step 10f) — the minimal repair
/// tool that closes the loop on refund/cancel compensation's clawback step
/// 409ing with `Conflict("點數不足")` (`orders::service::compensate_order_artifacts_tx`,
/// 10e): an admin restores the member's balance here, then retries the
/// refund PATCH. Consumes `PointReason::AdminAdjust`, previously a
/// zero-call-site variant.
///
/// Owns its own transaction (pool-in, unlike `apply_delta_tx`/
/// `lock_balance_tx`, which compose into a caller's transaction) — this is
/// a leaf operation, not a step inside a larger orchestrated write.
///
/// **CAS, not idempotency** (ADR-0007, Step 10h, has the full write-up —
/// this doc comment is the summary it's referenced from). Comparing the
/// locked balance against `req.expected_balance` before writing guards
/// against *re-applying the same adjustment twice*; it is not a
/// replay-safe idempotency key. A client that times out waiting for the
/// first call's response and retries with the same body will find the
/// balance it just changed, so the retry gets `Conflict` instead of
/// replaying the original success — the retry is *rejected*, not
/// replayed. That's an accepted tradeoff here: this endpoint is
/// admin-driven and low-frequency, and every application is independently
/// auditable via its `AdminAdjust` ledger row, so an admin who hits 409 on
/// retry re-checks the target user's current `points_balance` (via
/// `GET /users/{id}` — `GET /points/me` is self-only, bound to the
/// caller's own `auth.user_id`, so it can't look up someone else's
/// balance) and decides from there whether the adjustment already landed,
/// rather than blindly retrying; confirming the exact `AdminAdjust` ledger
/// row still means a direct `point_ledger` query, since no admin-facing
/// per-user ledger endpoint exists yet. The narrower residual risk is
/// ABA: a third party could
/// restore the balance to exactly `expected_balance` again inside the
/// retry window, letting a stale retry through unnoticed — accepted as
/// residual risk for a manual, low-frequency admin tool. If this endpoint
/// ever gains an automated (non-human) caller, upgrade to a request-id
/// dedup key or a partial unique index at that point; not added now.
pub async fn adjust_points(
    db: &PgPool,
    req: &AdjustPointsRequest,
) -> Result<PointsAdjustmentResponse, AppError> {
    let mut tx = db.begin().await?;

    let current_balance = lock_balance_tx(&mut tx, req.user_id).await?.balance();
    if current_balance != req.expected_balance {
        return Err(AppError::Conflict(format!(
            "balance mismatch: expected {}, actual {}",
            req.expected_balance, current_balance
        )));
    }

    let balance = apply_delta_tx(
        &mut tx,
        req.user_id,
        req.delta,
        PointReason::AdminAdjust,
        None,
    )
    .await?;

    tx.commit().await?;

    Ok(PointsAdjustmentResponse {
        user_id: req.user_id,
        balance,
    })
}
