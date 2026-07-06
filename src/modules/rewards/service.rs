use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::modules::points::model::PointReason;
use crate::modules::points::service as points_service;

use super::dto::{
    CreateRewardRequest, RedeemResponse, RedemptionListResponse, RedemptionResponse,
    RewardListResponse, RewardResponse, UpdateRewardRequest,
};
use super::repository::{self, RewardCreate, RewardUpdate};

/// `GET /rewards`. `all=true` requires admin — a member only ever sees the
/// `is_active` catalog, sorted for display.
pub async fn list(db: &PgPool, auth: &AuthUser, all: bool) -> Result<RewardListResponse, AppError> {
    let rewards = if all {
        auth.require_role("admin")?;
        repository::find_all(db).await?
    } else {
        repository::find_active(db).await?
    };

    Ok(RewardListResponse {
        rewards: rewards.into_iter().map(RewardResponse::from).collect(),
    })
}

/// 兌換：單一交易內 — 鎖品項（`FOR UPDATE`）→ 檢查 is_active（404）→ 檢查
/// stock（NULL 略過；0 → 409）→ 鎖並檢查 `users.points_balance`（不足 → 409）→
/// ledger 插入 + balance 同步（複用 `points::service::apply_delta_tx`，裁決
/// 7 — 點數唯一真相 = point_ledger + users.points_balance，此處不得另建一套
/// 機制）→ stock -1（非 NULL 才執行）→ 插入 redemption 紀錄。
pub async fn redeem(db: &PgPool, user_id: Uuid, reward_id: Uuid) -> Result<RedeemResponse, AppError> {
    let mut tx = db.begin().await?;

    let reward = repository::lock_by_id_tx(&mut tx, reward_id)
        .await?
        .ok_or_else(|| AppError::NotFound("獎勵不存在".into()))?;

    if !reward.is_active {
        return Err(AppError::NotFound("獎勵不存在".into()));
    }

    if let Some(stock) = reward.stock {
        if stock <= 0 {
            return Err(AppError::Conflict("已兌換完畢".into()));
        }
    }

    // Lock + read the caller's balance inside this transaction — a second
    // concurrent redeem/checkout for the same user blocks on this row lock
    // until we commit or roll back (mirrors
    // `orders::repository::lock_user_points_balance_tx`'s double-spend
    // guard).
    let balance = repository::lock_user_points_balance_tx(&mut tx, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    if balance < reward.points_cost as i64 {
        return Err(AppError::Conflict("點數不足".into()));
    }

    // The one true points mechanism (裁決 7): ledger insert + `users.points_balance`
    // sync, via the same helper `orders::service::checkout` uses. Cannot
    // fail here — we already proved `balance >= points_cost` above under
    // the same row lock this call re-touches.
    let balance_after = points_service::apply_delta_tx(
        &mut tx,
        user_id,
        -(reward.points_cost as i64),
        PointReason::Redeem,
        None,
    )
    .await?;

    if reward.stock.is_some() {
        repository::decrement_stock_tx(&mut tx, reward.id).await?;
    }

    let redemption =
        repository::insert_redemption_tx(&mut tx, reward.id, user_id, reward.points_cost).await?;

    tx.commit().await?;

    Ok(RedeemResponse {
        redemption_id: redemption.id,
        points_spent: redemption.points_spent,
        balance_after,
    })
}

/// `GET /rewards/redemptions/me` — paginated, newest first.
pub async fn my_redemptions(
    db: &PgPool,
    user_id: Uuid,
    pagination: &PaginationParams,
) -> Result<RedemptionListResponse, AppError> {
    let redemptions =
        repository::find_redemptions_by_user(db, user_id, pagination.limit(), pagination.offset())
            .await?;
    let total = repository::count_redemptions_by_user(db, user_id).await?;

    Ok(RedemptionListResponse {
        redemptions: redemptions.into_iter().map(RedemptionResponse::from).collect(),
        total,
        page: pagination.page,
        per_page: pagination.limit(),
    })
}

/// `POST /rewards` — admin only (checked by the handler).
pub async fn create(db: &PgPool, req: CreateRewardRequest) -> Result<RewardResponse, AppError> {
    let reward = repository::create(
        db,
        RewardCreate {
            name: &req.name,
            description: req.description.as_deref(),
            points_cost: req.points_cost,
            stock: req.stock,
            display_order: req.display_order.unwrap_or(0),
        },
    )
    .await?;

    Ok(RewardResponse::from(reward))
}

/// `PATCH /rewards/{id}` — admin only (checked by the handler).
pub async fn update(db: &PgPool, id: Uuid, req: UpdateRewardRequest) -> Result<RewardResponse, AppError> {
    let reward = repository::update(
        db,
        id,
        RewardUpdate {
            name: req.name.as_deref(),
            description: req.description.as_ref().map(|o| o.as_deref()),
            points_cost: req.points_cost,
            stock: req.stock,
            is_active: req.is_active,
            display_order: req.display_order,
        },
    )
    .await?
    .ok_or_else(|| AppError::NotFound("獎勵不存在".into()))?;

    Ok(RewardResponse::from(reward))
}
