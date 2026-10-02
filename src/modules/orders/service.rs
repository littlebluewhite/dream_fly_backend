use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::kafka::events::{OrderCreatedPayload, OrderStatusChangedPayload};
use crate::kafka::outbox;
use crate::modules::cart::service as cart_service;
use crate::modules::coupons::model::Coupon;
use crate::modules::coupons::service as coupons_service;
use crate::modules::enrolments::dto::EnrolmentResponse;
use crate::modules::enrolments::service as enrolments_service;
use crate::modules::notifications::service as notify;
use crate::modules::points::service as points_service;
use crate::modules::products::service as product_service;
use crate::modules::subscriptions::dto::SubscriptionResponse;
use crate::modules::subscriptions::service as subscriptions_service;
use crate::utils::studio_clock::{self, StudioNow};

use super::dto::{
    AdminOrderListResponse, AdminOrderSummary, CheckoutRequest, OrderListResponse, OrderResponse,
    OrderSummary,
};
use super::fulfilment;
use super::idempotency::{self, IdempotencyKey, Recorded};
use super::locks;
use super::model::{Order, OrderStatus, PAYMENT_METHODS};
use super::pricing;
use super::refund::{self, TransitionDecision};
use super::repository::{self, OrderAmounts};
use super::tx_witness::TxReleased;

/// The checkout request after every check that needs no database: the
/// payment method resolved against `PAYMENT_METHODS` (defaulting to
/// `credit_card` for back-compat — existing callers that never send the
/// field must keep working), the coupon code trimmed with a blank code
/// meaning "no coupon", and `use_points` defaulted to `false`.
#[derive(Debug)]
struct CheckoutIntent {
    payment_method: &'static str,
    coupon_code: Option<String>,
    use_points: bool,
}

/// Parse+validate a `CheckoutRequest` into a [`CheckoutIntent`].
/// `AppError::Validation` (422) on a payment method outside
/// `PAYMENT_METHODS`. Template: `courses::service::parse_schedule_slots`.
fn parse_request(req: CheckoutRequest) -> Result<CheckoutIntent, AppError> {
    let payment_method = match req.payment_method.as_deref() {
        None => "credit_card",
        Some(m) => PAYMENT_METHODS
            .into_iter()
            .find(|&p| p == m)
            .ok_or_else(|| AppError::Validation(format!("invalid payment method: {m}")))?,
    };
    let coupon_code = req
        .coupon_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Ok(CheckoutIntent {
        payment_method,
        coupon_code,
        use_points: req.use_points.unwrap_or(false),
    })
}

/// Checkout the user's cart. When an idempotency `key` is supplied, a second
/// attempt with the same (user_id, key) returns the original order instead
/// of creating a duplicate (double-click, network retry, mobile 502 retry) —
/// the table and its replay mechanics are owned by `orders::idempotency`.
///
/// Outcome priority — the first rule that fires decides the response:
/// - Idempotency replay: a key already recorded for this user returns that
///   order, before any other check (`idempotency::replay`).
/// - Invalid payment method: 422 (`parse_request`), before the transaction
///   opens.
/// - Unknown user: 404 from the points-balance lock (`lock_balance_tx`,
///   the first step of `locks::acquire_checkout_locks` — unconditional and
///   tx-first, the user-first lock order that keeps refund/cancel
///   compensation mutually exclusive with checkout for the same buyer;
///   ADR-0007).
/// - Empty cart: 400 — unless a same-key twin already committed, which is
///   replayed instead (`idempotency::replay_or`).
/// - Deactivated line: 422 (`fulfilment::ensure_all_purchasable`).
/// - Unknown/inactive/expired coupon: 422.
/// - Subtotal overflow: 422 (`pricing::price`, which also does the coupon
///   clamp, points cap, total and points earned).
/// - Illegal line quantity: 400 outside `1..=999`, 422 for a time-based
///   entitlement (`valid_days` set, no `session_count`) at quantity ≠ 1 —
///   `Product::ensure_line_quantity`, run by
///   `products::service::reserve_stock_tx` over every product line (lock
///   order, first failure wins) before its first decrement. The cart
///   rejects both at add/update time; this catches lines carted before
///   that rule.
/// - Insufficient stock: 409 (`products::service::reserve_stock_tx`).
/// - Full course / already enrolled: 409
///   (`enrolments::service::enrol_batch_from_purchase_tx`).
/// - Idempotency unique violation: a concurrent same-key twin won — its
///   order is replayed (`idempotency::record`).
///
/// Every rejection after the transaction opens rolls the whole checkout
/// back. On success the order is created already `paid`, followed by its
/// order_items, enrolments, subscriptions, points ledger rows, the cart
/// clear, the idempotency row and the outbox event — one transaction — then
/// the inline notification.
///
/// The transactional cart/coupon reads and the enrolment/subscription DTO
/// assembly go through their owning modules' service seams (ADR-0005), so
/// this module holds no sibling repository imports. `at` is the
/// handler-supplied studio timezone + sampled clock: the order number's date
/// stamp is the studio-LOCAL calendar day (contract §3.18 裁決 2 wall-clock
/// semantics), not the UTC day.
pub async fn checkout(
    db: &PgPool,
    user_id: Uuid,
    key: Option<IdempotencyKey>,
    req: CheckoutRequest,
    correlation_id: Option<String>,
    at: StudioNow,
) -> Result<OrderResponse, AppError> {
    let StudioNow { tz, now } = at;
    // Idempotency pre-check (outside tx). If we've already processed this
    // key for this user, return the prior order (artifacts included).
    if let Some(response) = idempotency::replay(db, user_id, key.as_ref()).await? {
        return Ok(response);
    }

    // Parse the request before opening the transaction — after the replay
    // above, so a same-key retry still returns its order.
    let intent = parse_request(req)?;

    // All reads and writes happen inside the transaction so the cart snapshot,
    // product/course prices, stock decrement, and every artifact created are
    // consistent and serialized.
    let mut tx = db.begin().await?;

    // Take every lock this checkout will need, in the order lock protocol's
    // fixed order: the buyer's points-balance row FIRST (unconditionally,
    // even when `use_points=false`, and before any cart read), then the
    // cart's products and courses ascending. Same-buyer and cross-buyer
    // rationale: `orders::locks` module doc (ADR-0007 決策 5).
    let locks = locks::acquire_checkout_locks(&mut tx, user_id).await?;

    // Lock and read cart items + current product/course prices (the
    // product/course rows are already held by `locks`). Course
    // lines are now first-class (the Task-3 "not yet supported" guard is
    // gone).
    let cart_items =
        cart_service::find_cart_items_for_checkout_tx(&mut tx, locks.balance()).await?;

    if cart_items.is_empty() {
        // A same-key twin may already have won this cart — replay it
        // instead of the 400 (`idempotency::replay_or` has the why).
        return idempotency::replay_or(
            db,
            tx,
            user_id,
            key.as_ref(),
            AppError::BadRequest("cart is empty".into()),
        )
        .await;
    }

    // Purchasability gate (甲案): every line in the snapshot just locked
    // above must still be active, or the whole checkout is rejected — 422,
    // naming every deactivated line so the buyer knows what to remove (see
    // `fulfilment::ensure_all_purchasable`). Three deliberate ordering
    // decisions:
    //   - AFTER the empty-cart check above, not before: a truly empty cart
    //     still 400s exactly as before; a cart whose every line has since
    //     gone inactive is non-empty at the snapshot level, so it lands
    //     here instead — this 422 replaces what used to be a misleading
    //     "cart is empty" 400 for that case.
    //   - BEFORE the coupon load below: whether the cart's own
    //     contents are still legal to buy is decided before any discount is
    //     even considered.
    //   - Deliberately NOT paired with an idempotency replay, unlike the
    //     empty-cart branch above and the unique-violation branch
    //     (`idempotency::record`) further down this function: a product/course
    //     going inactive is never *caused* by a concurrent checkout attempt
    //     — it's an independent admin-side deactivation — so there is no
    //     winning twin transaction to go replay here. This 422 is a business
    //     rejection on the same footing as the coupon 422 right below it;
    //     the idempotency pre-check already run at the top of this function
    //     covers a genuine same-key replay.
    let cart = fulfilment::ensure_all_purchasable(cart_items)?;

    // Coupon (optional), loaded and validated here — an unknown/
    // inactive/expired code is rejected outright — the caller should not
    // be silently charged full price while believing a discount applied.
    // Only the load happens in checkout; `pricing::price` turns this
    // (already-valid) coupon into the actual discount once the cart's
    // subtotal is known.
    let mut coupon: Option<Coupon> = None;
    if let Some(code) = intent.coupon_code.as_deref() {
        coupon = Some(
            coupons_service::find_valid_by_code_tx(&mut tx, code)
                .await?
                .ok_or_else(|| AppError::Validation("invalid coupon".into()))?,
        );
    }

    // Price the cart — subtotal, coupon clamp, points cap, total, and
    // points earned, all in one pure call now that the coupon is loaded
    // and the points balance is locked. The `FOR UPDATE` lock on this
    // user's balance was taken unconditionally at the top of the tx, so a
    // second concurrent checkout by the same user blocks until we commit or
    // roll back — no double-spend against a now-stale balance. The locked
    // balance is passed as-is: `pricing::price` reads it only when
    // `use_points`. Accepted behavior note: subtotal
    // overflow is now detected inside this call, after the coupon load
    // above (it used to run first) — a cart whose subtotal overflows i64
    // *and* carries an invalid coupon code now surfaces the coupon's 422
    // instead of the overflow error. This needs an astronomical cart to
    // reach; see `pricing::price` for the arithmetic itself.
    let outcome = pricing::price(
        &cart,
        coupon.as_ref(),
        locks.balance().balance(),
        intent.use_points,
    )?;

    // Line quantity check, then stock decrement — product lines only; fail
    // fast. `products::service::reserve_stock_tx` walks the lines in the
    // `ProductLocks` witness's lock order (`in_lock_order`, ascending
    // product_id — see `orders::locks`), judges every line's quantity
    // against its locked row before the first decrement, and hands back
    // every decremented row, each already locked by this transaction; the
    // subscription grant below reuses those rows instead of re-reading them.
    //
    // `fulfilment::plan` does the line-target split (product lines to
    // reserve, course ids to enrol) in one exhaustive match, replacing the
    // two `.filter(matches!)` walks this body used to run. It cannot fail:
    // each line's target was decoded once at the snapshot read
    // (`LineTarget`). Course ids ride along in `plan` until the enrolment
    // batch below.
    let plan = fulfilment::plan(&cart);

    let reserve_lines: Vec<(Uuid, i32, &str)> = plan
        .products
        .iter()
        .map(|p| (p.product_id, p.quantity, p.name.as_str()))
        .collect();
    let reserved =
        product_service::reserve_stock_tx(&mut tx, locks.products(), &reserve_lines).await?;

    // Generate an order number. The `DF-YYYYMMDD` date prefix is the
    // studio-LOCAL calendar day (`studio_clock::today` on the handler's
    // sampled `now`), not the UTC day — a Taipei-evening checkout (UTC
    // 16:00–24:00) stamps tomorrow's local date, per contract §3.18 裁決 2
    // wall-clock semantics. UUID-v7 suffix (hex-encoded last 32 bits)
    // gives us an unambiguous, monotonic, unguessable unique component —
    // no birthday collisions and no modulo bias.
    let order_number = {
        let suffix = Uuid::now_v7().as_u128() as u32;
        format!(
            "DF-{}{:08X}",
            studio_clock::today(tz, now).format("%Y%m%d"),
            suffix
        )
    };

    // Create the order row FIRST, already `paid` — order_id is needed
    // before enrolments/subscriptions/ledger rows can link to it.
    let order = repository::create_order(
        &mut tx,
        user_id,
        &order_number,
        OrderAmounts {
            total_cents: outcome.total_cents,
            discount_cents: outcome.discount_cents,
            points_used: outcome.points_used,
            points_earned: outcome.points_earned,
        },
        outcome.applied_coupon_code.as_deref(),
        intent.payment_method,
    )
    .await?;

    // order_items from the (locked) cart snapshot — both product and
    // course lines. `fulfilment::order_lines` (plan()'s sister pure
    // function) turns the snapshot into named `OrderLine`s: `name`
    // becomes the order_items snapshot column, so later reads
    // (OrderSummary/AdminOrderSummary `items`) never need to join the
    // live product/course catalog; `stock_decremented` is derived from
    // `reserved`'s post-decrement rows — see that function's doc for
    // the exact rule.
    let lines = fulfilment::order_lines(&cart, &reserved);
    repository::create_order_items(&mut tx, order.id, &lines).await?;

    // Artifacts.
    // Enrolments — course lines. `enrol_batch_from_purchase_tx` walks
    // them in the `CourseLocks` witness's lock order (`in_lock_order`,
    // ascending course_id — the same protocol `reserve_stock_tx` follows
    // for product lines above; see `orders::locks`). A full course or
    // a duplicate active
    // enrolment rolls back the *entire* checkout (order, order_items,
    // stock decrement — all of it), which is correct: partially
    // fulfilling a cart is not an acceptable outcome.
    enrolments_service::enrol_batch_from_purchase_tx(
        &mut tx,
        locks.courses(),
        user_id,
        &plan.course_ids,
        order.id,
    )
    .await?;

    // Subscriptions — product lines whose product_type is
    // entitlement-eligible. `grant_from_purchase_tx` itself returns
    // `Ok(None)` for non-eligible types, so every product line is
    // simply offered to it. It does not itself validate the quantity:
    // `reserve_stock_tx` above already ran `Product::ensure_line_quantity`
    // on every product line (`entitlement::plan`'s precondition). The row
    // comes straight out of `reserved` (the `reserve_stock_tx` result
    // above) instead of a fresh read — that transaction already holds this
    // row's lock, and the fields `grant_from_purchase_tx` reads
    // (product_type/session_count/valid_days) are untouched by the stock
    // decrement.
    for p in &plan.products {
        let product = reserved.get(&p.product_id).ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("product line was reserved by reserve_stock_tx"))
        })?;
        subscriptions_service::grant_from_purchase_tx(
            &mut tx,
            user_id,
            product,
            p.quantity,
            p.price_cents,
            order.id,
            now,
        )
        .await?;
    }

    // Points ledger — `PricingOutcome::ledger_deltas` owns the order
    // (redeem before earn) and the zero-skip.
    for delta in outcome.ledger_deltas(order.id) {
        points_service::apply_delta_tx(&mut tx, user_id, delta).await?;
    }

    // Clear the cart within the same transaction.
    cart_service::clear_cart_tx(&mut tx, user_id).await?;

    // Record the idempotency key inside the same tx; a concurrent same-key
    // twin that already committed wins, and its order is the response.
    let mut tx = match idempotency::record(db, tx, user_id, key.as_ref(), order.id).await? {
        Recorded::Fresh(tx) => tx,
        Recorded::TwinWon(response) => return Ok(response),
    };

    // Queue the order_created event into the outbox — persisted
    // atomically with the order itself. The background dispatcher (see
    // `kafka::outbox::start_dispatcher`) publishes it to Kafka with
    // at-least-once semantics.
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

    let released = TxReleased::commit(tx).await?;

    // Inline notification — the user expects order confirmation
    // regardless of whether Kafka is enabled, and even if the
    // dispatcher hasn't drained the event yet.
    notify::order_placed(order.user_id, order.id, &order.order_number)
        .deliver(db)
        .await;

    // Assemble the response (items + artifacts, looked up by order_id).
    assemble_response(db, order, released).await
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
///
/// The `_released` witness proves the caller's checkout/status transaction is
/// already committed or rolled back before this runs: the item + artifact
/// reads below go through the pool, and a still-open tx holding the pooled
/// connection would self-deadlock a low-connection pool (see
/// `tx_witness::TxReleased`). The value is unused at runtime — its work is
/// done at the type level, by being impossible to obtain without releasing.
pub(super) async fn assemble_response(
    db: &PgPool,
    order: Order,
    _released: TxReleased,
) -> Result<OrderResponse, AppError> {
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

    assemble_response(db, order, TxReleased::no_open_tx()).await
}

pub async fn my_orders(
    db: &PgPool,
    user_id: Uuid,
    pagination: &PaginationParams,
) -> Result<OrderListResponse, AppError> {
    let orders =
        repository::find_by_user(db, user_id, pagination.limit(), pagination.offset()).await?;
    let total = repository::count_by_user(db, user_id).await?;

    let summaries: Vec<OrderSummary> = orders.into_iter().map(OrderSummary::from).collect();

    Ok(OrderListResponse {
        orders: summaries,
        meta: pagination.meta(total),
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

    // Everything in a single tx: read+lock current status →
    // `refund::decide_transition` (same-status no-op / 400 / flip / flip +
    // compensate) → single atomic status UPDATE → outbox. Reading `current` under `FOR
    // UPDATE` is also the `orders`-row lock that opens the refund lock order
    // (full graph: `orders::locks` module doc).
    let mut tx = db.begin().await?;

    let current = repository::find_by_id_tx(&mut tx, order_id)
        .await?
        .ok_or_else(|| AppError::NotFound("order not found".into()))?;

    // The decision is taken only after the `FOR UPDATE` read above. An
    // illegal transition's 400 drop-rolls the tx back.
    match refund::decide_transition(&current.status, &target)? {
        // Same-status no-op: return the order unchanged — no UPDATE, no
        // outbox, no notification, no compensation — so a retried
        // webhook/admin PATCH is an *observable* idempotent no-op. Release
        // the tx before `assemble_response` (shared self-deadlock rationale
        // in `TxReleased`).
        TransitionDecision::NoOp => {
            let released = TxReleased::release(tx);
            return assemble_response(db, current, released).await;
        }
        TransitionDecision::Flip => {}
        // Refund/cancel compensation: undo the checkout side effects BEFORE
        // the status UPDATE and in this same tx. A
        // `users_points_balance_check` violation on the clawback surfaces as
        // `Conflict("點數不足")` and rolls the WHOLE transaction back —
        // status flip included — so there is no half-applied refund
        // (Cancelled ≡ Refunded compensation semantics).
        TransitionDecision::FlipAndCompensate => {
            compensate_order_artifacts_tx(&mut tx, &current).await?;
        }
    }

    let updated = repository::update_status_tx(&mut tx, order_id, &target)
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

    let released = TxReleased::commit(tx).await?;

    // Inline notification — every status change is user-visible and
    // shouldn't wait for the outbox dispatcher tick.
    notify::order_status_changed(
        updated.user_id,
        updated.id,
        &updated.order_number,
        target.as_str(),
    )
    .deliver(db)
    .await;

    assemble_response(db, updated, released).await
}

/// Undo a paid order's checkout side effects as part of moving it into a
/// terminal cancelled/refunded state (Cancelled ≡ Refunded). Runs inside
/// `update_order_status`'s transaction, BEFORE the status UPDATE, so the whole
/// thing — points reversal, restock, artifact cancellation, and the status
/// flip itself — commits atomically or (on the 409 clawback path) rolls back
/// together. Not a standalone `refund_order` entry point: reusing
/// `update_order_status` avoids re-implementing its parse / lock / transition
/// check / outbox / notify.
///
/// A flat list of owner calls — each owner undoes its own checkout side
/// effect from its own checkout *trace* (ADR-0007 決策 8), keyed by
/// `order_id`; `orders` computes nothing here:
/// 1. `locks::acquire_refund_locks` — the buyer's `users` row
///    UNCONDITIONALLY (even a zero-points order; same-buyer checkout/refund
///    exclusion, 決策 5), then the products the order will restock,
///    ascending. 404 "user not found"; `Internal` for a product line missing
///    its `product_id`.
/// 2. `points::service::reverse_order_tx` — reverses the order's
///    `checkout_earn`/`checkout_redeem` ledger flow, restore before
///    clawback (決策 4); 409「點數不足」 on a clawback the balance can't cover.
/// 3. `products::service::restore_for_order_tx` — restocks the
///    `stock_decremented=true` lines in witness order; `Internal` for a
///    product that doesn't resolve.
/// 4. `enrolments`/`subscriptions` `cancel_by_order_tx` — order-scoped batch
///    UPDATEs, naturally idempotent via `status <> 'cancelled'`, so a buyer
///    who already self-cancelled an enrolment is a harmless 0-row no-op.
///    The enrolments owner also cancels the pending leave requests of the
///    enrolments it just flipped (B5). Lock order of the whole refund:
///    `orders::locks` module doc.
///
/// Fixture / directly-built orders, and orders from seed runs before the
/// seed wrote checkout ledger rows, carry no traces, so every step no-ops
/// (the legacy-data policy — no special-casing).
async fn compensate_order_artifacts_tx(
    tx: &mut Transaction<'_, Postgres>,
    order: &Order,
) -> Result<(), AppError> {
    let locks = locks::acquire_refund_locks(tx, order).await?;
    points_service::reverse_order_tx(tx, locks.balance(), order.id).await?;
    product_service::restore_for_order_tx(tx, locks.products(), order.id).await?;
    enrolments_service::cancel_by_order_tx(tx, order.id).await?;
    subscriptions_service::cancel_by_order_tx(tx, order.id).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        coupon_code: Option<&str>,
        use_points: Option<bool>,
        payment_method: Option<&str>,
    ) -> CheckoutRequest {
        CheckoutRequest {
            coupon_code: coupon_code.map(str::to_owned),
            use_points,
            payment_method: payment_method.map(str::to_owned),
        }
    }

    #[test]
    fn parse_request_defaults_omitted_fields() {
        // Back-compat: a caller that never sends these fields pays by
        // credit card, applies no coupon and redeems no points.
        let intent = parse_request(request(None, None, None)).expect("parses");
        assert_eq!(intent.payment_method, "credit_card");
        assert_eq!(intent.coupon_code, None);
        assert!(!intent.use_points);
    }

    #[test]
    fn parse_request_rejects_payment_method_outside_the_value_domain() {
        let err = parse_request(request(None, None, Some("bitcoin"))).expect_err("must reject");
        assert!(
            matches!(err, AppError::Validation(ref m) if m == "invalid payment method: bitcoin"),
            "got: {err:?}"
        );
    }

    #[test]
    fn parse_request_trims_coupon_and_passes_valid_fields_through() {
        let intent =
            parse_request(request(Some("  SAVE10  "), Some(true), Some("line_pay"))).expect("parses");
        assert_eq!(intent.payment_method, "line_pay");
        assert_eq!(intent.coupon_code.as_deref(), Some("SAVE10"));
        assert!(intent.use_points);

        // A blank code is "no coupon", not an invalid-coupon 422.
        let blank = parse_request(request(Some("   "), None, None)).expect("parses");
        assert_eq!(blank.coupon_code, None);
    }
}
