use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/waitlist", post(handlers::join).get(handlers::list_for_course))
        .route("/waitlist/me", get(handlers::me))
        .route("/waitlist/{id}", delete(handlers::cancel))
}
