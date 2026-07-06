use axum::{
    Router,
    routing::{get, patch, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/leave-requests", get(handlers::list).post(handlers::create))
        .route("/leave-requests/me", get(handlers::me))
        .route(
            "/leave-requests/{id}",
            patch(handlers::decide).delete(handlers::cancel),
        )
        .route("/leave-requests/{id}/makeup", post(handlers::makeup))
}
