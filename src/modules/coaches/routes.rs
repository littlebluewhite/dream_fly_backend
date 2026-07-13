use axum::{Router, routing::{get, patch, post}};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/coaches", get(handlers::list))
        .route("/coaches/{id}", get(handlers::get_by_id))
        .route("/coaches/{id}/clock-in", post(handlers::clock_in))
        .route("/coaches/{id}/clock-out", post(handlers::clock_out))
        .route("/coaches/{id}/clock-records", get(handlers::get_clock_records))
        .route("/coaches/{id}/schedule", get(handlers::get_schedule).put(handlers::update_schedule))
}

/// admin 半邊:教練建立/更新(與公開的清單/查詢共用路徑,按 method 拆;capture
/// 名 `{id}` 與 `router()` 逐字節相同)。打卡與班表等端點各有自己的授權,留在
/// `router()`。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/coaches", post(handlers::create))
        .route("/coaches/{id}", patch(handlers::update))
}
