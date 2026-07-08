use axum::{Router, routing::{get, post}};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/coaches", get(handlers::list).post(handlers::create))
        .route("/coaches/{id}", get(handlers::get_by_id).patch(handlers::update))
        .route("/coaches/{id}/clock-in", post(handlers::clock_in))
        .route("/coaches/{id}/clock-out", post(handlers::clock_out))
        .route("/coaches/{id}/clock-records", get(handlers::get_clock_records))
        .route("/coaches/{id}/schedule", get(handlers::get_schedule).put(handlers::update_schedule))
}
