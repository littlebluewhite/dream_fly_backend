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
         RETURNING *",
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
         total_sessions, remaining_sessions, price_cents, created_at, updated_at \
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
                s.expires_at, s.total_sessions, s.remaining_sessions, s.price_cents \
         FROM subscriptions s \
         JOIN products p ON p.id = s.product_id \
         WHERE s.user_id = $1 \
         ORDER BY s.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
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
pub async fn redeem_one_session(db: &PgPool, id: Uuid) -> Result<Option<Subscription>, sqlx::Error> {
    sqlx::query_as::<_, Subscription>(
        "UPDATE subscriptions SET remaining_sessions = remaining_sessions - 1 \
         WHERE id = $1 AND status = 'active' AND remaining_sessions > 0 \
           AND (expires_at IS NULL OR expires_at > NOW()) \
         RETURNING *",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}
