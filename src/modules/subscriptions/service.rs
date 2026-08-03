use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::modules::products::model::Product;

use super::dto::SubscriptionResponse;
use super::entitlement;
use super::model::Subscription;
use super::repository;

/// 依商品 entitlement 設定產生訂閱；非 entitlement 商品（product_type 非
/// membership/ticket）回 Ok(None)。
///
/// The branch rules themselves (session-count / time-based / unlimited /
/// non-entitlement) live in `entitlement::plan` as a pure function — see
/// that module's doc for the four cases. This function is just: call it,
/// then write the one `repository::insert_tx` row.
///
/// `price_cents` is the unit price paid and is stored as given. `now` is
/// the caller-sampled clock (checkout's own `now`), threaded through so
/// `entitlement::plan` never reads the wall clock itself.
pub async fn grant_from_purchase_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    product: &Product,
    quantity: i32,
    price_cents: i64,
    order_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<Subscription>, AppError> {
    let Some(grant) = entitlement::plan(product, quantity, now)? else {
        return Ok(None);
    };

    let subscription = repository::insert_tx(
        tx,
        user_id,
        product.id,
        order_id,
        grant.expires_at,
        grant.total_sessions,
        grant.remaining_sessions,
        price_cents,
    )
    .await
    .map_err(AppError::Database)?;

    Ok(Some(subscription))
}

/// This user's subscriptions, newest first.
pub async fn list_my_subscriptions(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<SubscriptionResponse>, AppError> {
    let rows = repository::find_by_user(db, user_id)
        .await
        .map_err(AppError::Database)?;
    Ok(rows.into_iter().map(SubscriptionResponse::from).collect())
}

/// This order's subscriptions, mapped to their response DTOs. The DTO
/// wrapping lives in this owning module (not in `orders::service`) per
/// ADR-0005's "DTO 包裝在 service.rs" placement rule;
/// `orders::service::fetch_artifacts` calls this seam so `orders` never
/// reaches into this module's repository.
pub async fn list_by_order(
    db: &PgPool,
    order_id: Uuid,
) -> Result<Vec<SubscriptionResponse>, AppError> {
    let rows = repository::find_by_order(db, order_id)
        .await
        .map_err(AppError::Database)?;
    Ok(rows.into_iter().map(SubscriptionResponse::from).collect())
}

/// Passthrough to `repository::cancel_by_order_tx` — the ADR-0005 seam
/// `orders::service`'s refund/cancel compensation (Step 10e) calls, so
/// `orders` never imports this module's repository directly.
pub async fn cancel_by_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<u64, AppError> {
    repository::cancel_by_order_tx(tx, order_id)
        .await
        .map_err(AppError::Database)
}

/// Redeem (consume) one session from a subscription. The decrement is a
/// single atomic `UPDATE ... RETURNING`, and the response is built from the
/// exact row that UPDATE returned — re-reading the subscription here could
/// observe a *concurrent* redeem's later decrement and misreport what this
/// call consumed. Only `product_name` is fetched separately. On 0 rows we
/// re-read the row to tell a missing id (404) apart from the specific
/// reason it isn't redeemable (409).
pub async fn redeem(db: &PgPool, id: Uuid) -> Result<SubscriptionResponse, AppError> {
    if let Some(sub) = repository::redeem_one_session(db, id)
        .await
        .map_err(AppError::Database)?
    {
        let product_name = repository::product_name(db, sub.product_id)
            .await
            .map_err(AppError::Database)?;
        return Ok(SubscriptionResponse::from_subscription(sub, product_name));
    }

    let current = repository::find_by_id(db, id)
        .await
        .map_err(AppError::Database)?
        .ok_or_else(|| AppError::NotFound("subscription not found".into()))?;

    if current.remaining_sessions.is_none() {
        return Err(AppError::Conflict(
            "subscription has no session quota".into(),
        ));
    }

    Err(AppError::Conflict("subscription is not redeemable".into()))
}
