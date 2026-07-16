use axum::{
    Router,
    routing::{get, post},
};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/report-cards/me", get(handlers::my_report_cards))
        .route("/certificates/me", get(handlers::my_certificates))
}

/// staff 半邊:成績單、證書建立(admin 或 coach;細粒度學生/課程歸屬檢查
/// 留在 service)。個人查詢留在 `router()`。閘門由 `staff_api` route_layer
/// 施加。
pub fn staff_router() -> Router<AppState> {
    Router::new()
        .route("/report-cards", post(handlers::create_report_card))
        .route("/certificates", post(handlers::create_certificate))
}
