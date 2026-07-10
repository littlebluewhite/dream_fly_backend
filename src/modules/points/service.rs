use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;

use super::dto::{LedgerEntryResponse, PointsMeResponse};
use super::model::PointReason;
use super::repository;

/// 原子調整點數並寫 ledger；餘額不足（結果 < 0）→ AppError::Conflict("insufficient points")。
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
        // "insufficient points". Any other check violation falls through
        // to the generic Database arm.
        Err(sqlx::Error::Database(ref db_err))
            if db_err.is_check_violation()
                && db_err.constraint() == Some("users_points_balance_check") =>
        {
            return Err(AppError::Conflict("insufficient points".into()));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

    repository::insert_ledger_tx(tx, user_id, delta, balance_after, reason, order_id).await?;

    Ok(balance_after)
}

/// Lock + read a user's points balance inside the caller's transaction.
/// `None` (no matching user) maps to `AppError::NotFound("user not
/// found")`, the same mapping `get_my_points`'s own (unlocked) balance read
/// below uses.
pub async fn lock_balance_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<i64, AppError> {
    repository::lock_balance_tx(tx, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))
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
/// `apply_delta_tx`'s own `Conflict("insufficient points")`, which fires
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

    let balance = lock_balance_tx(tx, user_id).await?;

    if balance < cost {
        return Err(AppError::Conflict("點數不足".into()));
    }

    apply_delta_tx(tx, user_id, -cost, reason, order_id).await
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
