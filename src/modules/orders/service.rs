use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::kafka::events::{
    event_types, topics, OrderCreatedPayload, OrderStatusChangedPayload,
};
use crate::kafka::outbox;
use crate::modules::cart::repository as cart_repo;
use crate::modules::notifications::model::NotificationType;
use crate::modules::notifications::repository as notif_repo;
use crate::modules::products::repository as product_repo;

use super::dto::{OrderListResponse, OrderResponse, OrderSummary};
use super::model::OrderStatus;
use super::repository;

/// Checkout the user's cart. When an `idempotency_key` is supplied, a second
/// attempt with the same (user_id, key) returns the original order instead
/// of creating a duplicate (double-click, network retry, mobile 502 retry).
pub async fn checkout(
    db: &PgPool,
    user_id: Uuid,
    idempotency_key: Option<String>,
) -> Result<OrderResponse, AppError> {
    // 1. Idempotency pre-check (outside tx). If we've already processed this
    //    key for this user, return the prior order.
    if let Some(key) = &idempotency_key {
        if let Some(existing_id) =
            repository::find_idempotency(db, user_id, key).await?
        {
            let order = repository::find_by_id(db, existing_id)
                .await?
                .ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!(
                        "idempotency row referenced missing order {existing_id}"
                    ))
                })?;
            let items = repository::find_items_by_order(db, existing_id).await?;
            return Ok(OrderResponse::from_order_and_items(order, items));
        }
    }

    // All reads and writes happen inside the transaction so the cart snapshot,
    // product prices, and stock decrement are consistent and serialized.
    let mut tx = db.begin().await?;

    // 2. Lock and read cart items + current product prices
    let cart_items = cart_repo::find_cart_items_for_checkout_tx(&mut tx, user_id).await?;

    if cart_items.is_empty() {
        return Err(AppError::BadRequest("cart is empty".into()));
    }

    // 3. Decrement product stock for items that track stock; fail fast on shortage
    for item in &cart_items {
        let result = product_repo::try_decrement_stock_tx(&mut tx, item.product_id, item.quantity)
            .await?;
        if result.is_none() {
            return Err(AppError::Conflict(format!(
                "insufficient stock for product {}",
                item.product_name
            )));
        }
    }

    // 4. Compute total with overflow-checked arithmetic
    let mut total_cents: i64 = 0;
    for item in &cart_items {
        let line = item
            .price_cents
            .checked_mul(item.quantity as i64)
            .ok_or_else(|| AppError::Validation("order total overflow".into()))?;
        total_cents = total_cents
            .checked_add(line)
            .ok_or_else(|| AppError::Validation("order total overflow".into()))?;
    }

    // 5. Generate an order number. UUID-v7 suffix (base36-encoded last 32
    //    bits) gives us an unambiguous, monotonic, unguessable unique
    //    component — no birthday collisions and no modulo bias.
    let order_number = {
        let suffix = Uuid::now_v7().as_u128() as u32;
        format!(
            "DF-{}{:08X}",
            Utc::now().format("%Y%m%d"),
            suffix
        )
    };

    // 6. Create the order
    let order = repository::create_order(&mut tx, user_id, &order_number, total_cents).await?;

    // 7. Create order items from the (locked) cart snapshot
    let items_data: Vec<(Uuid, i32, i64)> = cart_items
        .iter()
        .map(|ci| (ci.product_id, ci.quantity, ci.price_cents))
        .collect();

    let order_items = repository::create_order_items(&mut tx, order.id, &items_data).await?;

    // 8. Clear the cart within the same transaction
    cart_repo::clear_cart_tx(&mut tx, user_id).await?;

    // 9. Record the idempotency key inside the same tx so a concurrent retry
    //    sees either nothing (and races for the lock) or the committed row.
    if let Some(key) = &idempotency_key {
        match repository::insert_idempotency_tx(&mut tx, user_id, key, order.id).await {
            Ok(()) => {}
            Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
                // Concurrent retry beat us. Roll back and let the caller
                // re-read the winning row.
                drop(tx);
                if let Some(existing_id) =
                    repository::find_idempotency(db, user_id, key).await?
                {
                    let order = repository::find_by_id(db, existing_id)
                        .await?
                        .ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "idempotency row referenced missing order {existing_id}"
                            ))
                        })?;
                    let items = repository::find_items_by_order(db, existing_id).await?;
                    return Ok(OrderResponse::from_order_and_items(order, items));
                }
                return Err(AppError::Conflict("duplicate checkout".into()));
            }
            Err(e) => return Err(AppError::Database(e)),
        }
    }

    // 10. Queue the order_created event into the outbox — persisted atomically
    //     with the order itself. The background dispatcher (see
    //     `kafka::outbox::start_dispatcher`) publishes it to Kafka with
    //     at-least-once semantics.
    outbox::insert_event_tx(
        &mut tx,
        topics::ORDERS_CREATED,
        event_types::ORDER_CREATED,
        &order.id.to_string(),
        OrderCreatedPayload {
            order_id: order.id,
            user_id: order.user_id,
            order_number: order.order_number.clone(),
            total_cents: order.total_cents,
        },
        None,
    )
    .await?;

    tx.commit().await?;

    // 11. Inline notification — the user expects order confirmation
    //     regardless of whether Kafka is enabled, and even if the
    //     dispatcher hasn't drained the event yet.
    if let Err(e) = notif_repo::create_notification(
        db,
        order.user_id,
        &NotificationType::OrderPlaced,
        "Order Placed",
        &format!("Your order {} has been placed.", order.order_number),
        Some(serde_json::json!({"order_id": order.id, "order_number": order.order_number})),
    )
    .await
    {
        tracing::error!(error = ?e, "failed to write order_placed notification");
    }

    Ok(OrderResponse::from_order_and_items(order, order_items))
}

pub async fn get_order(
    db: &PgPool,
    order_id: Uuid,
    user_id: Uuid,
    is_admin: bool,
) -> Result<OrderResponse, AppError> {
    let order = repository::find_by_id(db, order_id)
        .await?
        .ok_or_else(|| AppError::NotFound("order not found".into()))?;

    // Check ownership or admin
    if order.user_id != user_id && !is_admin {
        return Err(AppError::Forbidden("not authorized to view this order".into()));
    }

    let items = repository::find_items_by_order(db, order_id).await?;

    Ok(OrderResponse::from_order_and_items(order, items))
}

pub async fn my_orders(
    db: &PgPool,
    user_id: Uuid,
    page: u32,
    per_page: u32,
) -> Result<OrderListResponse, AppError> {
    let limit = per_page.clamp(1, 100);
    let offset = (page.max(1).saturating_sub(1)) * limit;

    let orders = repository::find_by_user(db, user_id, limit, offset).await?;
    let total = repository::count_by_user(db, user_id).await?;

    let summaries: Vec<OrderSummary> = orders.into_iter().map(OrderSummary::from).collect();

    Ok(OrderListResponse {
        orders: summaries,
        total,
        page: page.max(1),
        per_page: limit,
    })
}

pub async fn update_order_status(
    db: &PgPool,
    order_id: Uuid,
    status_str: &str,
) -> Result<OrderResponse, AppError> {
    let target: OrderStatus = status_str
        .parse()
        .map_err(|_| AppError::Validation(format!("invalid order status: {status_str}")))?;

    // Everything in a single tx: read current status → check transition →
    // single atomic UPDATE with conditional `paid_at`. No more split
    // UPDATE+UPDATE+SELECT that could leave `status='paid' AND paid_at=NULL`.
    let mut tx = db.begin().await?;

    let current = repository::find_by_id_tx(&mut tx, order_id)
        .await?
        .ok_or_else(|| AppError::NotFound("order not found".into()))?;

    if !current.status.can_transition_to(&target) {
        return Err(AppError::BadRequest(format!(
            "cannot transition order from '{}' to '{}'",
            current.status.as_str(),
            target.as_str()
        )));
    }

    let updated = repository::update_status_and_paid_at_tx(&mut tx, order_id, &target)
        .await?
        .ok_or_else(|| AppError::NotFound("order not found".into()))?;

    let items = repository::find_items_by_order_tx(&mut tx, order_id).await?;

    // Queue the status-change event atomically with the status update.
    outbox::insert_event_tx(
        &mut tx,
        topics::ORDERS_STATUS_CHANGED,
        event_types::ORDER_STATUS_CHANGED,
        &updated.id.to_string(),
        OrderStatusChangedPayload {
            order_id: updated.id,
            user_id: updated.user_id,
            status: target.as_str().to_string(),
        },
        None,
    )
    .await?;

    tx.commit().await?;

    // Inline notification — every status change is user-visible and
    // shouldn't wait for the outbox dispatcher tick.
    if let Err(e) = notif_repo::create_notification(
        db,
        updated.user_id,
        &NotificationType::OrderStatus,
        "Order Update",
        &format!("Your order status has been updated to: {}", target.as_str()),
        Some(serde_json::json!({
            "order_id": updated.id,
            "order_number": updated.order_number,
            "status": target.as_str(),
        })),
    )
    .await
    {
        tracing::error!(error = ?e, "failed to write order_status notification");
    }

    Ok(OrderResponse::from_order_and_items(updated, items))
}
