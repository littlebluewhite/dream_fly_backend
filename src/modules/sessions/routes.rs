use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/courses/{id}/sessions", get(handlers::list_course_sessions))
        .route("/sessions/today", get(handlers::today))
        .route("/schedule/me", get(handlers::my_schedule))
}
