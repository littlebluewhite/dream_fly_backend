use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::extractors::pagination::PageMeta;
use crate::utils::double_option::deserialize_some;

use super::model::{RedemptionWithReward, Reward};

#[derive(Debug, Serialize)]
pub struct RewardResponse {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub points_cost: i32,
    pub stock: Option<i32>,
    pub is_active: bool,
    pub display_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Reward> for RewardResponse {
    fn from(r: Reward) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.description,
            points_cost: r.points_cost,
            stock: r.stock,
            is_active: r.is_active,
            display_order: r.display_order,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RewardListResponse {
    pub rewards: Vec<RewardResponse>,
}

/// Query params for `GET /rewards`. `all=true` additionally requires admin
/// (checked in `handlers::list`) — a plain member always gets the
/// `is_active`-only catalog regardless of this flag.
#[derive(Debug, Deserialize)]
pub struct RewardListQuery {
    pub all: Option<bool>,
}

/// Response for `POST /rewards/{id}/redeem`.
#[derive(Debug, Serialize)]
pub struct RedeemResponse {
    pub redemption_id: Uuid,
    pub points_spent: i32,
    pub balance_after: i64,
}

#[derive(Debug, Serialize)]
pub struct RedemptionResponse {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub reward_name: String,
    pub points_spent: i32,
    pub created_at: DateTime<Utc>,
}

impl From<RedemptionWithReward> for RedemptionResponse {
    fn from(r: RedemptionWithReward) -> Self {
        Self {
            id: r.id,
            reward_id: r.reward_id,
            reward_name: r.reward_name,
            points_spent: r.points_spent,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RedemptionListResponse {
    pub redemptions: Vec<RedemptionResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateRewardRequest {
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    #[validate(length(max = 5000))]
    pub description: Option<String>,
    #[validate(range(min = 1))]
    pub points_cost: i32,
    #[validate(range(min = 0))]
    pub stock: Option<i32>,
    pub display_order: Option<i32>,
}

/// Partial update payload for `PATCH /rewards/{id}`. `description`/`stock`
/// use `Option<Option<T>>` (paired with `deserialize_some`, see above) so
/// callers can distinguish "don't touch" (`None`), "set to NULL"
/// (`Some(None)`), and "set to value" (`Some(Some(v))`). No `#[validate]` on
/// those two fields (validator can't express nested `Option` cleanly; the DB
/// CHECK constraints are the backstop, mirroring
/// `products::dto::UpdateProductRequest`).
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateRewardRequest {
    #[validate(length(min = 1, max = 200))]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub description: Option<Option<String>>,
    #[validate(range(min = 1))]
    pub points_cost: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub stock: Option<Option<i32>>,
    pub is_active: Option<bool>,
    pub display_order: Option<i32>,
}
