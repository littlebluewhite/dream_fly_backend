use axum::{Router, routing::get};
use crate::state::AppState;

use super::handlers;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/posts", get(handlers::list).post(handlers::create))
        .route(
            "/posts/{param}",
            get(handlers::get_by_slug_or_id)
                .patch(handlers::update)
                .delete(handlers::delete),
        )
}
