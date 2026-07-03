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
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_check_violation() => {
            return Err(AppError::Conflict("insufficient points".into()));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

    repository::insert_ledger_tx(tx, user_id, delta, balance_after, reason, order_id).await?;

    Ok(balance_after)
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
        total,
        page: pagination.page,
        per_page: pagination.limit(),
    })
}
