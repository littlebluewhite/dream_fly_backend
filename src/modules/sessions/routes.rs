use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/courses/{id}/sessions", get(handlers::list_course_sessions))
        .route("/schedule/me", get(handlers::my_schedule))
}

/// staff 半邊:今日課程總覽(admin 或 coach)。公開的課程 session 查詢、個人
/// 課表留在 `router()`。閘門由 `staff_api` route_layer 施加。
pub fn staff_router() -> Router<AppState> {
    Router::new().route("/sessions/today", get(handlers::today))
}
