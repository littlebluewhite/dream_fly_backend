use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/settings",
        get(handlers::get_settings).put(handlers::update_settings),
    )
}
