use axum::{
    Router,
    routing::{get, put},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions/{id}/roster", get(handlers::get_roster))
        .route("/sessions/{id}/attendance", put(handlers::bulk_upsert_attendance))
        .route("/coaches/me/students", get(handlers::my_students))
}
