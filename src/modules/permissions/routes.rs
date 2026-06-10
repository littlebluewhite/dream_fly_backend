use axum::{Router, routing::{delete, get, post}};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/roles", get(handlers::list_roles).post(handlers::create_role))
        .route("/roles/{role_id}/users", post(handlers::assign_role))
        .route("/roles/{role_id}/users/{user_id}", delete(handlers::remove_role))
}
