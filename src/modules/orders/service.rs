use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::{PageMeta, PaginationParams};
use crate::kafka::events::{OrderCreatedPayload, OrderStatusChangedPayload, event_types, topics};
use crate::kafka::outbox;
use crate::modules::cart::model::{CartItemType, CheckoutLine};
use crate::modules::cart::repository as cart_repo;
use crate::modules::coupons::model::Coupon;
use crate::modules::coupons::repository as coupons_repo;
use crate::modules::enrolments::dto::EnrolmentResponse;
use crate::modules::enrolments::service as enrolments_service;
use crate::modules::notifications::service as notify;
use crate::modules::points::model::PointReason;
use crate::modules::points::service as points_service;
use crate::modules::products::repository as product_repo;
use crate::modules::subscriptions::dto::SubscriptionResponse;
use crate::modules::subscriptions::service as subscriptions_service;

use super::dto::{
    AdminOrderListResponse, AdminOrderSummary, CheckoutRequest, OrderListResponse, OrderResponse,
    OrderSummary,
};
use super::model::{Order, OrderStatus};
use super::pricing;
use super::repository;

/// Checkout the user's cart. When an `idempotency_key` is supplied, a second
/// attempt with the same (user_id, key) returns the original order instead
/// of creating a duplicate (double-click, network retry, mobile 502 retry).
///
/// Rule order: coupon load -> points-balance lock -> `pricing::price`
/// (subtotal -> coupon clamp -> points cap -> total -> points earned; see
/// that module for the arithmetic itself) -> stock decrement -> create
/// order (paid) -> order_items -> artifacts (enrolments, subscriptions,
/// points ledger) -> clear cart -> idempotency -> outbox + notify.
pub async fn checkout(
    db: &PgPool,
    user_id: Uuid,
    idempotency_key: Option<String>,
    req: CheckoutRequest,
) -> Result<OrderResponse, AppError> {
    // 1. Idempotency pre-check (outside tx). If we've already processed this
    //    key for this user, return the prior order (artifacts included).
    if let Some(key) = &idempotency_key {
        if let Some(existing_id) = repository::find_idempotency(db, user_id, key).await? {
            let order = repository::find_by_id(db, existing_id)
                .await?
                .ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!(
                        "idempotency row referenced missing order {existing_id}"
                    ))
                })?;
            return assemble_response(db, order).await;
        }
    }

    // All reads and writes happen inside the transaction so the cart snapshot,
    // product/course prices, stock decrement, and every artifact created are
    // consistent and serialized.
    let mut tx = db.begin().await?;

    // 2. Lock and read cart items + current product/course prices. Course
    //    lines are now first-class (the Task-3 "not yet supported" guard is
    //    gone).
    let cart_items = cart_repo::find_cart_items_for_checkout_tx(&mut tx, user_id).await?;

    if cart_items.is_empty() {
        return Err(AppError::BadRequest("cart is empty".into()));
    }

    // 3. Coupon (optional), loaded and validated here — an unknown/
    //    inactive/expired code is rejected outright — the caller should not
    //    be silently charged full price while believing a discount applied.
    //    Only the load happens in checkout; `pricing::price` turns this
    //    (already-valid) coupon into the actual discount once the cart's
    //    subtotal is known.
    let mut coupon: Option<Coupon> = None;
    if let Some(code) = req
        .coupon_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        coupon = Some(
            coupons_repo::find_valid_by_code_tx(&mut tx, code)
                .await?
                .ok_or_else(|| AppError::Validation("invalid coupon".into()))?,
        );
    }

    // 4. Points redemption (optional). Balance is read with `FOR UPDATE`
    //    inside this transaction so a second concurrent checkout by the
    //    same user cannot compute `points_used` against the same
    //    now-stale balance (double spend) — it blocks on this lock until
    //    we commit or roll back. `use_points=false` never takes this lock;
    //    `pricing::price` gets a 0 balance instead, which is exactly what
    //    "no redemption" needs.
    let use_points = req.use_points.unwrap_or(false);
    let points_balance = if use_points {
        repository::lock_user_points_balance_tx(&mut tx, user_id)
            .await?
            .ok_or_else(|| AppError::NotFound("user not found".into()))?
    } else {
        0
    };

    // 5. Price the cart — subtotal, coupon clamp, points cap, total, and
    //    points earned, all in one pure call now that the coupon is loaded
    //    and the points balance is locked. Accepted behavior note: subtotal
    //    overflow is now detected inside this call, after the coupon load
    //    above (it used to run first) — a cart whose subtotal overflows i64
    //    *and* carries an invalid coupon code now surfaces the coupon's 422
    //    instead of the overflow error. This needs an astronomical cart to
    //    reach; see `pricing::price` for the arithmetic itself.
    let outcome = pricing::price(&cart_items, coupon.as_ref(), points_balance, use_points)?;

    // 6. Stock decrement — product lines only; fail fast on shortage.
    //    Sorted by product_id (deterministic global lock order) before
    //    touching any row: two concurrent checkouts that share two products
    //    added to their carts in opposite order could otherwise acquire the
    //    per-row UPDATE locks in opposite orders and deadlock. The cart
    //    read's own order is per-user cart-insertion order, which is not
    //    globally consistent across different users' carts.
    let mut product_lines: Vec<&CheckoutLine> = cart_items
        .iter()
        .filter(|item| matches!(item.item_type, CartItemType::Product))
        .collect();
    product_lines.sort_by_key(|line| line.product_id);

    for item in &product_lines {
        let product_id = item
            .product_id
            .expect("product line always carries product_id");
        let result =
            product_repo::try_decrement_stock_tx(&mut tx, product_id, item.quantity).await?;
        if result.is_none() {
            return Err(AppError::Conflict(format!(
                "insufficient stock for product {}",
                item.name
            )));
        }
    }

    // 7. Generate an order number. UUID-v7 suffix (base36-encoded last 32
    //    bits) gives us an unambiguous, monotonic, unguessable unique
    //    component — no birthday collisions and no modulo bias.
    let order_number = {
        let suffix = Uuid::now_v7().as_u128() as u32;
        format!("DF-{}{:08X}", Utc::now().format("%Y%m%d"), suffix)
    };

    // 8. Create the order row FIRST, already `paid` — order_id is needed
    //     before enrolments/subscriptions/ledger rows can link to it.
    let order = repository::create_order(
        &mut tx,
        user_id,
        &order_number,
        outcome.total_cents,
        outcome.discount_cents,
        outcome.applied_coupon_code.as_deref(),
        outcome.points_used,
        outcome.points_earned,
    )
    .await?;

    // 9. order_items from the (locked) cart snapshot — both product and
    //     course lines. `ci.name` becomes the order_items snapshot column,
    //     so later reads (OrderSummary/AdminOrderSummary `items`) never need
    //     to join the live product/course catalog.
    let items_data: Vec<(Option<Uuid>, Option<Uuid>, i32, i64, String)> = cart_items
        .iter()
        .map(|ci| {
            (
                ci.product_id,
                ci.course_id,
                ci.quantity,
                ci.price_cents,
                ci.name.clone(),
            )
        })
        .collect();
    repository::create_order_items(&mut tx, order.id, &items_data).await?;

    // 10. Artifacts.
    // 10a. Enrolments — course lines, sorted by course_id (deterministic
    //      global lock order: `enrol_from_purchase_tx` takes `FOR UPDATE`
    //      on the course row, and two concurrent checkouts sharing two
    //      courses could otherwise lock them in opposite orders and
    //      deadlock — same rationale as the product-line sort above). A
    //      full course or a duplicate active enrolment rolls back the
    //      *entire* checkout (order, order_items, stock decrement — all of
    //      it), which is correct: partially fulfilling a cart is not an
    //      acceptable outcome.
    let mut course_lines: Vec<&CheckoutLine> = cart_items
        .iter()
        .filter(|item| matches!(item.item_type, CartItemType::Course))
        .collect();
    course_lines.sort_by_key(|line| line.course_id);

    for line in &course_lines {
        let course_id = line
            .course_id
            .expect("course line always carries course_id");
        enrolments_service::enrol_from_purchase_tx(&mut tx, user_id, course_id, order.id).await?;
    }

    // 10b. Subscriptions — product lines whose product_type is
    //      entitlement-eligible. `grant_from_purchase_tx` itself returns
    //      `Ok(None)` for non-eligible types, so every product line is
    //      simply offered to it. It does not itself validate quantity >= 1;
    //      cart quantity is enforced to 1..=999 at add-time, so that always
    //      holds by the time we get here.
    for item in &product_lines {
        let product_id = item
            .product_id
            .expect("product line always carries product_id");
        let product = product_repo::find_by_id_tx(&mut tx, product_id)
            .await?
            .ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "product {product_id} vanished mid-checkout after stock decrement"
                ))
            })?;
        subscriptions_service::grant_from_purchase_tx(
            &mut tx,
            user_id,
            &product,
            item.quantity,
            item.price_cents,
            order.id,
        )
        .await?;
    }

    // 10c. Points ledger — redeem (negative) then earn (positive), each
    //      skipped when zero (`apply_delta_tx` rejects a zero delta).
    if outcome.points_used > 0 {
        points_service::apply_delta_tx(
            &mut tx,
            user_id,
            -outcome.points_used,
            PointReason::CheckoutRedeem,
            Some(order.id),
        )
        .await?;
    }
    if outcome.points_earned > 0 {
        points_service::apply_delta_tx(
            &mut tx,
            user_id,
            outcome.points_earned,
            PointReason::CheckoutEarn,
            Some(order.id),
        )
        .await?;
    }

    // 11. Clear the cart within the same transaction.
    cart_repo::clear_cart_tx(&mut tx, user_id).await?;

    // 12. Record the idempotency key inside the same tx so a concurrent
    //     retry sees either nothing (and races for the lock) or the
    //     committed row.
    if let Some(key) = &idempotency_key {
        match repository::insert_idempotency_tx(&mut tx, user_id, key, order.id).await {
            Ok(()) => {}
            Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
                // Concurrent retry beat us. Roll back and let the caller
                // re-read the winning row.
                drop(tx);
                if let Some(existing_id) = repository::find_idempotency(db, user_id, key).await? {
                    let existing_order = repository::find_by_id(db, existing_id)
                        .await?
                        .ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "idempotency row referenced missing order {existing_id}"
                            ))
                        })?;
                    return assemble_response(db, existing_order).await;
                }
                return Err(AppError::Conflict("duplicate checkout".into()));
            }
            Err(e) => return Err(AppError::Database(e)),
        }
    }

    // 13. Queue the order_created event into the outbox — persisted
    //     atomically with the order itself. The background dispatcher (see
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
            discount_cents: order.discount_cents,
            coupon_code: order.coupon_code.clone(),
            points_used: order.points_used,
            points_earned: order.points_earned,
        },
        None,
    )
    .await?;

    tx.commit().await?;

    // 14. Inline notification — the user expects order confirmation
    //     regardless of whether Kafka is enabled, and even if the
    //     dispatcher hasn't drained the event yet.
    notify::order_placed(db, order.user_id, order.id, &order.order_number).await;

    // 15. Assemble the response (items + artifacts, looked up by order_id).
    assemble_response(db, order).await
}

/// Fetch the enrolments/subscriptions a given order produced, mapped to
/// their response DTOs. Shared by `assemble_response` (checkout, replay,
/// `get_order`) so every read path presents identical artifacts.
async fn fetch_artifacts(
    db: &PgPool,
    order_id: Uuid,
) -> Result<(Vec<EnrolmentResponse>, Vec<SubscriptionResponse>), AppError> {
    let enrolments = repository::find_enrolments_by_order(db, order_id)
        .await?
        .into_iter()
        .map(EnrolmentResponse::from)
        .collect();
    let subscriptions = repository::find_subscriptions_by_order(db, order_id)
        .await?
        .into_iter()
        .map(SubscriptionResponse::from)
        .collect();
    Ok((enrolments, subscriptions))
}

/// Build the full `OrderResponse` for an already-fetched order row: its
/// items plus its artifacts, both looked up by `order.id`.
async fn assemble_response(db: &PgPool, order: Order) -> Result<OrderResponse, AppError> {
    let items = repository::find_items_by_order(db, order.id).await?;
    let (enrolments, subscriptions) = fetch_artifacts(db, order.id).await?;
    Ok(OrderResponse::assemble(order, items, enrolments, subscriptions))
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
        return Err(AppError::Forbidden(
            "not authorized to view this order".into(),
        ));
    }

    assemble_response(db, order).await
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
        meta: PageMeta {
            total,
            page: page.max(1),
            per_page: limit,
        },
    })
}

/// Paginated order list for admins, newest first — `AdminOrderSummary`
/// carries the buyer's name/email (JOINed) alongside the order.
pub async fn list_all_orders(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<AdminOrderListResponse, AppError> {
    let total = repository::count_all(db).await?;
    let rows = repository::find_all_with_user(db, pagination.limit(), pagination.offset()).await?;

    Ok(AdminOrderListResponse {
        orders: rows.into_iter().map(AdminOrderSummary::from).collect(),
        meta: pagination.meta(total),
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
    notify::order_status_changed(
        db,
        updated.user_id,
        updated.id,
        &updated.order_number,
        target.as_str(),
    )
    .await;

    assemble_response(db, updated).await
}
