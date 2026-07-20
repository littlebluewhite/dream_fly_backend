use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports/me", get(handlers::member_report))
}

/// Coach 半邊:coach-only carve-out(admin 刻意排除,見 `handlers::coach_report`
/// 註解)。閘門由 `coach_api` route_layer 施加(`middleware::require_coach`)。
pub fn coach_router() -> Router<AppState> {
    Router::new()
        .route("/reports/coach", get(handlers::coach_report))
}

/// admin 半邊:管理端報表。`/reports/me`(本人,任何登入使用者皆可)有自己的
/// 角色檢查,留在 `router()`。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/reports/admin", get(handlers::admin_report))
        .route("/reports/admin/activity", get(handlers::admin_activity))
}
