use chrono::{Duration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::modules::products::model::{Product, ProductType};

use super::dto::SubscriptionResponse;
use super::model::Subscription;
use super::repository;

/// 依商品 entitlement 設定產生訂閱；非 entitlement 商品（product_type 非
/// membership/ticket）回 Ok(None)。
///
/// Rules:
/// - `product.product_type` not in {membership, ticket} → `Ok(None)`.
/// - `session_count` set → one row, `total_sessions = remaining_sessions =
///   session_count * quantity`. If `valid_days` is *also* set, `expires_at`
///   is populated too (both constraints apply — sessions still drive the
///   quota).
/// - else `valid_days` set → `expires_at = now + valid_days`, no session
///   quota; `quantity` must be 1 (a time-based grant can't be multiplied
///   into one row), otherwise `AppError::Validation`.
/// - neither set → unlimited membership record (no expiry, no quota).
///
/// `price_cents` is the unit price paid and is stored as given.
pub async fn grant_from_purchase_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    product: &Product,
    quantity: i32,
    price_cents: i64,
    order_id: Uuid,
) -> Result<Option<Subscription>, AppError> {
    if !matches!(
        product.product_type,
        ProductType::Membership | ProductType::Ticket
    ) {
        return Ok(None);
    }

    let (total_sessions, remaining_sessions, expires_at) =
        if let Some(session_count) = product.session_count {
            let total = session_count * quantity;
            let expires_at = product
                .valid_days
                .map(|days| Utc::now() + Duration::days(days as i64));
            (Some(total), Some(total), expires_at)
        } else if let Some(valid_days) = product.valid_days {
            if quantity != 1 {
                return Err(AppError::Validation(
                    "time-based subscription quantity must be 1".into(),
                ));
            }
            (None, None, Some(Utc::now() + Duration::days(valid_days as i64)))
        } else {
            (None, None, None)
        };

    let subscription = repository::insert_tx(
        tx,
        user_id,
        product.id,
        order_id,
        expires_at,
        total_sessions,
        remaining_sessions,
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
