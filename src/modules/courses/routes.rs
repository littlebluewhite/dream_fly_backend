use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/courses", get(handlers::list).post(handlers::create))
        .route("/courses/{param}", get(handlers::get_by_slug_or_id).patch(handlers::update))
}
