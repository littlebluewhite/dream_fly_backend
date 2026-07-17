use axum::{
    Router,
    routing::{get, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new().route("/points/me", get(handlers::me))
}

/// admin 半邊:補點端點(Step 10f)——關閉退款/取消補償 clawback 步驟 409
/// 「點數不足」的修復迴路(見 `service::adjust_points`)。閘門由 `admin_api`
/// route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new().route("/points/adjustments", post(handlers::adjust))
}
