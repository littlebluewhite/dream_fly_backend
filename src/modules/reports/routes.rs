use axum::{Router, routing::get};

use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/reports/admin", get(handlers::admin_report))
        .route("/reports/admin/activity", get(handlers::admin_activity))
        .route("/reports/coach", get(handlers::coach_report))
        .route("/reports/me", get(handlers::member_report))
}
