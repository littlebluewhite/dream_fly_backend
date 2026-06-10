use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/schedule", get(handlers::get_monthly))
        .route("/schedule/availability", get(handlers::get_availability))
        .route("/schedule/slots", axum::routing::post(handlers::create_slots))
}
