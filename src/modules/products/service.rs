use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PageMeta;
use crate::utils::slug::slugify;

use super::dto::{
    CreateProductRequest, ProductListResponse, ProductResponse, UpdateProductRequest,
};
use super::model::Product;
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
    page: u32,
    per_page: u32,
) -> Result<ProductListResponse, AppError> {
    let per_page = per_page.clamp(1, 100);
    let offset = page.max(1).saturating_sub(1) * per_page;

    // Count first so a zero-total response doesn't need a second (empty)
    // result set; both queries share the same filter.
    let total = repository::count_active(db, product_type_filter).await?;
    let products = repository::find_all_active(db, product_type_filter, per_page, offset).await?;

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
        meta: PageMeta {
            total,
            page: page.max(1),
            per_page,
        },
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

pub async fn create(db: &PgPool, req: CreateProductRequest) -> Result<ProductResponse, AppError> {
    let slug = req.slug.unwrap_or_else(|| slugify(&req.name));

    // Validate product_type
    let pt = &req.product_type;
    if !["ticket", "course_package", "membership", "merchandise"].contains(&pt.as_str()) {
        return Err(AppError::Validation(format!("invalid product_type: {}", pt)));
    }

    // Rely on the DB unique index for slug uniqueness — avoids TOCTOU race
    // between a SELECT check and the INSERT.
    let product = match repository::create(
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
    {
        Ok(p) => p,
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            return Err(AppError::Conflict(format!("slug '{}' already exists", slug)));
        }
        Err(e) => return Err(AppError::Database(e)),
    };

    to_response(db, product).await
}

pub async fn update(
    db: &PgPool,
    id: Uuid,
    req: UpdateProductRequest,
) -> Result<ProductResponse, AppError> {
    // Validate product_type if provided
    if let Some(ref pt) = req.product_type {
        if !["ticket", "course_package", "membership", "merchandise"].contains(&pt.as_str()) {
            return Err(AppError::Validation(format!("invalid product_type: {}", pt)));
        }
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
    .await?
    .ok_or_else(|| AppError::NotFound("product not found".into()))?;

    to_response(db, product).await
}
