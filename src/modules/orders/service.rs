use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::{PageMeta, PaginationParams};
use crate::kafka::events::{OrderCreatedPayload, OrderStatusChangedPayload};
use crate::kafka::outbox;
use crate::modules::cart::service as cart_service;
use crate::modules::coupons::model::Coupon;
use crate::modules::coupons::service as coupons_service;
use crate::modules::enrolments::dto::EnrolmentResponse;
use crate::modules::enrolments::service as enrolments_service;
use crate::modules::notifications::service as notify;
use crate::modules::points::model::PointReason;
use crate::modules::points::service as points_service;
use crate::modules::products::service as product_service;
use crate::modules::subscriptions::dto::SubscriptionResponse;
use crate::modules::subscriptions::service as subscriptions_service;
use crate::utils::studio_clock;

use super::dto::{
    AdminOrderListResponse, AdminOrderSummary, CheckoutRequest, OrderListResponse, OrderResponse,
    OrderSummary,
};
use super::fulfilment;
use super::model::{Order, OrderStatus, PAYMENT_METHODS};
use super::pricing;
use super::repository;

/// Checkout the user's cart. When an `idempotency_key` is supplied, a second
/// attempt with the same (user_id, key) returns the original order instead
/// of creating a duplicate (double-click, network retry, mobile 502 retry).
///
/// Rule order: coupon load -> points-balance lock -> `pricing::price`
/// (subtotal -> coupon clamp -> points cap -> total -> points earned; see
/// that module for the arithmetic itself) -> `fulfilment::plan` (item_type
/// split: product lines to reserve, course ids to enrol) -> stock decrement
/// -> create order (paid) -> order_items -> artifacts (enrolments via
/// `enrol_batch`, subscriptions, points ledger) -> clear cart -> idempotency
/// -> outbox + notify.
///
/// The transactional cart/coupon reads and the enrolment/subscription DTO
/// assembly go through their owning modules' service seams (ADR-0005), so
/// this module holds no sibling repository imports. `server`/`now` are the
/// handler-supplied studio timezone + sampled clock: the order number's date
/// stamp is the studio-LOCAL calendar day (contract §3.18 裁決 2 wall-clock
/// semantics), not the UTC day.
pub async fn checkout(
    db: &PgPool,
    user_id: Uuid,
    idempotency_key: Option<String>,
    req: CheckoutRequest,
    correlation_id: Option<String>,
    server: &ServerConfig,
    now: DateTime<Utc>,
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

    // Resolve + validate `payment_method` before opening the transaction —
    // optional, defaults to `credit_card` for back-compat (existing callers
    // that never send this field must keep working); anything outside
    // `PAYMENT_METHODS` is a 422, raised before any DB work begins.
    let payment_method = req.payment_method.as_deref().unwrap_or("credit_card");
    if !PAYMENT_METHODS.contains(&payment_method) {
        return Err(AppError::Validation(format!(
            "invalid payment method: {payment_method}"
        )));
    }

    // All reads and writes happen inside the transaction so the cart snapshot,
    // product/course prices, stock decrement, and every artifact created are
    // consistent and serialized.
    let mut tx = db.begin().await?;

    // 2. Lock and read cart items + current product/course prices. Course
    //    lines are now first-class (the Task-3 "not yet supported" guard is
    //    gone).
    let cart_items = cart_service::find_cart_items_for_checkout_tx(&mut tx, user_id).await?;

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
            coupons_service::find_valid_by_code_tx(&mut tx, code)
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
        points_service::lock_balance_tx(&mut tx, user_id).await?
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
    //    `products::service::reserve_stock_tx` owns the lock-ordering
    //    discipline now (sorts by product_id before touching any row — see
    //    its doc comment for why) and hands back every decremented row,
    //    each already locked by this transaction; step 10b below reuses
    //    those rows instead of re-reading them.
    //
    //    `fulfilment::plan` does the item_type split (product lines to
    //    reserve, course ids to enrol) in one exhaustive match, replacing the
    //    two `.filter(matches!)` walks this body used to run. It sits in the
    //    original filter's position — right after pricing — so a coupon/
    //    overflow 422 still precedes the (today-unreachable) `Internal` a
    //    target-less line would raise. Course ids ride along in `plan` until
    //    step 10a.
    let plan = fulfilment::plan(&cart_items)?;

    let reserve_lines: Vec<(Uuid, i32, &str)> = plan
        .products
        .iter()
        .map(|p| (p.product_id, p.quantity, p.name.as_str()))
        .collect();
    let reserved = product_service::reserve_stock_tx(&mut tx, &reserve_lines).await?;

    // 7. Generate an order number. The `DF-YYYYMMDD` date prefix is the
    //    studio-LOCAL calendar day (`studio_clock::today` on the handler's
    //    sampled `now`), not the UTC day — a Taipei-evening checkout (UTC
    //    16:00–24:00) stamps tomorrow's local date, per contract §3.18 裁決 2
    //    wall-clock semantics. UUID-v7 suffix (base36-encoded last 32 bits)
    //    gives us an unambiguous, monotonic, unguessable unique component —
    //    no birthday collisions and no modulo bias.
    let order_number = {
        let suffix = Uuid::now_v7().as_u128() as u32;
        format!(
            "DF-{}{:08X}",
            studio_clock::today(studio_clock::studio_tz(server), now).format("%Y%m%d"),
            suffix
        )
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
        payment_method,
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
    // 10a. Enrolments — course lines. `enrol_batch_from_purchase_tx` owns the
    //      course-line lock-ordering discipline now (sorts by course_id
    //      before taking any `FOR UPDATE` on a course row, so two concurrent
    //      checkouts sharing two courses can't lock them in opposite orders
    //      and deadlock — the same discipline
    //      `products::service::reserve_stock_tx` applies to product lines in
    //      step 6, course lines' batch deep function at last, so this body no
    //      longer sorts them itself). A full course or a duplicate active
    //      enrolment rolls back the *entire* checkout (order, order_items,
    //      stock decrement — all of it), which is correct: partially
    //      fulfilling a cart is not an acceptable outcome.
    enrolments_service::enrol_batch_from_purchase_tx(&mut tx, user_id, &plan.course_ids, order.id)
        .await?;

    // 10b. Subscriptions — product lines whose product_type is
    //      entitlement-eligible. `grant_from_purchase_tx` itself returns
    //      `Ok(None)` for non-eligible types, so every product line is
    //      simply offered to it. It does not itself validate quantity >= 1;
    //      cart quantity is enforced to 1..=999 at add-time, so that always
    //      holds by the time we get here. The row comes straight out of
    //      `reserved` (step 6's `reserve_stock_tx` result) instead of a
    //      fresh read — that transaction already holds this row's lock,
    //      and the fields `grant_from_purchase_tx` reads
    //      (product_type/session_count/valid_days) are untouched by the
    //      stock decrement.
    for p in &plan.products {
        let product = reserved
            .get(&p.product_id)
            .expect("product line was reserved in step 6");
        subscriptions_service::grant_from_purchase_tx(
            &mut tx,
            user_id,
            product,
            p.quantity,
            p.price_cents,
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
    cart_service::clear_cart_tx(&mut tx, user_id).await?;

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
    outbox::insert_domain_event_tx(
        &mut tx,
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
        correlation_id,
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
    let enrolments = enrolments_service::list_by_order(db, order_id).await?;
    let subscriptions = subscriptions_service::list_by_order(db, order_id).await?;
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
    auth: &AuthUser,
) -> Result<OrderResponse, AppError> {
    let order = repository::find_by_id(db, order_id)
        .await?
        .ok_or_else(|| AppError::NotFound("order not found".into()))?;

    // Check ownership or admin
    auth.owns_or_admin(order.user_id, "not authorized to view this order")?;

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
    correlation_id: Option<String>,
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
    outbox::insert_domain_event_tx(
        &mut tx,
        OrderStatusChangedPayload {
            order_id: updated.id,
            user_id: updated.user_id,
            status: target.as_str().to_string(),
        },
        correlation_id,
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
