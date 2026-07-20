use axum::{Router, routing::post};
use crate::state::AppState;
use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/logout", post(handlers::logout))
}

/// 8 條吃憑證/花錢的端點(register/login/google/refresh/otp/send、
/// otp/verify、password/forgot、password/reset),與舊版 `is_auth_endpoint`
/// 前綴清單一一對應。本群組吃 10/min 嚴格桶;新增吃憑證或觸發
/// OTP/email/SMS 的端點必須加在這裡。閘門由 `strict_rate_limit`
/// route_layer 施加,掛在 `startup.rs`(宣告形狀比照 `admin_api`/
/// `staff_api`;見 `middleware::rate_limit::strict_rate_limit`)。
pub fn throttled_router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(handlers::register))
        .route("/auth/login", post(handlers::login))
        .route("/auth/google", post(handlers::google_auth))
        .route("/auth/refresh", post(handlers::refresh))
        .route("/auth/otp/send", post(handlers::send_otp))
        .route("/auth/otp/verify", post(handlers::verify_otp))
        .route("/auth/password/forgot", post(handlers::forgot_password))
        .route("/auth/password/reset", post(handlers::reset_password))
}
