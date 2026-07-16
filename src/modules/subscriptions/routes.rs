use axum::{
    Router,
    routing::{get, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new().route("/subscriptions/me", get(handlers::me))
}

/// staff 半邊:核銷訂閱堂數(admin 或 coach)。個人查詢留在 `router()`。
/// 閘門由 `staff_api` route_layer 施加。
pub fn staff_router() -> Router<AppState> {
    Router::new().route("/subscriptions/{id}/redeem", post(handlers::redeem))
}
