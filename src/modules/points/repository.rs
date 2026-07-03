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
