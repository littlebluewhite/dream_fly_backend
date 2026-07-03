use axum::{
    Router,
    routing::{get, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/subscriptions/me", get(handlers::me))
        .route("/subscriptions/{id}/redeem", post(handlers::redeem))
}
