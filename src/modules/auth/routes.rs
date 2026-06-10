use axum::{Router, routing::post};
use crate::state::AppState;
use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(handlers::register))
        .route("/auth/login", post(handlers::login))
        .route("/auth/google", post(handlers::google_auth))
        .route("/auth/refresh", post(handlers::refresh))
        .route("/auth/logout", post(handlers::logout))
        .route("/auth/otp/send", post(handlers::send_otp))
        .route("/auth/otp/verify", post(handlers::verify_otp))
        .route("/auth/password/forgot", post(handlers::forgot_password))
        .route("/auth/password/reset", post(handlers::reset_password))
}
