use std::collections::HashMap;

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;
use crate::utils::slug::slugify;

use super::dto::{
    CreateProductRequest, ProductListResponse, ProductResponse, UpdateProductRequest,
};
use super::model::{Product, ProductType};
use super::repository::{self, ProductCreate, ProductUpdate};

/// Attach the `sold` aggregate to a single product. Used by the
/// single-row endpoints (`get_by_slug`, `get_by_id`, `create`, `update`);
/// `list` batches `find_sold_counts` across the whole page instead, to
/// avoid one query per row.
async fn to_response(db: &PgPool, product: Product) -> Result<ProductResponse, AppError> {
    let sold_map = repository::find_sold_counts(db, &[product.id]).await?;
    let sold = sold_map.get(&product.id).copied().unwrap_or(0);
    Ok(ProductResponse::from_product(product, sold))
}

pub async fn list(
    db: &PgPool,
    product_type_filter: Option<&str>,
    pagination: &PaginationParams,
) -> Result<ProductListResponse, AppError> {
    // Count first so a zero-total response doesn't need a second (empty)
    // result set; both queries share the same filter.
    let total = repository::count_active(db, product_type_filter).await?;
    let products = repository::find_all_active(
        db,
        product_type_filter,
        pagination.limit(),
        pagination.offset(),
    )
    .await?;

    // One batched aggregate for every product on the page — not one query
    // per row (see `repository::find_sold_counts`'s doc comment).
    let product_ids: Vec<Uuid> = products.iter().map(|p| p.id).collect();
    let sold_map = repository::find_sold_counts(db, &product_ids).await?;

    Ok(ProductListResponse {
        products: products
            .into_iter()
            .map(|p| {
                let sold = sold_map.get(&p.id).copied().unwrap_or(0);
                ProductResponse::from_product(p, sold)
            })
            .collect(),
        meta: pagination.meta(total),
    })
}

pub async fn get_by_slug(db: &PgPool, slug: &str) -> Result<ProductResponse, AppError> {
    let product = repository::find_by_slug(db, slug)
        .await?
        .ok_or_else(|| AppError::NotFound("product not found".into()))?;
    to_response(db, product).await
}

pub async fn get_by_id(db: &PgPool, id: Uuid) -> Result<ProductResponse, AppError> {
    let product = repository::find_by_id(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("product not found".into()))?;
    to_response(db, product).await
}

/// Public-facing lookup — only returns an active product. Template:
/// `posts::service::get_published_by_slug_or_id`.
pub async fn get_active_by_slug(db: &PgPool, slug: &str) -> Result<ProductResponse, AppError> {
    let product = repository::find_active_by_slug(db, slug)
        .await?
        .ok_or_else(|| AppError::NotFound("product not found".into()))?;
    to_response(db, product).await
}

/// Public-facing lookup — only returns an active product. Template:
/// `posts::service::get_published_by_slug_or_id`.
pub async fn get_active_by_id(db: &PgPool, id: Uuid) -> Result<ProductResponse, AppError> {
    let product = repository::find_active_by_id(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("product not found".into()))?;
    to_response(db, product).await
}

pub async fn create(db: &PgPool, req: CreateProductRequest) -> Result<ProductResponse, AppError> {
    let slug = req.slug.unwrap_or_else(|| slugify(&req.name));

    // Validate product_type
    let pt = &req.product_type;
    pt.parse::<ProductType>()
        .map_err(|_| AppError::Validation(format!("invalid product_type: {}", pt)))?;

    // Rely on the DB unique index for slug uniqueness — avoids TOCTOU race
    // between a SELECT check and the INSERT.
    let product = repository::create(
        db,
        ProductCreate {
            name: &req.name,
            slug: &slug,
            product_type: pt,
            description: req.description.as_deref(),
            price_cents: req.price_cents,
            original_price_cents: req.original_price_cents,
            features: &req.features,
            is_highlighted: req.is_highlighted,
            badge: req.badge.as_deref(),
            stock: req.stock,
            valid_days: req.valid_days,
            session_count: req.session_count,
        },
    )
    .await
    .map_err(|e| AppError::conflict_on_unique(e, format!("slug '{}' already exists", slug)))?;

    to_response(db, product).await
}

/// `PATCH /products/{id}` — admin only. Enforced by the `admin_api`
/// route_layer (see `startup.rs`). Slug uniqueness is enforced by the DB's
/// `uq_products_slug_lower` functional index; a violation surfaces as
/// `sqlx::Error::Database` here and is translated to 409 — same idiom as
/// `venues::service::update_venue` (see its comment for why the constraint
/// name is matched explicitly).
pub async fn update(
    db: &PgPool,
    id: Uuid,
    req: UpdateProductRequest,
) -> Result<ProductResponse, AppError> {
    // Validate product_type if provided
    if let Some(ref pt) = req.product_type {
        pt.parse::<ProductType>()
            .map_err(|_| AppError::Validation(format!("invalid product_type: {}", pt)))?;
    }

    let product = repository::update(
        db,
        id,
        ProductUpdate {
            name: req.name.as_deref(),
            slug: req.slug.as_deref(),
            product_type: req.product_type.as_deref(),
            description: req.description.as_deref(),
            price_cents: req.price_cents,
            original_price_cents: req.original_price_cents,
            features: req.features.as_deref(),
            is_highlighted: req.is_highlighted,
            badge: req.badge.as_ref().map(|o| o.as_deref()),
            stock: req.stock,
            valid_days: req.valid_days,
            session_count: req.session_count,
            is_active: req.is_active,
        },
    )
    .await
    .map_err(|e| {
        AppError::conflict_on_constraint(
            e,
            "uq_products_slug_lower",
            format!(
                "slug '{}' already exists",
                req.slug.as_deref().unwrap_or_default()
            ),
        )
    })?
    .ok_or_else(|| AppError::NotFound("product not found".into()))?;

    to_response(db, product).await
}

/// Witness that `lock_products_tx` has taken `FOR NO KEY UPDATE` on these
/// `products` rows inside the caller's still-open transaction — the
/// products stage of the order lock protocol (`orders::locks`). Fields are
/// private; only `lock_products_tx` can construct one (same private-field
/// witness technique as `courses::seats`'s `SessionLock`).
///
/// `ids` are the locked rows' ids, ascending and deduplicated — the order
/// the locks were taken in (PostgreSQL orders `uuid` bytewise, the same as
/// `Uuid`'s `Ord`). [`ProductLocks::in_lock_order`] is the single
/// owner of "walk these lines in lock order": a write-lock owner
/// (`reserve_stock_tx`) no longer sorts on its own.
#[derive(Debug)]
pub struct ProductLocks {
    ids: Vec<Uuid>,
}

impl ProductLocks {
    pub fn ids(&self) -> &[Uuid] {
        &self.ids
    }

    /// Reorder `items` into this witness's lock order (ascending product
    /// id). An item whose id this witness does not cover maps to
    /// `AppError::Internal` — writing a row the caller never locked is a
    /// protocol bug, not a business rejection.
    pub fn in_lock_order<T>(
        &self,
        items: Vec<T>,
        id: impl Fn(&T) -> Uuid,
    ) -> Result<Vec<T>, AppError> {
        if let Some(item) = items.iter().find(|item| !self.covers(id(item))) {
            let product_id = id(item);
            return Err(AppError::Internal(anyhow::anyhow!(
                "product {product_id} is not covered by ProductLocks"
            )));
        }
        let mut items = items;
        items.sort_by_key(|item| id(item));
        Ok(items)
    }

    fn covers(&self, product_id: Uuid) -> bool {
        self.ids.binary_search(&product_id).is_ok()
    }
}

/// Lock the given `products` rows `FOR NO KEY UPDATE`, ascending by id
/// (`ORDER BY id`), inside the caller's transaction and return the
/// [`ProductLocks`] witness. `FOR NO KEY UPDATE` is exactly the strength
/// the later stock UPDATE needs, so no upgrade happens afterwards (two
/// buyers of one product queue here instead of deadlocking on a
/// SHARE→UPDATE upgrade), and it does not block the `FOR KEY SHARE` that FK
/// checks take (`order_items`/`cart_items` inserts). Ids that don't resolve
/// are simply not in the witness. Global ascending order rationale: the
/// "Cross-buyer dimension" anchor in `orders::locks` (ADR-0007 決策 5).
pub async fn lock_products_tx(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<ProductLocks, AppError> {
    let ids = repository::lock_products_tx(tx, ids).await?;
    Ok(ProductLocks { ids })
}

/// Reserve stock for a batch of product lines inside the caller's
/// transaction — `orders::service::checkout`'s stock decrement. `lines` is
/// `(product_id, quantity, name)` tuples rather than
/// `cart::model::CheckoutLine` so this module
/// doesn't have to import back into `cart`; the `HashMap` return follows
/// the same idiom as `repository::find_sold_counts` in this module.
///
/// Takes `&ProductLocks`: every line's row must already be locked by
/// `lock_products_tx` (a line outside the witness is `AppError::Internal`),
/// and the lines are walked in the witness's lock order
/// ([`ProductLocks::in_lock_order`], ascending `product_id`) — ordering
/// rationale: the "Cross-buyer dimension" anchor in `orders::locks`
/// (ADR-0007 決策 5).
///
/// Each line is then decremented in that order via
/// `try_decrement_stock_tx`. The first line (in lock order) whose stock is
/// insufficient fails the whole reservation with
/// `AppError::Conflict("insufficient stock for product {name}")` — when
/// more than one line is short, this is whichever has the smallest
/// `product_id`, not necessarily the first element of the input slice.
/// Nothing is rolled back here; on error the caller's transaction is left
/// for the caller to roll back (or simply not commit), same contract as
/// every other `_tx` function in this codebase.
///
/// On success, returns every reserved row keyed by `product_id`. Each row
/// comes straight from `try_decrement_stock_tx`'s `RETURNING *` — already
/// locked by this transaction — so a caller that needs the
/// row afterward (checkout's subscription-grant step) reads it out of this
/// map instead of re-reading it from the database. An empty `lines` is a
/// no-op that returns an empty map without touching the database.
pub async fn reserve_stock_tx(
    tx: &mut Transaction<'_, Postgres>,
    locks: &ProductLocks,
    lines: &[(Uuid, i32, &str)],
) -> Result<HashMap<Uuid, Product>, AppError> {
    let ordered = locks.in_lock_order(lines.to_vec(), |(product_id, _, _)| *product_id)?;

    let mut reserved = HashMap::with_capacity(ordered.len());
    for (product_id, quantity, name) in ordered {
        let product = repository::try_decrement_stock_tx(tx, product_id, quantity)
            .await?
            .ok_or_else(|| AppError::Conflict(format!("insufficient stock for product {name}")))?;
        reserved.insert(product_id, product);
    }

    Ok(reserved)
}

/// Reverse a batch of stock reservations inside the caller's transaction —
/// refund/cancel compensation's (`orders::service::compensate_order_artifacts_tx`)
/// mirror of `reserve_stock_tx`.
/// Sorts by `product_id` ascending before touching any row — the same
/// ascending order `lock_products_tx` locks in (refund currently calls
/// `lock_products_tx` right before this as a transitional step). Deadlock
/// rationale: see the "Cross-buyer dimension" anchor in `orders::locks`
/// (ADR-0007 決策 5).
///
/// Contract: callers must pass only `(product_id, quantity)` pairs whose
/// `order_items.stock_decremented` was `true` at checkout time —
/// `refund::plan_refund` is what produces that already-filtered
/// list. This function does not itself check the flag (it has no access to
/// `order_items` at all); handing it an unfiltered `lines` would restore
/// stock into a product that was never actually decremented.
///
/// A product id that doesn't resolve maps to `AppError::Internal` — every
/// caller's `lines` come from `order_items.product_id`, an FK into
/// `products`, so a miss here signals a data-integrity bug, not a
/// legitimate business-rule rejection.
pub async fn restore_stock_tx(
    tx: &mut Transaction<'_, Postgres>,
    lines: &[(Uuid, i32)],
) -> Result<(), AppError> {
    let mut sorted = lines.to_vec();
    sorted.sort_by_key(|(product_id, _)| *product_id);

    for (product_id, quantity) in sorted {
        repository::restore_stock_tx(tx, product_id, quantity)
            .await?
            .ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "restore_stock_tx: product {product_id} not found"
                ))
            })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (Uuid, Uuid, Uuid) {
        let mut v = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
        v.sort();
        (v[0], v[1], v[2])
    }

    #[test]
    fn in_lock_order_sorts_items_ascending_by_id() {
        let (low, mid, high) = ids();
        let locks = ProductLocks {
            ids: vec![low, mid, high],
        };
        let ordered = locks
            .in_lock_order(vec![(high, "h"), (low, "l"), (mid, "m")], |(id, _)| *id)
            .expect("all covered");
        assert_eq!(ordered, vec![(low, "l"), (mid, "m"), (high, "h")]);
    }

    #[test]
    fn in_lock_order_accepts_a_subset_of_the_witness() {
        let (low, mid, high) = ids();
        let locks = ProductLocks {
            ids: vec![low, mid, high],
        };
        let ordered = locks
            .in_lock_order(vec![high, low], |id| *id)
            .expect("subset is covered");
        assert_eq!(ordered, vec![low, high]);
    }

    #[test]
    fn in_lock_order_rejects_an_id_outside_the_witness_as_internal() {
        let (low, mid, high) = ids();
        let locks = ProductLocks {
            ids: vec![low, high],
        };
        let err = locks
            .in_lock_order(vec![low, mid], |id| *id)
            .expect_err("mid was never locked");
        assert!(matches!(err, AppError::Internal(_)), "got {err:?}");
    }
}
