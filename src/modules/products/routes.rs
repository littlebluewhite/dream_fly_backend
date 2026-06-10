use axum::Router;
use axum::routing::get;

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/products", get(handlers::list).post(handlers::create))
        .route("/products/{slug_or_id}", get(handlers::get_by_slug).patch(handlers::update))
}
