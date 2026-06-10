use axum::{Router, routing::{get, patch}};
use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/notifications", get(handlers::list))
        .route("/notifications/unread-count", get(handlers::unread_count))
        .route("/notifications/{id}/read", patch(handlers::mark_read))
}
