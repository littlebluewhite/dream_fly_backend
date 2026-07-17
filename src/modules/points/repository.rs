use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{PointLedgerEntry, PointReason};

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
/// Refund/cancel compensation (Step 10d/10e) reverses these ledger sums
/// rather than reading `orders.points_earned`/`points_used`: a seed/
/// fixture-built order (or one predating the points ledger) can carry
/// non-zero values in those denormalized columns with no backing ledger
/// row, and reversing against the columns would claw back or restore
/// points that were never actually moved. Reading the ledger itself means
/// an order with no matching rows naturally sums to `(0, 0)` — no
/// legacy-data special case needed.
///
/// `checkout_redeem` rows are written with a *negative* `delta`
/// (`orders::service::checkout` calls `apply_delta_tx` with
/// `-outcome.points_used`) — this negates the summed `checkout_redeem`
/// delta before returning it, so both halves of the tuple come back
/// `>= 0`; the caller (`refund::plan_refund`, Step 10d) assigns the sign
/// itself. `COALESCE(..., 0)` covers the "no matching rows" case: an
/// unconditional `SUM(...) FILTER (...)` over zero rows is `NULL`, not `0`.
pub async fn find_order_flow_sums_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<(i64, i64), sqlx::Error> {
    let (earned, redeemed): (i64, i64) = sqlx::query_as(
        "SELECT \
            COALESCE(SUM(delta) FILTER (WHERE reason = 'checkout_earn'::point_reason), 0)::bigint AS earned, \
            COALESCE(-(SUM(delta) FILTER (WHERE reason = 'checkout_redeem'::point_reason)), 0)::bigint AS redeemed \
         FROM point_ledger \
         WHERE order_id = $1",
    )
    .bind(order_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok((earned, redeemed))
}

/// Insert the ledger row recording an applied delta, in the same
/// transaction as the balance update.
pub async fn insert_ledger_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    delta: i64,
    balance_after: i64,
    reason: PointReason,
    order_id: Option<Uuid>,
) -> Result<PointLedgerEntry, sqlx::Error> {
    sqlx::query_as::<_, PointLedgerEntry>(
        "INSERT INTO point_ledger (id, user_id, delta, balance_after, reason, order_id, created_at) \
         VALUES ($1, $2, $3, $4, $5::point_reason, $6, NOW()) \
         RETURNING id, user_id, delta, balance_after, reason, order_id, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(delta)
    .bind(balance_after)
    .bind(reason)
    .bind(order_id)
    .fetch_one(&mut **tx)
    .await
}
