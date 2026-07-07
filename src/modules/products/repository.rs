use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use super::model::Product;

/// Input payload for `create`. Packages the 10 fields that previously formed
/// a too-large positional argument list.
pub struct ProductCreate<'a> {
    pub name: &'a str,
    pub slug: &'a str,
    pub product_type: &'a str,
    pub description: Option<&'a str>,
    pub price_cents: i64,
    pub original_price_cents: Option<i64>,
    pub features: &'a [String],
    pub is_highlighted: bool,
    pub badge: Option<&'a str>,
    pub stock: Option<i32>,
    pub valid_days: Option<i32>,
    pub session_count: Option<i32>,
}

/// Input payload for `update`. Every field is `Option` because this is a
/// partial (PATCH-style) update. Nullable DB columns use `Option<Option<T>>`
/// so callers can distinguish "don't touch" (`None`) from "set to NULL"
/// (`Some(None)`) from "set to value" (`Some(Some(v))`).
#[derive(Default)]
pub struct ProductUpdate<'a> {
    pub name: Option<&'a str>,
    pub slug: Option<&'a str>,
    pub product_type: Option<&'a str>,
    pub description: Option<&'a str>,
    pub price_cents: Option<i64>,
    pub original_price_cents: Option<Option<i64>>,
    pub features: Option<&'a [String]>,
    pub is_highlighted: Option<bool>,
    pub badge: Option<Option<&'a str>>,
    pub stock: Option<Option<i32>>,
    pub valid_days: Option<Option<i32>>,
    pub session_count: Option<Option<i32>>,
    pub is_active: Option<bool>,
}

pub async fn find_all_active(
    db: &PgPool,
    product_type_filter: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<Vec<Product>, sqlx::Error> {
    sqlx::query_as::<_, Product>(
        "SELECT id, name, slug, product_type, description, price_cents, \
         original_price_cents, features, is_highlighted, badge, stock, \
         valid_days, session_count, is_active, created_at, updated_at \
         FROM products \
         WHERE is_active = true \
           AND ($1::text IS NULL OR product_type = $1::product_type) \
         ORDER BY name \
         LIMIT $2 OFFSET $3",
    )
    .bind(product_type_filter)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

/// Count active products for pagination totals. Kept next to
/// [`find_all_active`] so the two queries stay filter-aligned.
pub async fn count_active(
    db: &PgPool,
    product_type_filter: Option<&str>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM products \
         WHERE is_active = true \
           AND ($1::text IS NULL OR product_type = $1::product_type)",
    )
    .bind(product_type_filter)
    .fetch_one(db)
    .await
}

pub async fn find_by_slug(db: &PgPool, slug: &str) -> Result<Option<Product>, sqlx::Error> {
    sqlx::query_as::<_, Product>(
        "SELECT id, name, slug, product_type, description, price_cents, \
         original_price_cents, features, is_highlighted, badge, stock, \
         valid_days, session_count, is_active, created_at, updated_at \
         FROM products WHERE LOWER(slug) = LOWER($1)",
    )
    .bind(slug)
    .fetch_optional(db)
    .await
}

pub async fn find_by_id(db: &PgPool, id: Uuid) -> Result<Option<Product>, sqlx::Error> {
    sqlx::query_as::<_, Product>(
        "SELECT id, name, slug, product_type, description, price_cents, \
         original_price_cents, features, is_highlighted, badge, stock, \
         valid_days, session_count, is_active, created_at, updated_at \
         FROM products WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Transactional counterpart of [`find_by_id`], consumed by the checkout
/// flow (Task 9) to fetch the full `Product` row for subscription-eligible
/// cart lines inside the checkout transaction (`grant_from_purchase_tx`
/// needs the whole row, not just price/quantity).
pub async fn find_by_id_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<Product>, sqlx::Error> {
    sqlx::query_as::<_, Product>(
        "SELECT id, name, slug, product_type, description, price_cents, \
         original_price_cents, features, is_highlighted, badge, stock, \
         valid_days, session_count, is_active, created_at, updated_at \
         FROM products WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Attempts to decrement `stock` by `quantity` atomically.
///
/// Returns:
/// - `Ok(Some(remaining))` on success (None-stock products leave stock NULL untouched)
/// - `Ok(None)` if the product has finite stock and it is insufficient
/// - `Err(...)` on database error
///
/// Products with `stock IS NULL` are treated as unlimited inventory
/// (tickets/memberships/course packages typically have no stock cap).
pub async fn try_decrement_stock_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    product_id: Uuid,
    quantity: i32,
) -> Result<Option<Option<i32>>, sqlx::Error> {
    let row: Option<(Option<i32>,)> = sqlx::query_as::<_, (Option<i32>,)>(
        "UPDATE products \
         SET stock = CASE WHEN stock IS NULL THEN NULL ELSE stock - $2 END \
         WHERE id = $1 \
           AND (stock IS NULL OR stock >= $2) \
         RETURNING stock",
    )
    .bind(product_id)
    .bind(quantity)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row.map(|(s,)| s))
}

/// Sum of `order_items.quantity` per product across "paid-class" orders
/// (`paid`/`processing`/`completed` — a `pending`/`cancelled`/`refunded`
/// order never counts toward sold units), computed in one GROUP BY query
/// for the whole batch of `product_ids`. Callers listing multiple products
/// must pass every id in a single call rather than looping one-at-a-time —
/// that would reintroduce the N+1 this exists to avoid. A product id absent
/// from the returned map has zero sold units.
/// This status list is a semantic twin of `orders::model::REVENUE_STATUSES`
/// (used by reports for revenue aggregation) — currently identical, kept separate
/// on purpose because "sold units" and "revenue" are distinct domain concepts;
/// a change to either side must be reconciled deliberately.
pub async fn find_sold_counts(
    db: &PgPool,
    product_ids: &[Uuid],
) -> Result<HashMap<Uuid, i64>, sqlx::Error> {
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT oi.product_id, SUM(oi.quantity)::bigint AS sold \
         FROM order_items oi \
         JOIN orders o ON o.id = oi.order_id \
         WHERE oi.product_id = ANY($1) \
           AND o.status IN ('paid'::order_status, 'processing'::order_status, 'completed'::order_status) \
         GROUP BY oi.product_id",
    )
    .bind(product_ids)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().collect())
}

pub async fn create(db: &PgPool, input: ProductCreate<'_>) -> Result<Product, sqlx::Error> {
    sqlx::query_as::<_, Product>(
        "INSERT INTO products (id, name, slug, product_type, description, price_cents, \
         original_price_cents, features, is_highlighted, badge, stock, valid_days, session_count, \
         is_active, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3::product_type, $4, $5, $6, $7, $8, $9, $10, $11, $12, \
         true, NOW(), NOW()) \
         RETURNING *",
    )
    .bind(input.name)
    .bind(input.slug)
    .bind(input.product_type)
    .bind(input.description)
    .bind(input.price_cents)
    .bind(input.original_price_cents)
    .bind(input.features)
    .bind(input.is_highlighted)
    .bind(input.badge)
    .bind(input.stock)
    .bind(input.valid_days)
    .bind(input.session_count)
    .fetch_one(db)
    .await
}

pub async fn update(
    db: &PgPool,
    id: Uuid,
    input: ProductUpdate<'_>,
) -> Result<Option<Product>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE products SET updated_at = now()");

    if let Some(v) = input.name {
        qb.push(", name = ").push_bind(v);
    }
    if let Some(v) = input.slug {
        qb.push(", slug = ").push_bind(v);
    }
    if let Some(v) = input.product_type {
        qb.push(", product_type = ").push_bind(v).push("::product_type");
    }
    if let Some(v) = input.description {
        qb.push(", description = ").push_bind(v);
    }
    if let Some(v) = input.price_cents {
        qb.push(", price_cents = ").push_bind(v);
    }
    if let Some(v) = input.original_price_cents {
        qb.push(", original_price_cents = ").push_bind(v);
    }
    if let Some(v) = input.features {
        qb.push(", features = ").push_bind(v);
    }
    if let Some(v) = input.is_highlighted {
        qb.push(", is_highlighted = ").push_bind(v);
    }
    if let Some(v) = input.badge {
        qb.push(", badge = ").push_bind(v);
    }
    if let Some(v) = input.stock {
        qb.push(", stock = ").push_bind(v);
    }
    if let Some(v) = input.valid_days {
        qb.push(", valid_days = ").push_bind(v);
    }
    if let Some(v) = input.session_count {
        qb.push(", session_count = ").push_bind(v);
    }
    if let Some(v) = input.is_active {
        qb.push(", is_active = ").push_bind(v);
    }

    qb.push(" WHERE id = ").push_bind(id);
    qb.push(" RETURNING *");

    qb.build_query_as::<Product>().fetch_optional(db).await
}
