use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::utils::studio_clock::StudioNow;

use super::model::{OrderPointsFlow, PointLedgerEntry, PointReason};

/// Current points balance for a user (NOT NULL column on `users`). `None`
/// means no such user.
pub async fn find_balance(db: &PgPool, user_id: Uuid) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT points_balance FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await
}

/// Lock the user's row and read their current points balance inside the
/// caller's transaction, so a second concurrent spend for the same user can
/// never compute against the same stale balance (double spend). The lock is
/// held until the caller's transaction commits or rolls back — a concurrent
/// spend for the same user blocks here until then, and re-reads the
/// now-updated balance afterward. Moved here from
/// `orders::repository::lock_user_points_balance_tx` (Task 4, C2) — the
/// same lock is now shared by `orders::service::checkout` (lock-only) and
/// `rewards::service::redeem` (lock-then-spend via `try_spend_tx`).
pub async fn lock_balance_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT points_balance FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await
}

/// This user's ledger entries, newest first, paginated.
pub async fn find_ledger_by_user(
    db: &PgPool,
    user_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<PointLedgerEntry>, sqlx::Error> {
    sqlx::query_as::<_, PointLedgerEntry>(
        "SELECT id, user_id, delta, balance_after, reason, order_id, created_at \
         FROM point_ledger \
         WHERE user_id = $1 \
         ORDER BY created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(user_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_ledger_by_user(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM point_ledger WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// Sum of the user's `checkout_earn` deltas in the studio month containing
/// `at.now`. Clawbacks and every other reason are deliberately not netted.
pub async fn sum_earned_in_studio_month(
    db: &PgPool,
    user_id: Uuid,
    at: StudioNow,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM(delta), 0)::BIGINT FROM point_ledger \
         WHERE user_id = $1 AND reason = 'checkout_earn' \
           AND date_trunc('month', created_at AT TIME ZONE $3) = studio_month_anchor($2, $3)",
    )
    .bind(user_id)
    .bind(at.now)
    .bind(at.tz.name())
    .fetch_one(db)
    .await
}

/// Atomically adjust a user's points balance inside the caller's
/// transaction. Returns `None` if no user matched `user_id`. If the new
/// balance would go negative, the `users_points_balance_check` CHECK
/// constraint rejects the UPDATE with a check-violation database error —
/// the caller (`service::apply_delta_tx`) catches that and maps it to
/// `AppError::Conflict`.
pub async fn adjust_balance_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    delta: i64,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "UPDATE users SET points_balance = points_balance + $2 WHERE id = $1 \
         RETURNING points_balance",
    )
    .bind(user_id)
    .bind(delta)
    .fetch_optional(&mut **tx)
    .await
}

/// Sum of one order's `checkout_earn`/`checkout_redeem` `point_ledger`
/// rows, returned as two non-negative magnitudes — `(earned, redeemed)`.
/// Refund/cancel compensation (ADR-0007 決策 8) reverses these ledger sums
/// rather than reading `orders.points_earned`/`points_used`: a fixture/
/// directly-built order, an order from a seed run before this change, or
/// one predating the points ledger can carry
/// non-zero values in those denormalized columns with no backing ledger
/// row, and reversing against the columns would claw back or restore
/// points that were never actually moved. Reading the ledger itself means
/// an order with no matching rows naturally sums to `(0, 0)` — no
/// legacy-data special case needed.
///
/// `checkout_redeem` rows are written with a *negative* `delta`
/// (`orders::service::checkout` applies the deltas from
/// `PricingOutcome::ledger_deltas`, whose redeem delta is
/// `-points_used`) — this negates the summed `checkout_redeem`
/// delta before returning it, so both fields of the [`OrderPointsFlow`]
/// come back `>= 0`; `OrderPointsFlow::reversal_deltas` assigns the sign
/// itself. `COALESCE(..., 0)` covers the "no matching rows" case: an
/// unconditional `SUM(...) FILTER (...)` over zero rows is `NULL`, not `0`.
pub async fn find_order_flow_sums_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<OrderPointsFlow, sqlx::Error> {
    sqlx::query_as::<_, OrderPointsFlow>(
        "SELECT \
            COALESCE(SUM(delta) FILTER (WHERE reason = 'checkout_earn'::point_reason), 0)::bigint AS earned, \
            COALESCE(-(SUM(delta) FILTER (WHERE reason = 'checkout_redeem'::point_reason)), 0)::bigint AS redeemed \
         FROM point_ledger \
         WHERE order_id = $1",
    )
    .bind(order_id)
    .fetch_one(&mut **tx)
    .await
}

/// Insert the ledger row recording an applied delta, in the same
/// transaction as the balance update. `created_at` 是業務時間(`earned_this_month`
/// 依它分桶),綁 handler 取樣的 `now`(ADR-0017)。
pub async fn insert_ledger_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    delta: i64,
    balance_after: i64,
    reason: PointReason,
    order_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<PointLedgerEntry, sqlx::Error> {
    sqlx::query_as::<_, PointLedgerEntry>(
        "INSERT INTO point_ledger (id, user_id, delta, balance_after, reason, order_id, created_at) \
         VALUES ($1, $2, $3, $4, $5::point_reason, $6, $7) \
         RETURNING id, user_id, delta, balance_after, reason, order_id, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(delta)
    .bind(balance_after)
    .bind(reason)
    .bind(order_id)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
}
