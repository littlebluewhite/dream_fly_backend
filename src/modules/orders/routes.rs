use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/orders", post(handlers::checkout))
        .route("/orders/me", get(handlers::my_orders))
        .route("/orders/{id}", get(handlers::get_order))
}

/// admin 半邊:全站訂單清單(`GET /orders`,與公開的 `POST /orders` 共用路徑,
/// 按 method 拆)、訂單狀態流轉。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/orders", get(handlers::admin_list_orders))
        .route(
            "/orders/{id}/status",
            axum::routing::patch(handlers::update_status),
        )
}
