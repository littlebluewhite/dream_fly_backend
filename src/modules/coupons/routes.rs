use axum::{
    Router,
    routing::{get, patch},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/coupons/{code}/validate", get(handlers::validate))
}

/// admin 半邊:優惠券 CRUD。公開的 `/coupons/{code}/validate` 留在 `router()`。
/// 閘門由 `admin_api` route_layer 施加。
pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/coupons", get(handlers::list).post(handlers::create))
        .route(
            "/coupons/{id}",
            patch(handlers::update).delete(handlers::delete),
        )
}
