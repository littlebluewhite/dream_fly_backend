use axum::{
    Router,
    routing::{get, patch},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/coupons", get(handlers::list).post(handlers::create))
        .route(
            "/coupons/{id}",
            patch(handlers::update).delete(handlers::delete),
        )
        .route("/coupons/{code}/validate", get(handlers::validate))
}
