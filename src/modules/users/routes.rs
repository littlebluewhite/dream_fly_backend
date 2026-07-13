use axum::{Router, routing::get};
use crate::state::AppState;
use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users/me", get(handlers::me).patch(handlers::update_me))
}

/// admin 半邊:使用者清單/建立/查詢/後台更新。`/users/me`(本人)留在 `router()`。
/// 閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/users", get(handlers::list).post(handlers::create))
        .route("/users/{id}", get(handlers::get_user).patch(handlers::admin_update))
}
