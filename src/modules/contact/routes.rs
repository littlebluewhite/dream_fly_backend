use axum::{Router, routing::{get, patch, post}};
use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/contact", post(handlers::submit))
        .route("/contact/inquiries", get(handlers::list))
        .route("/contact/inquiries/{id}", patch(handlers::update))
}
