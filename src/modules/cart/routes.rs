use axum::Router;
use axum::routing::{get, post};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/cart", get(handlers::get_cart).delete(handlers::clear))
        .route("/cart/items", post(handlers::add_item))
        .route("/cart/items/{id}", axum::routing::patch(handlers::update_quantity).delete(handlers::remove_item))
}
