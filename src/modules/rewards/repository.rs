use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{RedemptionWithReward, Reward, RewardRedemption};

/// Input payload for `create`.
pub struct RewardCreate<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub points_cost: i32,
    pub stock: Option<i32>,
    pub display_order: i32,
}

/// Partial update input for `update` (PATCH-style). Nullable DB columns use
/// `Option<Option<T>>` so callers can distinguish "don't touch" (`None`)
/// from "set to NULL" (`Some(None)`) from "set to value" (`Some(Some(v))`) —
/// same idiom as `products::repository::ProductUpdate`.
#[derive(Default)]
pub struct RewardUpdate<'a> {
    pub name: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub points_cost: Option<i32>,
    pub stock: Option<Option<i32>>,
    pub is_active: Option<bool>,
    pub display_order: Option<i32>,
}

/// Active rewards for the member-facing list, sorted for display.
pub async fn find_active(db: &PgPool) -> Result<Vec<Reward>, sqlx::Error> {
    sqlx::query_as::<_, Reward>(
        "SELECT id, name, description, points_cost, stock, is_active, display_order, \
         created_at, updated_at \
         FROM rewards \
         WHERE is_active = true \
         ORDER BY display_order, created_at",
    )
    .fetch_all(db)
    .await
}

/// Every reward regardless of `is_active`, for the admin `?all=true` view.
pub async fn find_all(db: &PgPool) -> Result<Vec<Reward>, sqlx::Error> {
    sqlx::query_as::<_, Reward>(
        "SELECT id, name, description, points_cost, stock, is_active, display_order, \
         created_at, updated_at \
         FROM rewards \
         ORDER BY display_order, created_at",
    )
    .fetch_all(db)
    .await
}

/// Lock the reward row `FOR UPDATE` inside the redeem transaction — this
/// serializes concurrent redemptions of the *same* reward so the
/// stock-check-then-decrement in `service::redeem` can't race.
pub async fn lock_by_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<Reward>, sqlx::Error> {
    sqlx::query_as::<_, Reward>(
        "SELECT id, name, description, points_cost, stock, is_active, display_order, \
         created_at, updated_at \
         FROM rewards \
         WHERE id = $1 \
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Decrement finite stock by 1 inside the redeem transaction; a no-op value
/// (stays `NULL`) for unlimited-stock rewards. Safe without a re-check
/// `WHERE stock > 0` guard because the caller already holds the row lock
/// from `lock_by_id_tx` and validated `stock > 0` earlier in the same
/// transaction — no other transaction can have changed it since.
pub async fn decrement_stock_tx(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE rewards \
         SET stock = CASE WHEN stock IS NULL THEN NULL ELSE stock - 1 END, updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Lock the user's row and read their current points balance inside the
/// redeem transaction — mirrors `orders::repository::lock_user_points_balance_tx`
/// (same "local to the module that needs it" idiom; a second concurrent
/// redeem/checkout for the same user blocks here until this transaction
/// commits or rolls back).
pub async fn lock_user_points_balance_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT points_balance FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await
}

pub async fn insert_redemption_tx(
    tx: &mut Transaction<'_, Postgres>,
    reward_id: Uuid,
    user_id: Uuid,
    points_spent: i32,
) -> Result<RewardRedemption, sqlx::Error> {
    sqlx::query_as::<_, RewardRedemption>(
        "INSERT INTO reward_redemptions (id, reward_id, user_id, points_spent, created_at) \
         VALUES ($1, $2, $3, $4, NOW()) \
         RETURNING id, reward_id, user_id, points_spent, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(reward_id)
    .bind(user_id)
    .bind(points_spent)
    .fetch_one(&mut **tx)
    .await
}

/// This user's redemptions, newest first, paginated, joined with the
/// reward's current name (looked up live rather than snapshotted — unlike
/// order_items' checkout-time name snapshot, there's no financial-record
/// reason to freeze it, and rewards is a small admin-curated catalog).
pub async fn find_redemptions_by_user(
    db: &PgPool,
    user_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<RedemptionWithReward>, sqlx::Error> {
    sqlx::query_as::<_, RedemptionWithReward>(
        "SELECT rr.id, rr.reward_id, r.name AS reward_name, rr.points_spent, rr.created_at \
         FROM reward_redemptions rr \
         JOIN rewards r ON r.id = rr.reward_id \
         WHERE rr.user_id = $1 \
         ORDER BY rr.created_at DESC \
         LIMIT $2 OFFSET $3",
    )
    .bind(user_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(db)
    .await
}

pub async fn count_redemptions_by_user(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM reward_redemptions WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

pub async fn create(db: &PgPool, input: RewardCreate<'_>) -> Result<Reward, sqlx::Error> {
    sqlx::query_as::<_, Reward>(
        "INSERT INTO rewards (id, name, description, points_cost, stock, is_active, \
         display_order, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, true, $5, NOW(), NOW()) \
         RETURNING id, name, description, points_cost, stock, is_active, display_order, \
         created_at, updated_at",
    )
    .bind(input.name)
    .bind(input.description)
    .bind(input.points_cost)
    .bind(input.stock)
    .bind(input.display_order)
    .fetch_one(db)
    .await
}

pub async fn update(db: &PgPool, id: Uuid, input: RewardUpdate<'_>) -> Result<Option<Reward>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE rewards SET updated_at = now()");

    if let Some(v) = input.name {
        qb.push(", name = ").push_bind(v);
    }
    if let Some(v) = input.description {
        qb.push(", description = ").push_bind(v);
    }
    if let Some(v) = input.points_cost {
        qb.push(", points_cost = ").push_bind(v);
    }
    if let Some(v) = input.stock {
        qb.push(", stock = ").push_bind(v);
    }
    if let Some(v) = input.is_active {
        qb.push(", is_active = ").push_bind(v);
    }
    if let Some(v) = input.display_order {
        qb.push(", display_order = ").push_bind(v);
    }

    qb.push(" WHERE id = ").push_bind(id);
    qb.push(" RETURNING *");

    qb.build_query_as::<Reward>().fetch_optional(db).await
}
