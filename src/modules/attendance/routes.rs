use axum::{
    Router,
    routing::{get, put},
};

use crate::state::AppState;

use super::handlers;

/// staff 半邊:點名名冊、批次登記出缺席(admin 或 coach)。coach-only 的
/// `GET /coaches/me/students` 已獨立收斂至 `coach_router()`(見下方)。閘門
/// 由 `staff_api` route_layer 施加。
pub fn staff_router() -> Router<AppState> {
    Router::new()
        .route("/sessions/{id}/roster", get(handlers::get_roster))
        .route("/sessions/{id}/attendance", put(handlers::bulk_upsert_attendance))
}

/// Coach 半邊:coach-only carve-out(admin 刻意排除,見 `handlers::my_students`
/// 註解)。閘門由 `coach_api` route_layer 施加(`middleware::require_coach`)。
pub fn coach_router() -> Router<AppState> {
    Router::new()
        .route("/coaches/me/students", get(handlers::my_students))
}
