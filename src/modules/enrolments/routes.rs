use axum::{
    Router,
    routing::{get, patch},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/enrolments/me", get(handlers::me))
        .route("/enrolments/{id}/cancel", patch(handlers::cancel))
}
