use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Reward {
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

#[derive(Debug, sqlx::FromRow)]
pub struct RewardRedemption {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub user_id: Uuid,
    pub points_spent: i32,
    pub created_at: DateTime<Utc>,
}

/// `reward_redemptions` joined with the reward's current name, for
/// `GET /rewards/redemptions/me`.
#[derive(Debug, sqlx::FromRow)]
pub struct RedemptionWithReward {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub reward_name: String,
    pub points_spent: i32,
    pub created_at: DateTime<Utc>,
}
