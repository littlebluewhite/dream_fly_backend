use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/schedule", get(handlers::get_monthly))
        .route("/schedule/availability", get(handlers::get_availability))
}

/// admin 半邊:建立時段。公開的月曆/可預約查詢留在 `router()`。
/// 閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/schedule/slots", axum::routing::post(handlers::create_slots))
}
