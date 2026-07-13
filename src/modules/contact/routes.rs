use axum::{Router, routing::{get, patch, post}};
use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/contact", post(handlers::submit))
}

/// admin 半邊:客訴查詢與跟進。公開的 `POST /contact` 留在 `router()`。
/// 閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/contact/inquiries", get(handlers::list))
        .route("/contact/inquiries/{id}", patch(handlers::update))
}
