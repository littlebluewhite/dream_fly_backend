use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

/// 全數 admin-only:整個模組上移到 admin 半邊,`router()` 退場(不再掛入公開
/// merge 清單)。閘門由 `startup.rs` 的 `admin_api` route_layer 單點施加。
pub fn admin_router() -> Router<AppState> {
    Router::new().route(
        "/settings",
        get(handlers::get_settings).put(handlers::update_settings),
    )
}
