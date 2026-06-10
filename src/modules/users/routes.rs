use axum::{Router, routing::get};
use crate::state::AppState;
use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users/me", get(handlers::me).patch(handlers::update_me))
        .route("/users", get(handlers::list))
        .route("/users/{id}", get(handlers::get_user))
}
