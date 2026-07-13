use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/waitlist", post(handlers::join))
        .route("/waitlist/me", get(handlers::me))
        .route("/waitlist/{id}", delete(handlers::cancel))
}

/// admin 半邊:某課程候補名單(`GET /waitlist`,與公開的 `POST /waitlist` 共用
/// 路徑,按 method 拆)。閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/waitlist", get(handlers::list_for_course))
}
