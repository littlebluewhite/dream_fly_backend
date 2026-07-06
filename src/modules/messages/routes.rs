use axum::{
    Router,
    routing::{get, patch, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/conversations", post(handlers::create_conversation))
        .route("/conversations/me", get(handlers::my_conversations))
        .route(
            "/conversations/{id}/messages",
            get(handlers::list_messages).post(handlers::send_message),
        )
        .route("/conversations/{id}/read", patch(handlers::mark_read))
}
