use axum::{
    Router,
    routing::{get, patch, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/rewards", get(handlers::list).post(handlers::create))
        .route("/rewards/redemptions/me", get(handlers::my_redemptions))
        .route("/rewards/{id}", patch(handlers::update))
        .route("/rewards/{id}/redeem", post(handlers::redeem))
}
