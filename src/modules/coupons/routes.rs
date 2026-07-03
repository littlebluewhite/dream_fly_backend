use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/coupons", get(handlers::list).post(handlers::create))
        .route("/coupons/{code}/validate", get(handlers::validate))
}
