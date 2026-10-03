use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use crate::error::AppError;
use crate::modules::products::model::Product;

/// Discriminates whether a cart (or checkout) line targets a product or a
/// course. Maps to the Postgres `cart_item_type` enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "cart_item_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CartItemType {
    Product,
    Course,
}

impl CartItemType {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` derives from this instead of hand-copying the list.
    pub const ALL: [Self; 2] = [Self::Product, Self::Course];

    /// The SQL string literal for this variant. The Postgres `cart_item_type`
    /// enum, the `item_type` columns, and this method must all agree on these
    /// two spellings. Reads are type-checked: every row that carries the
    /// `item_type`/`product_id`/`course_id` union decodes through
    /// [`LineTarget`]'s `FromRow`, and every write of an order line binds
    /// `LineTarget::item_type()` itself. What remains hand-written into SQL:
    ///
    /// SQL-literal sites, by function (each hard-codes `'product'`/`'course'`):
    /// - `cart::repository::add_product_item_tx` — `'product'::cart_item_type` on insert
    /// - `cart::repository::add_course_item` — `'course'::cart_item_type` on insert
    /// - `cart::repository::find_cart_items_for_checkout_tx` — ×4: the
    ///   `'product'`/`'course'` SELECT literal and the `item_type = '…'`
    ///   filter, once in each of the two (product, course) branch queries
    /// - `reports::repository` income-source `CASE` — maps `oi.item_type =
    ///   'course'` into the `course` revenue bucket
    /// - `products::repository::find_stock_traces_by_order_tx` — `item_type =
    ///   'product'::cart_item_type` filter
    ///
    /// Adding a variant — the full checklist:
    /// 1. `ALTER TYPE cart_item_type ADD VALUE '…'` migration.
    /// 2. Re-sync the `cart_items` target + quantity CHECKs
    ///    (`cart_items_one_target`, `cart_items_course_qty`; migration
    ///    `20260704000001`, lines 34–49) — a new target column and its
    ///    exclusivity/quantity rules.
    /// 3. Every SQL-literal site listed above.
    /// 4. A new [`LineTarget`] variant — the compiler then forces every
    ///    exhaustive `match` on it (its decoder and accessors,
    ///    `orders::fulfilment::plan`, `orders::fulfilment::order_lines`,
    ///    `products::service::restock_lines`, …; none has a `_` arm).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Course => "course",
        }
    }

    /// The per-type quantity rule for the requested quantity, before any
    /// row is looked up: `Product` delegates to
    /// `products::model::Product::ensure_quantity_in_range`, `Course` allows
    /// only `1`. For a product this is only an early range guard (it keeps
    /// the 400 ahead of the product lookup's 404); the owner of a product
    /// line's legal quantity — range, the time-based entitlement rule, and
    /// judged on the merged/final quantity — is
    /// `products::model::Product::ensure_line_quantity`.
    ///
    /// `update_quantity`'s pre-lookup guard is a separate, deliberately
    /// duplicated inline check — see the comment there for why it isn't
    /// just a call to this method.
    pub fn validate_quantity(&self, qty: i32) -> Result<(), AppError> {
        match self {
            Self::Product => Product::ensure_quantity_in_range(qty)?,
            Self::Course => {
                if qty != 1 {
                    return Err(AppError::Validation("course quantity must be 1".into()));
                }
            }
        }
        Ok(())
    }
}

impl std::str::FromStr for CartItemType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|v| v.as_str() == s).ok_or(())
    }
}

/// What one cart / order line targets — the `item_type` + `product_id` /
/// `course_id` discriminated union (ADR-0002) decoded once, at the row
/// boundary. The DB keeps the three-column shape; the
/// `cart_items_one_target`/`order_items_one_target` CHECKs (migration
/// `20260704000001`) allow exactly the two shapes this enum has, and the
/// hand-written `FromRow` below accepts exactly those two — anything else is
/// `sqlx::Error::Decode`, never a variant. Past the decode no caller unwraps
/// an `Option` id or re-matches `item_type` against it. Embedded with
/// `#[sqlx(flatten)]`; the accessors map back to the DB columns for binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineTarget {
    Product(Uuid),
    Course(Uuid),
}

impl LineTarget {
    /// The `item_type` column value for this target.
    pub fn item_type(&self) -> CartItemType {
        match self {
            Self::Product(_) => CartItemType::Product,
            Self::Course(_) => CartItemType::Course,
        }
    }

    /// The `product_id` column value: `Some` only for a product line.
    pub fn product_id(&self) -> Option<Uuid> {
        match self {
            Self::Product(id) => Some(*id),
            Self::Course(_) => None,
        }
    }

    /// The `course_id` column value: `Some` only for a course line.
    pub fn course_id(&self) -> Option<Uuid> {
        match self {
            Self::Product(_) => None,
            Self::Course(id) => Some(*id),
        }
    }
}

impl<'r> sqlx::FromRow<'r, PgRow> for LineTarget {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let item_type: CartItemType = row.try_get("item_type")?;
        let product_id: Option<Uuid> = row.try_get("product_id")?;
        let course_id: Option<Uuid> = row.try_get("course_id")?;
        match (item_type, product_id, course_id) {
            (CartItemType::Product, Some(id), None) => Ok(Self::Product(id)),
            (CartItemType::Course, None, Some(id)) => Ok(Self::Course(id)),
            (item_type, product_id, course_id) => Err(sqlx::Error::Decode(
                format!(
                    "line target mismatch: item_type={} product_id={product_id:?} \
                     course_id={course_id:?}",
                    item_type.as_str()
                )
                .into(),
            )),
        }
    }
}

/// Raw `cart_items` row.
#[derive(Debug, sqlx::FromRow)]
pub struct CartItem {
    pub id: Uuid,
    pub user_id: Uuid,
    #[sqlx(flatten)]
    pub target: LineTarget,
    pub quantity: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Cart row joined against whichever table its target points at, to surface
/// the display name/slug/price for `CartResponse`.
#[derive(Debug, sqlx::FromRow)]
pub struct CartItemJoined {
    pub id: Uuid,
    pub user_id: Uuid,
    #[sqlx(flatten)]
    pub target: LineTarget,
    pub quantity: i32,
    pub name: String,
    pub slug: String,
    pub price_cents: i64,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Cart line snapshot consumed by `orders::service::checkout`. Produced by
/// `repository::find_cart_items_for_checkout_tx`; only becomes purchasable
/// by passing `orders::fulfilment::ensure_all_purchasable`, which wraps the
/// lines in `PurchasableCart`.
#[derive(Debug, sqlx::FromRow)]
pub struct CheckoutLine {
    #[sqlx(flatten)]
    pub target: LineTarget,
    pub quantity: i32,
    pub price_cents: i64,
    pub name: String,
    pub is_active: bool,
}

/// The ids a checkout's cart targets — what the order lock protocol
/// (`orders::locks::acquire_checkout_locks`) locks before the cart snapshot
/// is read. Produced by `repository::find_checkout_targets_tx`; unsorted
/// (the lock owners sort).
#[derive(Debug, sqlx::FromRow)]
pub struct CheckoutTargets {
    pub product_ids: Vec<Uuid>,
    pub course_ids: Vec<Uuid>,
}

/// 行小計的溢位安全乘法——pricing 與 CartResponse 共用,溢位文案各自保留。
pub fn checked_line_subtotal(price_cents: i64, quantity: i32) -> Option<i64> {
    price_cents.checked_mul(quantity as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- validate_quantity: Product (1..=999) ---

    #[test]
    fn product_quantity_within_1_to_999_is_ok() {
        assert!(CartItemType::Product.validate_quantity(1).is_ok());
        assert!(CartItemType::Product.validate_quantity(500).is_ok());
        assert!(CartItemType::Product.validate_quantity(999).is_ok());
    }

    #[test]
    fn product_quantity_outside_1_to_999_is_bad_request() {
        for qty in [i32::MIN, -1, 0, 1000, i32::MAX] {
            let err = CartItemType::Product
                .validate_quantity(qty)
                .expect_err("must reject");
            assert!(
                matches!(err, AppError::BadRequest(ref m) if m == "quantity must be between 1 and 999"),
                "got: {err:?} for qty={qty}"
            );
        }
    }

    // --- validate_quantity: Course (== 1) ---

    #[test]
    fn course_quantity_of_one_is_ok() {
        assert!(CartItemType::Course.validate_quantity(1).is_ok());
    }

    #[test]
    fn course_quantity_other_than_one_is_validation_error() {
        for qty in [i32::MIN, -1, 0, 2, 999, 1000, i32::MAX] {
            let err = CartItemType::Course
                .validate_quantity(qty)
                .expect_err("must reject");
            assert!(
                matches!(err, AppError::Validation(ref m) if m == "course quantity must be 1"),
                "got: {err:?} for qty={qty}"
            );
        }
    }

    // --- LineTarget decode (the one place the item_type/id union is read) ---

    #[sqlx::test]
    async fn line_target_decode_rejects_mismatched_columns(db: sqlx::PgPool) {
        let id = Uuid::now_v7();
        let decode = |sql: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query_as::<_, LineTarget>(sql)
                    .bind(id)
                    .fetch_one(&db)
                    .await
            }
        };

        // The two shapes the `*_one_target` CHECKs allow decode to a variant.
        let product = decode(
            "SELECT 'product'::cart_item_type AS item_type, $1::uuid AS product_id, \
             NULL::uuid AS course_id",
        )
        .await
        .expect("product shape decodes");
        assert_eq!(product, LineTarget::Product(id));
        assert_eq!(product.item_type().as_str(), "product");
        assert_eq!(
            (product.product_id(), product.course_id()),
            (Some(id), None)
        );

        let course = decode(
            "SELECT 'course'::cart_item_type AS item_type, NULL::uuid AS product_id, \
             $1::uuid AS course_id",
        )
        .await
        .expect("course shape decodes");
        assert_eq!(course, LineTarget::Course(id));
        assert_eq!(course.item_type().as_str(), "course");
        assert_eq!((course.product_id(), course.course_id()), (None, Some(id)));

        // Every shape the CHECKs forbid is a decode error, never a variant.
        for sql in [
            "SELECT 'product'::cart_item_type AS item_type, NULL::uuid AS product_id, \
             $1::uuid AS course_id",
            "SELECT 'product'::cart_item_type AS item_type, NULL::uuid AS product_id, \
             NULL::uuid AS course_id",
            "SELECT 'product'::cart_item_type AS item_type, $1::uuid AS product_id, \
             $1::uuid AS course_id",
            "SELECT 'course'::cart_item_type AS item_type, $1::uuid AS product_id, \
             NULL::uuid AS course_id",
            "SELECT 'course'::cart_item_type AS item_type, NULL::uuid AS product_id, \
             NULL::uuid AS course_id",
            "SELECT 'course'::cart_item_type AS item_type, $1::uuid AS product_id, \
             $1::uuid AS course_id",
        ] {
            let err = decode(sql)
                .await
                .expect_err("mismatched columns must not decode");
            assert!(
                matches!(err, sqlx::Error::Decode(_)),
                "got: {err:?} for {sql}"
            );
        }
    }
}
