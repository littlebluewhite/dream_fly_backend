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
