use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/orders",
            post(handlers::checkout).get(handlers::admin_list_orders),
        )
        .route("/orders/me", get(handlers::my_orders))
        .route("/orders/{id}", get(handlers::get_order))
        .route("/orders/{id}/status", axum::routing::patch(handlers::update_status))
}
