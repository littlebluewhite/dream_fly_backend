use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::Coupon;

/// Normalize a coupon code the same way on every read and write path: trim
/// surrounding whitespace, then uppercase. Centralized here so `create`,
/// `find_valid_by_code`, and `find_valid_by_code_tx` can never drift out of
/// sync on what "the same code" means.
fn normalize_code(code: &str) -> String {
    code.trim().to_uppercase()
}

pub async fn create(
    db: &PgPool,
    code: &str,
    discount_cents: i64,
    expires_at: Option<DateTime<Utc>>,
) -> Result<Coupon, sqlx::Error> {
    sqlx::query_as::<_, Coupon>(
        "INSERT INTO coupons (id, code, discount_cents, expires_at, created_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, now()) \
         RETURNING id, code, discount_cents, is_active, expires_at, created_at",
    )
    .bind(normalize_code(code))
    .bind(discount_cents)
    .bind(expires_at)
    .fetch_one(db)
    .await
}

/// Look up a coupon by code, applying the same "valid" rule the checkout
/// path uses: active, and either no expiry or not yet expired.
pub async fn find_valid_by_code(db: &PgPool, code: &str) -> Result<Option<Coupon>, sqlx::Error> {
    sqlx::query_as::<_, Coupon>(
        "SELECT id, code, discount_cents, is_active, expires_at, created_at \
         FROM coupons \
         WHERE code = $1 AND is_active = true AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(normalize_code(code))
    .fetch_optional(db)
    .await
}

/// Transactional counterpart of [`find_valid_by_code`], consumed by the
/// checkout flow (Task 9) which already holds an open transaction.
pub async fn find_valid_by_code_tx(
    tx: &mut Transaction<'_, Postgres>,
    code: &str,
) -> Result<Option<Coupon>, sqlx::Error> {
    sqlx::query_as::<_, Coupon>(
        "SELECT id, code, discount_cents, is_active, expires_at, created_at \
         FROM coupons \
         WHERE code = $1 AND is_active = true AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(normalize_code(code))
    .fetch_optional(&mut **tx)
    .await
}

pub async fn find_all(db: &PgPool, limit: u32, offset: u32) -> Result<Vec<Coupon>, sqlx::Error> {
    sqlx::query_as::<_, Coupon>(
        "SELECT id, code, discount_cents, is_active, expires_at, created_at \
         FROM coupons \
         ORDER BY created_at DESC \
         LIMIT $1 OFFSET $2",
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_all(db: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM coupons")
        .fetch_one(db)
        .await
}

/// Partial (PATCH-style) update — every argument optional. `code` is
/// intentionally not updatable here (see `UpdateCouponRequest`'s doc
/// comment). `expires_at` is `Option<Option<DateTime<Utc>>>` so callers can
/// distinguish "don't touch" (`None`) from "clear to permanently valid"
/// (`Some(None)`) from "set to a new expiry" (`Some(Some(v))`) — mirrors
/// `venues::repository::update`. Unlike that template, `coupons` has no
/// `updated_at` column to anchor the `SET` clause on, so `code = code` (a
/// no-op self-assignment of the one column this endpoint never touches)
/// plays that role instead, keeping the SQL valid even when every optional
/// argument is `None`. Returns `Ok(None)` if `id` doesn't match any row
/// (caller maps to 404).
pub async fn update(
    db: &PgPool,
    id: Uuid,
    discount_cents: Option<i64>,
    is_active: Option<bool>,
    expires_at: Option<Option<DateTime<Utc>>>,
) -> Result<Option<Coupon>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE coupons SET code = code");

    if let Some(v) = discount_cents {
        qb.push(", discount_cents = ").push_bind(v);
    }
    if let Some(v) = is_active {
        qb.push(", is_active = ").push_bind(v);
    }
    if let Some(v) = expires_at {
        qb.push(", expires_at = ").push_bind(v);
    }

    qb.push(" WHERE id = ").push_bind(id);
    qb.push(" RETURNING id, code, discount_cents, is_active, expires_at, created_at");

    qb.build_query_as::<Coupon>().fetch_optional(db).await
}

/// Hard delete. Safe because `orders.coupon_code` is a string snapshot with
/// no FK to this table — deleting a coupon can never orphan an order row.
/// Returns `true` if a row was deleted (caller maps `false` to 404).
pub async fn delete(db: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM coupons WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}
