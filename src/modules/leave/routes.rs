use axum::{
    Router,
    routing::{delete, get, patch, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/leave-requests", post(handlers::create))
        .route("/leave-requests/me", get(handlers::me))
        .route("/leave-requests/{id}", delete(handlers::cancel))
        .route("/leave-requests/{id}/makeup", post(handlers::makeup))
}

/// staff 半邊:清單查詢、核准/婉拒(admin 或 coach;與公開的建立/取消/補課
/// 共用路徑,按 method 拆;capture 名 `{id}` 與 `router()` 逐字節相同)。
/// 閘門由 `staff_api` route_layer 施加。
pub fn staff_router() -> Router<AppState> {
    Router::new()
        .route("/leave-requests", get(handlers::list))
        .route("/leave-requests/{id}", patch(handlers::decide))
}
