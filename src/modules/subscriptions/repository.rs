use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{Subscription, SubscriptionWithProduct};

/// Insert a new subscription row inside the caller's transaction. `status`
/// is left to the column default (`'active'`).
#[allow(clippy::too_many_arguments)]
pub async fn insert_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    product_id: Uuid,
    order_id: Uuid,
    expires_at: Option<DateTime<Utc>>,
    total_sessions: Option<i32>,
    remaining_sessions: Option<i32>,
    price_cents: i64,
) -> Result<Subscription, sqlx::Error> {
    sqlx::query_as::<_, Subscription>(
        "INSERT INTO subscriptions \
         (id, user_id, product_id, order_id, expires_at, total_sessions, remaining_sessions, \
          price_cents, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW()) \
         RETURNING *, subscription_derived_status(status, expires_at, remaining_sessions) AS derived_status",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(product_id)
    .bind(order_id)
    .bind(expires_at)
    .bind(total_sessions)
    .bind(remaining_sessions)
    .bind(price_cents)
    .fetch_one(&mut **tx)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Subscription>, sqlx::Error> {
    sqlx::query_as::<_, Subscription>(
        "SELECT id, user_id, product_id, order_id, status, started_at, expires_at, \
         total_sessions, remaining_sessions, price_cents, created_at, updated_at, \
         subscription_derived_status(status, expires_at, remaining_sessions) AS derived_status \
         FROM subscriptions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// This user's subscriptions, newest first, joined with `products` for
/// `product_name`.
pub async fn find_by_user(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<SubscriptionWithProduct>, sqlx::Error> {
    sqlx::query_as::<_, SubscriptionWithProduct>(
        "SELECT s.id, s.product_id, p.name AS product_name, s.status, s.started_at, \
                s.expires_at, s.total_sessions, s.remaining_sessions, s.price_cents, \
                subscription_derived_status(s.status, s.expires_at, s.remaining_sessions) AS derived_status \
         FROM subscriptions s \
         JOIN products p ON p.id = s.product_id \
         WHERE s.user_id = $1 \
         ORDER BY s.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// This order's subscriptions JOINed with `products`, oldest first. Used by
/// `orders::service::fetch_artifacts` to assemble the checkout response
/// (fresh, replayed, or re-fetched via `GET /orders/{id}`) — distinct from
/// [`find_by_user`] above: filters by `order_id` instead of `user_id`, and
/// ASC order (checkout wants purchase order, not newest-first).
pub async fn find_by_order(
    db: &PgPool,
    order_id: Uuid,
) -> Result<Vec<SubscriptionWithProduct>, sqlx::Error> {
    sqlx::query_as::<_, SubscriptionWithProduct>(
        "SELECT s.id, s.product_id, p.name AS product_name, s.status, s.started_at, \
                s.expires_at, s.total_sessions, s.remaining_sessions, s.price_cents, \
                subscription_derived_status(s.status, s.expires_at, s.remaining_sessions) AS derived_status \
         FROM subscriptions s \
         JOIN products p ON p.id = s.product_id \
         WHERE s.order_id = $1 \
         ORDER BY s.created_at",
    )
    .bind(order_id)
    .fetch_all(db)
    .await
}

/// Cancel every non-cancelled subscription tied to `order_id` in one
/// UPDATE — refund/cancel compensation's (Step 10e) mirror of
/// `enrolments::repository::cancel_by_order_tx`. `cancelled` here is a real
/// persisted `status` value, unlike `expired`, which
/// `subscription_derived_status` derives at read time and never writes
/// back (ADR-0003) — so this is a genuine state transition, not a derived
/// read. `status <> 'cancelled'` makes it naturally idempotent (a
/// same-status retry, or a second compensation attempt, affects zero rows
/// instead of erroring). Returns the number of rows actually flipped.
pub async fn cancel_by_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE subscriptions SET status = 'cancelled'::subscription_status, updated_at = NOW() \
         WHERE order_id = $1 AND status <> 'cancelled'::subscription_status",
    )
    .bind(order_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected())
}

/// Product name for response assembly after a redeem.
/// `subscriptions.product_id` is a NOT NULL FK into `products` (which has no
/// cascading delete), so the row always exists; if that invariant somehow
/// breaks, `fetch_one`'s `RowNotFound` surfaces as a 500 — the appropriate
/// severity.
pub async fn product_name(db: &PgPool, product_id: Uuid) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT name FROM products WHERE id = $1")
        .bind(product_id)
        .fetch_one(db)
        .await
}

/// Atomically decrement one session. Returns `None` if the subscription
/// wasn't redeemable (not found, not active, no sessions left, or expired);
/// `service::redeem` re-reads the row to distinguish 404 from the specific
/// 409 reason.
///
/// Redeemability is `subscription_derived_status(...) = 'active'` — the same
/// SQL function every other subscription query reads its `derived_status`
/// column from — plus an extra `remaining_sessions > 0` guard: an unlimited
/// membership (`remaining_sessions IS NULL`) derives to `active` but still
/// has nothing to redeem a session from. There is no Rust-side twin of this
/// predicate anymore; the SQL function is the single implementation.
pub async fn redeem_one_session(db: &PgPool, id: Uuid) -> Result<Option<Subscription>, sqlx::Error> {
    sqlx::query_as::<_, Subscription>(
        "UPDATE subscriptions SET remaining_sessions = remaining_sessions - 1 \
         WHERE id = $1 \
           AND subscription_derived_status(status, expires_at, remaining_sessions) = 'active' \
           AND remaining_sessions > 0 \
         RETURNING *, subscription_derived_status(status, expires_at, remaining_sessions) AS derived_status",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}
