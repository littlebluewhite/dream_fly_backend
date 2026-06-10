use axum::{Router, routing::{get, patch, post}};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/bookings", post(handlers::create).get(handlers::list_all))
        .route("/bookings/me", get(handlers::my_bookings))
        .route("/bookings/{id}/cancel", patch(handlers::cancel))
}
