//! 鎖序圖 (Lock Order Graph) — the single code anchor for every multi-table
//! row-lock order in the money/stock/seat paths. Other modules' docs point
//! here instead of restating it:
//!
//! ```text
//! checkout   users (FOR UPDATE)
//!              -> cart target ids (read, no lock)
//!              -> products (FOR NO KEY UPDATE, asc)
//!              -> courses (FOR UPDATE, asc)
//!              -> cart_items (FOR UPDATE OF ci, snapshot read)
//!              -> enrolments (INSERT) -> subscriptions (INSERT)
//! refund     orders (FOR UPDATE)
//!              -> users (FOR UPDATE)
//!              -> products (FOR NO KEY UPDATE, asc)
//!              -> enrolments (UPDATE by order_id)
//!              -> leave_requests (UPDATE, pending only)
//!              -> subscriptions (UPDATE by order_id)
//! redeem     rewards (FOR UPDATE) -> users (FOR UPDATE, via try_spend_tx)
//! ```
//!
//! `checkout` and `refund` agree on `users` before `products`, and on
//! `products` ascending, so they cannot form a cycle. `refund`'s `orders`
//! lock precedes `users`, but checkout only INSERTs its own new `orders`
//! row (nobody else can hold it yet), so no path holds `users` and then waits
//! on an `orders` row someone else holds.
//!
//! `rewards::redeem` is `rewards` -> `users`. This is safe only because NO
//! path holds `users` and then locks `rewards`: checkout and refund never
//! touch `rewards`, and `redeem` itself takes `rewards` first. A new path
//! that locks `users` before `rewards` would close a cycle with `redeem` —
//! add it here first.
//!
//! Two seat paths live outside this graph; neither can deadlock today.
//! Venue booking: create takes `time_slots` (FOR NO KEY UPDATE) then INSERTs
//! `bookings`; cancel takes `bookings` (FOR UPDATE) then `time_slots` (FOR
//! SHARE, non-admin only). Makeup booking: `leave_requests` (FOR UPDATE) then
//! `course_sessions` (FOR UPDATE). Each touches a disjoint table set from the
//! graph above, and create's `bookings` row is brand new, so no one else can
//! hold it.
//!
//! Where each step lives: [`acquire_checkout_locks`] runs checkout's first
//! three locks, and the `cart_items` lock is the `FOR UPDATE OF ci` in
//! `cart::service::find_cart_items_for_checkout_tx` (taken right after, and
//! before any write). [`acquire_refund_locks`] runs users + products; the
//! `orders` lock is `orders::repository::find_by_id_tx` in
//! `update_order_status`; the enrolments -> leave_requests -> subscriptions
//! tail is `orders::service::compensate_order_artifacts_tx` (the enrolments
//! owner cancels the pending leave requests of the enrolments it flipped).
//!
//! 訂單鎖協定 (Order Lock Protocol) — every row lock `checkout` takes
//! before it writes, acquired in one place, in one fixed order:
//!
//! ```text
//! users (FOR UPDATE)                     points::service::lock_balance_tx
//!   -> read the cart's target ids        cart::service::find_checkout_targets_tx
//!   -> products (FOR NO KEY UPDATE, asc) products::service::lock_products_tx
//!   -> courses (FOR UPDATE, asc)         courses::seats::lock_courses_tx
//! ```
//!
//! [`acquire_checkout_locks`] runs the four steps and hands back
//! [`CheckoutLocks`], which carries the three witnesses (`BalanceLock`,
//! `ProductLocks`, `CourseLocks`). The witnesses live with the tables' owners
//! (points, products, courses); this module only fixes the order. Every later
//! checkout write goes through a witness: `reserve_stock_tx` takes
//! `&ProductLocks`, `enrol_batch_from_purchase_tx` takes `&CourseLocks`, and
//! each walks its lines via the witness's `in_lock_order` — which is also why
//! a multi-line 409 names the line with the smallest id. A line the witness
//! doesn't cover is `AppError::Internal`, not a silent unlocked write.
//! `ProductLocks` also carries the rows read under the lock (`SELECT *`), so
//! `reserve_stock_tx` judges every line's quantity
//! (`Product::ensure_line_quantity`) before its first decrement without a
//! second read — no new lock, no new edge.
//!
//! Cart add (`cart::service::add_product_item`) runs its upsert in a
//! transaction so the merged quantity can be judged before commit. It is
//! one statement — the `cart_items` row (or unique-index slot) plus the FK
//! `FOR KEY SHARE` on `users`/`products` that the plain upsert always took —
//! so it adds no edge to the graph above either.
//!
//! Refund/cancel compensation runs the refund half, [`acquire_refund_locks`]
//! → [`RefundLocks`]: users (`FOR UPDATE`, `lock_balance_tx`) → the products
//! the order will restock (`FOR NO KEY UPDATE`, asc,
//! `products::service::lock_restock_for_order_tx`). No courses — refund
//! never writes a seat count. Its writes take the witnesses the same way:
//! `points::service::reverse_order_tx` takes `&BalanceLock`,
//! `products::service::restore_for_order_tx` takes `&RestockLocks` — the
//! lock plus the lines to restore, decided once when the lock was taken
//! (the trace is read there and not again), so what is restocked is exactly
//! what is locked.
//!
//! Each lock is taken at the strength its later write needs (`FOR NO KEY
//! UPDATE` for the stock UPDATE, `FOR UPDATE` for the seat count), so no
//! lock is ever upgraded: two buyers of one product/course queue at the lock
//! instead of deadlocking on a SHARE→UPDATE upgrade.
//!
//! Reading the cart's ids takes no lock of its own: `cart_items.user_id` is
//! an FK into `users`, so adding a cart line needs `FOR KEY SHARE` on the
//! `users` row, which the step-one `FOR UPDATE` blocks until commit
//! (`checkout_locks_block_same_user_cart_insert_until_commit`). The cart can
//! only shrink before the snapshot read, so the snapshot's lines are always
//! covered by the witnesses.
//!
//! Same-buyer dimension: `users` first, before the cart read, unconditionally
//! — full argument on `BalanceLock` (`points::service`).
//!
//! Cross-buyer dimension (single code anchor for this argument; prose
//! authority remains ADR-0007 決策 5): the users-first lock only serializes
//! the SAME buyer's checkout vs refund — two different buyers hold two
//! different `users` rows, so it does nothing for them. That gap is closed by
//! a *global* ascending order on `products` (and on `courses`) at every site
//! that ever locks those rows, so no two transactions can hold locks on the
//! same pair of rows in opposite orders, regardless of which buyers or paths
//! are involved:
//!   1. checkout's lock step — `lock_products_tx` / `lock_courses_tx`
//!      (ascending, UPDATE-strength), inside [`acquire_checkout_locks`]
//!   2. checkout's writes — `reserve_stock_tx` / `enrol_batch_from_purchase_tx`,
//!      in witness order (`in_lock_order`) on rows already held
//!   3. refund's lock step and restore — [`acquire_refund_locks`] locks the
//!      order's restock products via `lock_restock_for_order_tx` (ascending,
//!      through `lock_products_tx`), and `restore_for_order_tx` writes them in
//!      the `RestockLocks` witness's order (`in_lock_order`)
//!
//! Regression tests: `checkout_locks_take_products_ascending_no_cross_buyer_deadlock`,
//! `checkout_same_product_two_buyers_queue_instead_of_deadlocking`,
//! `checkout_same_course_two_buyers_queue_instead_of_deadlocking`.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::modules::cart::service as cart_service;
use crate::modules::courses::seats::{self, CourseLocks};
use crate::modules::points::service::{self as points_service, BalanceLock};
use crate::modules::products::service::{self as product_service, ProductLocks, RestockLocks};

use super::model::Order;

/// Every lock `checkout` holds before its first write — see the module doc.
/// Fields are private; only [`acquire_checkout_locks`] builds one.
#[derive(Debug)]
pub struct CheckoutLocks {
    balance: BalanceLock,
    products: ProductLocks,
    courses: CourseLocks,
}

impl CheckoutLocks {
    pub fn balance(&self) -> &BalanceLock {
        &self.balance
    }

    pub fn products(&self) -> &ProductLocks {
        &self.products
    }

    pub fn courses(&self) -> &CourseLocks {
        &self.courses
    }
}

/// Run the order lock protocol inside the caller's transaction: users →
/// cart target ids → products ascending → courses ascending. The only error
/// it can raise besides a database error is `lock_balance_tx`'s 404 "user
/// not found".
pub async fn acquire_checkout_locks(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<CheckoutLocks, AppError> {
    let balance = points_service::lock_balance_tx(tx, user_id).await?;
    let targets = cart_service::find_checkout_targets_tx(tx, &balance).await?;
    let products = product_service::lock_products_tx(tx, &targets.product_ids).await?;
    let courses = seats::lock_courses_tx(tx, &targets.course_ids).await?;

    Ok(CheckoutLocks {
        balance,
        products,
        courses,
    })
}

/// Every lock refund/cancel compensation holds before its first write —
/// see the module doc. Refund locks no courses (it never touches seat
/// counts; enrolments/subscriptions are cancelled by `order_id`). Fields are
/// private; only [`acquire_refund_locks`] builds one.
#[derive(Debug)]
pub struct RefundLocks {
    balance: BalanceLock,
    restock: RestockLocks,
}

impl RefundLocks {
    pub fn balance(&self) -> &BalanceLock {
        &self.balance
    }

    pub fn restock(&self) -> &RestockLocks {
        &self.restock
    }
}

/// Run the refund half of the order lock protocol inside the caller's
/// transaction (which already holds the `orders` row `FOR UPDATE`): users
/// (unconditionally, even for a zero-points order) → the products the order
/// will restock, ascending. Errors besides a database error:
/// `lock_balance_tx`'s 404 "user not found".
pub async fn acquire_refund_locks(
    tx: &mut Transaction<'_, Postgres>,
    order: &Order,
) -> Result<RefundLocks, AppError> {
    let balance = points_service::lock_balance_tx(tx, order.user_id).await?;
    let restock = product_service::lock_restock_for_order_tx(tx, order.id).await?;

    Ok(RefundLocks { balance, restock })
}
