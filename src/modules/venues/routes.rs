use axum::{Router, routing::{get, patch, post}};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/venues", get(handlers::list))
        .route("/venues/{slug}", get(handlers::get_by_slug))
}

/// admin 半邊:場館建立/更新(與公開的清單/查詢共用路徑,按 method 拆;capture
/// 名 `{slug}` 與 `router()` 逐字節相同)。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/venues", post(handlers::create))
        .route("/venues/{slug}", patch(handlers::update))
}
