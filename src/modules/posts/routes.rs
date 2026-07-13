use axum::{Router, routing::{delete, get}};
use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/posts", get(handlers::list).post(handlers::create))
        .route(
            "/posts/{param}",
            get(handlers::get_by_slug_or_id).patch(handlers::update),
        )
}

/// admin 半邊:刪文(`DELETE /posts/{param}`,與公開的查詢/更新共用路徑,按
/// method 拆;capture 名 `{param}` 與 `router()` 逐字節相同)。`POST /posts`
/// (admin 或 coach)、`PATCH /posts/{param}`(admin 或作者)各有自己的授權,
/// 留在 `router()`。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/posts/{param}", delete(handlers::delete))
}
