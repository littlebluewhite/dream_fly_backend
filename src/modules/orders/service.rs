use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
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
use super::refund;
use super::repository;

/// Checkout the user's cart. When an `idempotency_key` is supplied, a second
/// attempt with the same (user_id, key) returns the original order instead
/// of creating a duplicate (double-click, network retry, mobile 502 retry).
///
/// Rule order: points-balance lock (unconditional, tx-first — the unified
/// lock ordering that keeps refund/cancel compensation mutually exclusive
/// with checkout for the same buyer; see the body) -> coupon load ->
/// `pricing::price`
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
            return assemble_response(db, order, tx_witness::TxReleased::no_open_tx()).await;
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

    // Lock the buyer's points-balance row FIRST — unconditionally, even when
    // `use_points=false`. This is the checkout half of a unified lock order
    // that keeps refund/cancel compensation (Step 10e) fully mutually
    // exclusive with checkout for the same buyer:
    //   checkout: users -> cart_items/products/courses (SHARE, the cart read
    //             below; its second SQL also takes FOR SHARE on courses) ->
    //             products (UPDATE, product_id asc) -> courses (asc) ->
    //             enrolments -> subscriptions
    //   refund:   orders -> users (explicit unconditional lock_balance_tx,
    //             Step 10e) -> products (UPDATE, asc) -> enrolments ->
    //             subscriptions
    // Making the lock merely *unconditional* is not enough — it must be taken
    // BEFORE the cart read. `find_cart_items_for_checkout_tx` already takes
    // `FOR SHARE` on the product/course rows, so if it ran first checkout
    // would hold products-SHARE while waiting on users, and refund holds
    // users while waiting on products-UPDATE: a deadlock cycle. Taking
    // `users` first on both paths makes it the unconditional first lock, so
    // no cycle can form. An empty cart is harmless: the empty-cart branch
    // just below drops the tx and the rollback releases this lock.
    // (Pre-existing, neither introduced nor fixed here: two checkouts racing
    // a SHARE->UPDATE upgrade on the same product can still deadlock — PG's
    // detector aborts one.)
    //
    // Cross-buyer dimension (single code anchor for this argument; prose
    // authority remains ADR-0007 決策 5): the users-first lock above only
    // serializes the SAME buyer's checkout vs refund — two different buyers
    // hold two different `users` rows, so it does nothing for them. That gap
    // is closed by a *global* `product_id`-ascending order enforced
    // independently at every site that ever locks a `products` row, so no
    // two transactions can hold locks on the same pair of products in
    // opposite orders, regardless of which buyers or paths are involved:
    //   1. cart's SHARE pre-lock — dedicated pre-lock query inside
    //      `find_cart_items_for_checkout_tx` (`cart::repository`)
    //   2. checkout's UPDATE reservation — `reserve_stock_tx` (step 6, below)
    //   3. refund's UPDATE restore — `restore_stock_tx` (compensation, Step 10e)
    // Regression test:
    // `checkout_cart_read_locks_products_ascending_no_cross_buyer_deadlock`.
    // The sort itself is not shared code: each site's write-lock owner sorts
    // independently — no shared helper (CONTEXT.md「行計畫」詞條裁決).
    let locked_points_balance = points_service::lock_balance_tx(&mut tx, user_id).await?;

    // 2. Lock and read cart items + current product/course prices. Course
    //    lines are now first-class (the Task-3 "not yet supported" guard is
    //    gone).
    let cart_items = cart_service::find_cart_items_for_checkout_tx(&mut tx, user_id).await?;

    if cart_items.is_empty() {
        // A concurrent request carrying the *same* idempotency key may have
        // already won this cart. Idempotency is scoped per user_id, so both
        // requests first contend on the SAME buyer's `users` row: the
        // unconditional `lock_balance_tx` call above (Step
        // 10b moved it ahead of the cart read) is what actually serializes
        // the two checkouts. The loser blocks there until the winner locks
        // these same cart rows, runs the whole checkout, clears the cart,
        // and commits (steps 11/12/`TxReleased::commit` below) — so by the time the
        // loser is unblocked and reaches this empty-cart check, the winner
        // is guaranteed to have already committed too (cart-clear and
        // idempotency-insert share that one transaction). Failing outright
        // here, before ever checking idempotency, breaks the "same key
        // replay returns the first order" contract. Release the tx first —
        // the replay's `assemble_response` runs pool queries, so the
        // connection has to go back before it (the shared self-deadlock
        // rationale now lives in `TxReleased`) — then re-check idempotency
        // before giving up. Note the release only happens when a key is
        // present: with `idempotency_key == None` the `if let` is skipped,
        // `tx` is never moved, and the `Err` return below drop-rolls-it-back
        // exactly as before.
        if let Some(key) = &idempotency_key {
            let released = tx_witness::TxReleased::release(tx);
            if let Some(existing_id) = repository::find_idempotency(db, user_id, key).await? {
                let existing_order = repository::find_by_id(db, existing_id)
                    .await?
                    .ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!(
                            "idempotency row referenced missing order {existing_id}"
                        ))
                    })?;
                return assemble_response(db, existing_order, released).await;
            }
        }
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

    // 4. Resolve the balance `pricing::price` will see. The `FOR UPDATE` lock
    //    on this user's balance was already taken unconditionally at the top
    //    of the tx (see the lock-ordering note there), so a second concurrent
    //    checkout by the same user blocks until we commit or roll back — no
    //    double-spend against a now-stale balance. Here we only choose the
    //    value to price against: the locked balance when redeeming, or `0`
    //    when not. `use_points=false` still prices bit-for-bit as before —
    //    `pricing::price` reads the balance only inside its `use_points`
    //    branch.
    let use_points = req.use_points.unwrap_or(false);
    let points_balance = if use_points { locked_points_balance } else { 0 };

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
    //    wall-clock semantics. UUID-v7 suffix (hex-encoded last 32 bits)
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
    //     to join the live product/course catalog. `stock_decremented`
    //     (Step 10a/10c) snapshots whether this line actually decremented
    //     `products.stock` — read off `reserved`'s post-decrement row
    //     (`stock.is_some()` means the product had finite stock, so it
    //     really was decremented; `None` means the product was
    //     unlimited-stock and untouched — `try_decrement_stock_tx`'s
    //     NULL-preserving CASE, `products/repository.rs`). Always `false`
    //     for course lines, which never have a `product_id` and so never
    //     reach `reserved` at all.
    let items_data: Vec<(Option<Uuid>, Option<Uuid>, i32, i64, String, bool)> = cart_items
        .iter()
        .map(|ci| {
            let stock_decremented = ci
                .product_id
                .and_then(|pid| reserved.get(&pid))
                .map(|p| p.stock.is_some())
                .unwrap_or(false);
            (
                ci.product_id,
                ci.course_id,
                ci.quantity,
                ci.price_cents,
                ci.name.clone(),
                stock_decremented,
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
    //      step 6 — course lines get a batch deep function of their own at
    //      last, so this body no longer sorts them itself). A full course or
    //      a duplicate active
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
                // Concurrent retry beat us. Release the tx first — the replay
                // path's `assemble_response` runs pool queries (shared
                // self-deadlock rationale in `TxReleased`) — then let the
                // caller re-read the winning row. On the fall-through
                // `Conflict` path `released` simply drops unused (the witness
                // is a permission, not a `#[must_use]` obligation).
                let released = tx_witness::TxReleased::release(tx);
                if let Some(existing_id) = repository::find_idempotency(db, user_id, key).await? {
                    let existing_order = repository::find_by_id(db, existing_id)
                        .await?
                        .ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "idempotency row referenced missing order {existing_id}"
                            ))
                        })?;
                    return assemble_response(db, existing_order, released).await;
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

    let released = tx_witness::TxReleased::commit(tx).await?;

    // 14. Inline notification — the user expects order confirmation
    //     regardless of whether Kafka is enabled, and even if the
    //     dispatcher hasn't drained the event yet.
    notify::order_placed(order.user_id, order.id, &order.order_number)
        .deliver(db)
        .await;

    // 15. Assemble the response (items + artifacts, looked up by order_id).
    assemble_response(db, order, released).await
}

/// Self-deadlock discipline for `assemble_response`, lifted out of three
/// cross-referencing comments into the type system. See `TxReleased`.
///
/// A private submodule on purpose: `TxReleased`'s only field is private, so
/// one can be built solely from *inside this module*. Isolating the type here
/// means the surrounding `service` functions — the very code this discipline
/// governs — cannot hand-write `TxReleased(())` to skip the constructors; they
/// must go through `release` / `commit` / `no_open_tx`. (Same private-field
/// witness technique as `courses::seats`'s `SessionLock`.)
mod tx_witness {
    use sqlx::{Postgres, Transaction};

    /// Proof that the checkout / status-update transaction has already been
    /// released back to the pool — rolled back via `release` or committed via
    /// `commit` — *before* `assemble_response` runs.
    ///
    /// `assemble_response` re-reads the order's items + artifacts through the
    /// pool (`fetch_artifacts`). Issuing a pool query while a transaction
    /// still holds its pooled connection self-deadlocks under a low-connection
    /// pool: the query waits for a free connection, the only connection is not
    /// freed until the transaction ends, and the transaction cannot end while
    /// it is blocked on that query. `assemble_response` takes this witness *by
    /// value*, so it cannot be reached without proof the tx is already gone —
    /// the invariant now lives in the signature instead of in a comment every
    /// caller has to remember.
    ///
    /// Deliberately not `#[must_use]`: the witness is *permission* to call
    /// `assemble_response`, not an obligation to do anything with it — a path
    /// that releases its tx and then returns an error (e.g. the unique-
    /// violation branch falling through to a `Conflict`) legitimately drops it
    /// unused.
    ///
    /// Honest residual seam: `no_open_tx` is a caller-attested assertion, not
    /// a machine-checked fact. The two read-only callers (`checkout`'s
    /// idempotency pre-check and `get_order`) never open a transaction, so
    /// there is nothing to release; the witness there records "this path holds
    /// no open tx" on the caller's word. `release` and `commit` consume a real
    /// `Transaction`, so those two are machine-checked.
    pub(super) struct TxReleased(());

    impl TxReleased {
        pub(super) fn release(tx: Transaction<'_, Postgres>) -> Self {
            drop(tx);          // sqlx 對被 drop 的交易發 rollback,語意同原呼叫端
            Self(())
        }
        pub(super) async fn commit(tx: Transaction<'_, Postgres>) -> Result<Self, sqlx::Error> {
            tx.commit().await?;
            Ok(Self(()))
        }
        pub(super) fn no_open_tx() -> Self {
            Self(())
        }
    }
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
async fn assemble_response(
    db: &PgPool,
    order: Order,
    _released: tx_witness::TxReleased,
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

    assemble_response(db, order, tx_witness::TxReleased::no_open_tx()).await
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

    // Everything in a single tx: read+lock current status → (same-status
    // early-return) → check transition → compensate → single atomic UPDATE
    // with conditional `paid_at` → outbox. Reading `current` under `FOR
    // UPDATE` is also the `orders`-row lock that opens the refund lock order
    // (orders → users → products → enrolments → subscriptions, see
    // `compensate_order_artifacts_tx`). No split UPDATE+UPDATE+SELECT that
    // could leave `status='paid' AND paid_at=NULL`.
    let mut tx = db.begin().await?;

    let current = repository::find_by_id_tx(&mut tx, order_id)
        .await?
        .ok_or_else(|| AppError::NotFound("order not found".into()))?;

    // Same-status no-op: return the order unchanged — no UPDATE, no outbox, no
    // notification, no compensation — so a retried webhook/admin PATCH is an
    // *observable* idempotent no-op (the old path re-UPDATEd + re-queued the
    // outbox + re-notified on every same-status call; and re-running here
    // would try to compensate a second time). Release the tx before
    // `assemble_response` (shared self-deadlock rationale in `TxReleased`).
    if current.status.as_str() == target.as_str() {
        let released = tx_witness::TxReleased::release(tx);
        return assemble_response(db, current, released).await;
    }

    if !current.status.can_transition_to(&target) {
        return Err(AppError::BadRequest(format!(
            "cannot transition order from '{}' to '{}'",
            current.status.as_str(),
            target.as_str()
        )));
    }

    // Refund/cancel compensation: undo the checkout side effects when a
    // revenue order moves into a terminal cancelled/refunded state, BEFORE the
    // status UPDATE and in this same tx. A `users_points_balance_check`
    // violation on the clawback surfaces as `Conflict("點數不足")` and rolls
    // the WHOLE transaction back — status flip included — so there is no
    // half-applied refund (Cancelled ≡ Refunded compensation semantics).
    if refund::compensation_required(&current.status, &target) {
        compensate_order_artifacts_tx(&mut tx, &current).await?;
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

    let released = tx_witness::TxReleased::commit(tx).await?;

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
/// Order of operations (ADR-0007), sibling reads/writes all through the
/// service seams (ADR-0005 — `orders` never touches a sibling repository):
/// 1. Lock the buyer's points-balance row UNCONDITIONALLY — even a
///    zero-points order takes it. This is the refund half of the unified lock
///    order (orders → users → products → enrolments → subscriptions) that
///    keeps a same-buyer checkout and refund mutually exclusive; it also lets
///    the ledger flow-sum read below observe a consistent snapshot under the
///    same lock (a `use_points=false`/zero-flow refund would otherwise lock
///    `users` only implicitly, via a points UPDATE that never happens).
/// 2. Read the checkout *traces* — line items (each carrying its checkout-time
///    `stock_decremented` snapshot) and the order's `checkout_earn`/
///    `checkout_redeem` ledger flow sums, both keyed by `order_id`. A seed /
///    history / directly-built order that never went through checkout carries
///    none of these, so `plan_refund` computes an all-zero plan and the whole
///    body no-ops (the legacy-data policy — no special-casing).
/// 3. Points reversal — RESTORE (positive, reverses `checkout_redeem`) FIRST,
///    then CLAWBACK (negative, reverses `checkout_earn`). Under this one tx
///    the `users_points_balance_check` CHECK is evaluated per statement, so
///    applying the positive restore before the negative clawback relaxes the
///    success condition from `balance ≥ earned` to `balance + restored ≥
///    earned` (a deliberate deviation from strict reverse order — ADR-0007).
///    Each direction is skipped when its magnitude is 0 (`apply_delta_tx`
///    rejects a zero delta); both carry `order_id`, so the partial unique
///    index `uniq_point_ledger_refund_once` caps each direction at one row per
///    order.
/// 4. Restock — only the `stock_decremented=true` lines (`plan_refund` already
///    filtered); `restore_stock_tx` owns the ascending-`product_id` write-lock
///    ordering.
/// 5. Cancel the order's enrolments + subscriptions (order-scoped batch
///    UPDATEs, naturally idempotent via `status <> 'cancelled'`, so a buyer
///    who already self-cancelled an enrolment is a harmless 0-row no-op).
async fn compensate_order_artifacts_tx(
    tx: &mut Transaction<'_, Postgres>,
    order: &Order,
) -> Result<(), AppError> {
    // 1. Lock the buyer's balance unconditionally.
    points_service::lock_balance_tx(tx, order.user_id).await?;

    // 2. Read the checkout traces.
    let items = repository::find_items_by_order_tx(tx, order.id).await?;
    let flow = points_service::find_order_flow_sums_tx(tx, order.id).await?;

    // 3. Pure plan.
    let plan = refund::plan_refund(order, &items, flow)?;

    // 4. Points reversal — restore first, clawback second.
    if plan.restore_points > 0 {
        points_service::apply_delta_tx(
            tx,
            order.user_id,
            plan.restore_points,
            PointReason::RefundRestore,
            Some(order.id),
        )
        .await?;
    }
    if plan.clawback_points > 0 {
        points_service::apply_delta_tx(
            tx,
            order.user_id,
            -plan.clawback_points,
            PointReason::RefundClawback,
            Some(order.id),
        )
        .await?;
    }

    // 5. Restock the decremented product lines (already filtered by the plan).
    let restocks: Vec<(Uuid, i32)> = plan
        .restocks
        .iter()
        .map(|r| (r.product_id, r.quantity))
        .collect();
    product_service::restore_stock_tx(tx, &restocks).await?;

    // 6. Cancel the order's enrolments + subscriptions (order-scoped,
    //    idempotent).
    enrolments_service::cancel_by_order_tx(tx, order.id).await?;
    subscriptions_service::cancel_by_order_tx(tx, order.id).await?;

    Ok(())
}
