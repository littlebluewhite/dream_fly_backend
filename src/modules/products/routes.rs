use axum::Router;
use axum::routing::{get, patch, post};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/products", get(handlers::list))
        .route("/products/{slug_or_id}", get(handlers::get_by_slug))
}

/// admin 半邊:商品建立/更新(與公開的清單/查詢共用路徑,按 method 拆;capture
/// 名 `{slug_or_id}` 與 `router()` 逐字節相同)。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/products", post(handlers::create))
        .route("/products/{slug_or_id}", patch(handlers::update))
}
