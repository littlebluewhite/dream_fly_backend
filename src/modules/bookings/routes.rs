use axum::{Router, routing::{get, patch, post}};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/bookings", post(handlers::create))
        .route("/bookings/me", get(handlers::my_bookings))
        .route("/bookings/{id}/cancel", patch(handlers::cancel))
}

/// admin 半邊:全站預約清單(`GET /bookings`,與公開的 `POST /bookings` 共用
/// 路徑,按 method 拆)。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/bookings", get(handlers::list_all))
}
