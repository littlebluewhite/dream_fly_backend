use std::collections::HashMap;

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;
use crate::modules::cart::model::LineTarget;
use crate::utils::slug::slugify;

use super::dto::{
    CreateProductRequest, ProductListResponse, ProductResponse, UpdateProductRequest,
};
use super::model::{OrderStockTrace, Product, ProductType};
use super::repository::{self, ProductCreate, ProductUpdate};

/// Parse+validate a `product_type` wire string into [`ProductType`] — the
/// single service-boundary parser `create`/`update`/`list` all share, so the
/// error message stays identical across the three call sites (this refactor
/// mustn't change it). Case-sensitive: [`ProductType`]'s `FromStr` doesn't
/// lowercase, matching `create_product_mixed_case_type_returns_422`.
fn parse_product_type(s: &str) -> Result<ProductType, AppError> {
    s.parse::<ProductType>()
        .map_err(|_| AppError::Validation(format!("invalid product_type: {}", s)))
}

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
    // Parsed once at the service boundary — an unparseable filter 422s here
    // instead of reaching the SQL `product_type` cast and failing as a 500.
    let product_type_filter = product_type_filter.map(parse_product_type).transpose()?;

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
    let product_type = parse_product_type(&req.product_type)?;

    // Rely on the DB unique index for slug uniqueness — avoids TOCTOU race
    // between a SELECT check and the INSERT.
    let product = repository::create(
        db,
        ProductCreate {
            name: &req.name,
            slug: &slug,
            product_type,
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
    let product_type = req
        .product_type
        .as_deref()
        .map(parse_product_type)
        .transpose()?;

    let product = repository::update(
        db,
        id,
        ProductUpdate {
            name: req.name.as_deref(),
            slug: req.slug.as_deref(),
            product_type,
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
/// `rows` are the locked rows as read under the lock, ascending by id and
/// deduplicated — the order the locks were taken in (PostgreSQL orders
/// `uuid` bytewise, the same as `Uuid`'s `Ord`). Nothing else can change a
/// held row before the caller's transaction ends, so `reserve_stock_tx`
/// judges each line's quantity against these rows without re-reading
/// ([`ProductLocks::rows_in_lock_order`] hands them out with the lines).
/// [`ProductLocks::in_lock_order`] is the single owner of "walk these lines
/// in lock order": a write-lock owner (`reserve_stock_tx`) no longer sorts
/// on its own.
#[derive(Debug)]
pub struct ProductLocks {
    rows: Vec<Product>,
}

impl ProductLocks {
    /// Reorder `items` into this witness's lock order (ascending product
    /// id). An item whose id this witness does not cover maps to
    /// `AppError::Internal` — writing a row the caller never locked is a
    /// protocol bug, not a business rejection.
    pub fn in_lock_order<T>(
        &self,
        items: Vec<T>,
        id: impl Fn(&T) -> Uuid,
    ) -> Result<Vec<T>, AppError> {
        Ok(self
            .rows_in_lock_order(items, id)?
            .into_iter()
            .map(|(_, item)| item)
            .collect())
    }

    /// [`Self::in_lock_order`], with each item paired with its locked row
    /// (as read under the lock) — for a caller that judges the row, so it
    /// never looks the row up a second time. Same ordering and same
    /// `Internal` for an uncovered id (the first one in input order).
    pub fn rows_in_lock_order<T>(
        &self,
        items: Vec<T>,
        id: impl Fn(&T) -> Uuid,
    ) -> Result<Vec<(&Product, T)>, AppError> {
        let mut pairs = Vec::with_capacity(items.len());
        for item in items {
            let product_id = id(&item);
            let row = self.row(product_id).ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "product {product_id} is not covered by ProductLocks"
                ))
            })?;
            pairs.push((row, item));
        }
        pairs.sort_by_key(|(row, _)| row.id);
        Ok(pairs)
    }

    /// The locked row for `product_id`, as read under the lock.
    fn row(&self, product_id: Uuid) -> Option<&Product> {
        self.rows
            .binary_search_by_key(&product_id, |row| row.id)
            .ok()
            .map(|i| &self.rows[i])
    }
}

/// Lock the given `products` rows `FOR NO KEY UPDATE`, ascending by id
/// (`ORDER BY id`), inside the caller's transaction and return the
/// [`ProductLocks`] witness, carrying the rows read under the lock. `FOR NO
/// KEY UPDATE` is exactly the strength the later stock UPDATE needs, so no
/// upgrade happens afterwards (two
/// buyers of one product queue here instead of deadlocking on a
/// SHARE→UPDATE upgrade), and it does not block the `FOR KEY SHARE` that FK
/// checks take (`order_items`/`cart_items` inserts). Ids that don't resolve
/// are simply not in the witness. Global ascending order rationale: the
/// "Cross-buyer dimension" anchor in `orders::locks` (ADR-0007 決策 5).
pub async fn lock_products_tx(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<ProductLocks, AppError> {
    let rows = repository::lock_products_tx(tx, ids).await?;
    Ok(ProductLocks { rows })
}

/// Reserve stock for a batch of product lines inside the caller's
/// transaction — `orders::service::checkout`'s stock decrement. `lines` is
/// `(product_id, quantity, name)` tuples rather than
/// `cart::model::CheckoutLine` so this module
/// doesn't depend on cart's checkout snapshot shape; the `HashMap` return follows
/// the same idiom as `repository::find_sold_counts` in this module.
///
/// Takes `&ProductLocks`: every line's row must already be locked by
/// `lock_products_tx` (a line outside the witness is `AppError::Internal`),
/// and the lines are walked in the witness's lock order
/// ([`ProductLocks::in_lock_order`], ascending `product_id`) — ordering
/// rationale: the "Cross-buyer dimension" anchor in `orders::locks`
/// (ADR-0007 決策 5).
///
/// Before the first decrement, every line's quantity is judged against its
/// locked row by `Product::ensure_line_quantity`, in lock order: `1..=999`
/// (400) and a time-based entitlement only at 1 (422). A cart line can only
/// get past that rule if it was written before the rule existed, but
/// checkout must not grant from it either way — and running it ahead of
/// every decrement is what puts the line-quantity 400/422 before the stock
/// 409 in `checkout`'s priority list.
///
/// Each line is then decremented in that order via
/// `try_decrement_stock_tx`, still the stock authority. The first line (in
/// lock order) whose stock is insufficient fails the whole reservation with
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
    let ordered = locks.rows_in_lock_order(lines.to_vec(), |(product_id, _, _)| *product_id)?;

    for (row, (_, quantity, _)) in &ordered {
        row.ensure_line_quantity(*quantity)?;
    }

    let mut reserved = HashMap::with_capacity(ordered.len());
    for (_, (product_id, quantity, name)) in ordered {
        let product = repository::try_decrement_stock_tx(tx, product_id, quantity)
            .await?
            .ok_or_else(|| AppError::Conflict(format!("insufficient stock for product {name}")))?;
        reserved.insert(product_id, product);
    }

    Ok(reserved)
}

/// The `(product_id, quantity)` lines an order's refund must restock, in
/// trace (line-creation) order: only product lines whose checkout-time
/// `stock_decremented` snapshot is `true` — `false` (unlimited-stock
/// product, or a legacy row) restores nothing, and a course line never
/// touches stock. Infallible: each trace's target was decoded at the row
/// boundary (`LineTarget`). Not sorted — walking in lock order is
/// [`ProductLocks::in_lock_order`]'s job.
fn restock_lines(traces: &[OrderStockTrace]) -> Vec<(Uuid, i32)> {
    let mut lines = Vec::new();
    for trace in traces {
        match trace.target {
            LineTarget::Product(product_id) => {
                if trace.stock_decremented {
                    lines.push((product_id, trace.quantity));
                }
            }
            LineTarget::Course(_) => {}
        }
    }
    lines
}

/// Witness that `lock_restock_for_order_tx` has locked the products an
/// order's refund will restock — the products stage of the refund half of the
/// order lock protocol (`orders::locks::acquire_refund_locks`) — and decided,
/// from the order's stock traces read once under that lock, which lines to
/// restore. Fields are private; only `lock_restock_for_order_tx` can
/// construct one, so the locked set and the lines `restore_for_order_tx`
/// writes cannot disagree: restore reads no trace of its own.
///
/// `lines` are in trace (line-creation) order, not sorted: walking them in
/// lock order is [`ProductLocks::in_lock_order`]'s job, done at restore.
#[derive(Debug)]
pub struct RestockLocks {
    locks: ProductLocks,
    lines: Vec<(Uuid, i32)>,
}

/// Lock the products an order's refund will restock — refund's products
/// stage (`orders::locks::acquire_refund_locks`), taken right after the
/// buyer's `users` row. Reads the order's own stock traces (`order_items`,
/// ADR-0007 決策 8) once, decides the lines to restock (`restock_lines`),
/// and locks their products via [`lock_products_tx`] (ascending, `FOR NO KEY
/// UPDATE`) — even when nothing needs restocking. Returns the lock together
/// with those lines as a [`RestockLocks`].
pub async fn lock_restock_for_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<RestockLocks, AppError> {
    let traces = repository::find_stock_traces_by_order_tx(tx, order_id).await?;
    let lines = restock_lines(&traces);
    let ids: Vec<Uuid> = lines.iter().map(|(product_id, _)| *product_id).collect();
    let locks = lock_products_tx(tx, &ids).await?;
    Ok(RestockLocks { locks, lines })
}

/// Undo an order's checkout stock decrement inside the caller's transaction —
/// refund/cancel compensation's (`orders::service::compensate_order_artifacts_tx`)
/// mirror of `reserve_stock_tx`. Restores each line `restock` decided at lock
/// time (no trace is read here), walked in the lock's order
/// ([`ProductLocks::in_lock_order`], ascending `product_id`) — ordering
/// rationale: the "Cross-buyer dimension" anchor in `orders::locks`
/// (ADR-0007 決策 5). A line outside the witness is `AppError::Internal`.
///
/// A product id that doesn't resolve maps to `AppError::Internal` — it was
/// never locked, so `in_lock_order` rejects it as uncovered (the
/// repository's `None` stays as a second belt). Every line comes from
/// `order_items.product_id`, an FK into `products`, so a miss signals a
/// data-integrity bug, not a legitimate business-rule rejection.
pub async fn restore_for_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    restock: &RestockLocks,
) -> Result<(), AppError> {
    let lines = restock
        .locks
        .in_lock_order(restock.lines.clone(), |(product_id, _)| *product_id)?;

    for (product_id, quantity) in lines {
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

    /// A witness over `ids` (must be ascending), with filler rows.
    fn locks_over(ids: &[Uuid]) -> ProductLocks {
        let rows = ids
            .iter()
            .map(|&id| Product {
                id,
                name: "Test Product".into(),
                slug: "test-product".into(),
                product_type: ProductType::Merchandise,
                description: None,
                price_cents: 1000,
                original_price_cents: None,
                features: vec![],
                is_highlighted: false,
                badge: None,
                stock: None,
                valid_days: None,
                session_count: None,
                is_active: true,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .collect();
        ProductLocks { rows }
    }

    fn ids() -> (Uuid, Uuid, Uuid) {
        let mut v = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
        v.sort();
        (v[0], v[1], v[2])
    }

    #[test]
    fn in_lock_order_sorts_items_ascending_by_id() {
        let (low, mid, high) = ids();
        let locks = locks_over(&[low, mid, high]);
        let ordered = locks
            .in_lock_order(vec![(high, "h"), (low, "l"), (mid, "m")], |(id, _)| *id)
            .expect("all covered");
        assert_eq!(ordered, vec![(low, "l"), (mid, "m"), (high, "h")]);
    }

    #[test]
    fn in_lock_order_accepts_a_subset_of_the_witness() {
        let (low, mid, high) = ids();
        let locks = locks_over(&[low, mid, high]);
        let ordered = locks
            .in_lock_order(vec![high, low], |id| *id)
            .expect("subset is covered");
        assert_eq!(ordered, vec![low, high]);
    }

    #[test]
    fn rows_in_lock_order_pairs_each_item_with_its_locked_row() {
        let (low, mid, high) = ids();
        let locks = locks_over(&[low, mid, high]);
        let pairs = locks
            .rows_in_lock_order(vec![(high, "h"), (low, "l")], |(id, _)| *id)
            .expect("all covered");
        let got: Vec<(Uuid, (Uuid, &str))> = pairs.into_iter().map(|(row, item)| (row.id, item)).collect();
        assert_eq!(got, vec![(low, (low, "l")), (high, (high, "h"))]);
    }

    #[test]
    fn in_lock_order_rejects_an_id_outside_the_witness_as_internal() {
        let (low, mid, high) = ids();
        let locks = locks_over(&[low, high]);
        let err = locks
            .in_lock_order(vec![low, mid], |id| *id)
            .expect_err("mid was never locked");
        assert!(matches!(err, AppError::Internal(_)), "got {err:?}");
    }

    fn trace(product_id: Uuid, quantity: i32, stock_decremented: bool) -> OrderStockTrace {
        OrderStockTrace {
            target: LineTarget::Product(product_id),
            quantity,
            stock_decremented,
        }
    }

    #[test]
    fn restock_lines_restocks_line_when_stock_was_decremented() {
        let product_id = Uuid::now_v7();
        let lines = restock_lines(&[trace(product_id, 3, true)]);
        assert_eq!(lines, vec![(product_id, 3)]);
    }

    #[test]
    fn restock_lines_skips_line_when_stock_not_decremented() {
        // Unlimited-stock product at checkout time (or a legacy row) — the
        // snapshot says nothing was actually decremented, so nothing gets
        // restored.
        let lines = restock_lines(&[trace(Uuid::now_v7(), 2, false)]);
        assert!(lines.is_empty(), "stock_decremented=false must not restock");
    }

    #[test]
    fn restock_lines_empty_traces_yield_no_lines() {
        let lines = restock_lines(&[]);
        assert!(lines.is_empty());
    }

    #[test]
    fn restock_lines_skips_course_lines() {
        // The trace query never returns course lines today; this pins the
        // defensive filter in `restock_lines` should that ever change.
        // A course line never touches stock — skipped even if its trace
        // claimed a decrement. Product lines around it keep their order.
        let (first, last) = (Uuid::now_v7(), Uuid::now_v7());
        let course = OrderStockTrace {
            target: LineTarget::Course(Uuid::now_v7()),
            quantity: 1,
            stock_decremented: true,
        };
        let lines = restock_lines(&[trace(first, 2, true), course, trace(last, 1, true)]);
        assert_eq!(lines, vec![(first, 2), (last, 1)]);
    }
}
