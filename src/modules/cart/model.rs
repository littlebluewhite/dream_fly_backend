use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Discriminates whether a cart (or checkout) line targets a product or a
/// course. Maps to the Postgres `cart_item_type` enum.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "cart_item_type", rename_all = "snake_case")]
pub enum CartItemType {
    Product,
    Course,
}

impl CartItemType {
    /// The SQL string literal for this variant. The Postgres `cart_item_type`
    /// enum, the `item_type` columns, and this method must all agree on these
    /// two spellings, and — outside `fulfilment::plan` — the type system
    /// cannot enforce that: the spellings are hand-written into SQL at a
    /// spread of sites.
    ///
    /// SQL-literal sites, by function (each hard-codes `'product'`/`'course'`):
    /// - `cart::repository::add_product_item` — `'product'::cart_item_type` on insert
    /// - `cart::repository::add_course_item` — `'course'::cart_item_type` on insert
    /// - `cart::repository::find_cart_items_for_checkout_tx` — ×4: the
    ///   `'product'`/`'course'` SELECT literal plus the `item_type = '…'`
    ///   filter, once in each of the two (product, course) branch queries
    /// - `orders::repository::create_order_items` — the `CASE WHEN
    ///   u.product_id IS NOT NULL THEN 'product' ELSE 'course' END` derivation
    /// - `reports::repository` income-source `CASE` — maps `oi.item_type =
    ///   'course'` into the `course` revenue bucket
    /// - `bin/seed.rs` order-line `CASE` — the same product/course derivation
    ///   for the deterministic reporting dataset
    ///
    /// Adding a variant — the full checklist:
    /// 1. `ALTER TYPE cart_item_type ADD VALUE '…'` migration.
    /// 2. Re-sync the `cart_items` target + quantity CHECKs
    ///    (`cart_items_one_target`, `cart_items_course_qty`; migration
    ///    `20260704000001`, lines 34–49) — a new target column and its
    ///    exclusivity/quantity rules.
    /// 3. Every SQL-literal site listed above.
    /// 4. `orders::fulfilment::plan`'s exhaustive `match` — the compiler forces
    ///    this one (no `_` arm); it is the only site the type system catches.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Course => "course",
        }
    }
}

impl std::str::FromStr for CartItemType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "product" => Ok(Self::Product),
            "course" => Ok(Self::Course),
            _ => Err(()),
        }
    }
}

/// Raw `cart_items` row. Exactly one of `product_id`/`course_id` is set,
/// matching `item_type` (enforced by the `cart_items_one_target` CHECK).
#[derive(Debug, sqlx::FromRow)]
pub struct CartItem {
    pub id: Uuid,
    pub user_id: Uuid,
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Cart row joined against whichever table `item_type` targets, to surface
/// the display name/slug/price for `CartResponse`.
#[derive(Debug, sqlx::FromRow)]
pub struct CartItemJoined {
    pub id: Uuid,
    pub user_id: Uuid,
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub name: String,
    pub slug: String,
    pub price_cents: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Cart line snapshot consumed by `orders::service::checkout` to build order
/// items. Produced by `repository::find_cart_items_for_checkout_tx`.
#[derive(Debug, sqlx::FromRow)]
pub struct CheckoutLine {
    pub item_type: CartItemType,
    pub product_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub quantity: i32,
    pub price_cents: i64,
    pub name: String,
}

/// 行小計的溢位安全乘法——pricing 與 CartResponse 共用,溢位文案各自保留。
pub fn checked_line_subtotal(price_cents: i64, quantity: i32) -> Option<i64> {
    price_cents.checked_mul(quantity as i64)
}
