use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/venues", get(handlers::list).post(handlers::create))
        .route("/venues/{slug}", get(handlers::get_by_slug))
}
