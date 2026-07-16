use axum::{
    Router,
    routing::{get, put},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/coaches/me/students", get(handlers::my_students))
}

/// staff 半邊:點名名冊、批次登記出缺席(admin 或 coach)。coach-only 的
/// `GET /coaches/me/students`(admin 刻意排除,見該 handler 註解)留在
/// `router()`。閘門由 `staff_api` route_layer 施加。
pub fn staff_router() -> Router<AppState> {
    Router::new()
        .route("/sessions/{id}/roster", get(handlers::get_roster))
        .route("/sessions/{id}/attendance", put(handlers::bulk_upsert_attendance))
}
