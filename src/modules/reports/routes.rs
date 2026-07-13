use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports/coach", get(handlers::coach_report))
        .route("/reports/me", get(handlers::member_report))
}

/// admin 半邊:管理端報表。`/reports/coach`(coach)、`/reports/me`(本人)各有
/// 自己的角色檢查,留在 `router()`。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/reports/admin", get(handlers::admin_report))
        .route("/reports/admin/activity", get(handlers::admin_activity))
}
