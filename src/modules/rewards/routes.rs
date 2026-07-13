use axum::{
    Router,
    routing::{get, patch, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/rewards", get(handlers::list))
        .route("/rewards/redemptions/me", get(handlers::my_redemptions))
        .route("/rewards/{id}/redeem", post(handlers::redeem))
}

/// admin 半邊:獎勵建立/更新。`GET /rewards`(list)是**條件式**閘門
/// (`?all=true` 才需 admin,見 handlers::list),不可上移——留在 `router()`
/// 並與公開的 `POST /rewards` 共用路徑按 method 拆。閘門由 `admin_api`
/// route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/rewards", post(handlers::create))
        .route("/rewards/{id}", patch(handlers::update))
}
