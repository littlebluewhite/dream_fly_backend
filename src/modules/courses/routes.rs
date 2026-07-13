use axum::{Router, routing::{get, patch, post}};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/courses", get(handlers::list))
        .route("/courses/{param}", get(handlers::get_by_slug_or_id))
}

/// admin 半邊:課程建立/更新(與公開的清單/查詢共用路徑,按 method 拆;capture
/// 名 `{param}` 與 `router()` 逐字節相同)。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/courses", post(handlers::create))
        .route("/courses/{param}", patch(handlers::update))
}
