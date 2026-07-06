use axum::{
    Router,
    routing::{get, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/report-cards", post(handlers::create_report_card))
        .route("/report-cards/me", get(handlers::my_report_cards))
        .route("/certificates", post(handlers::create_certificate))
        .route("/certificates/me", get(handlers::my_certificates))
}
