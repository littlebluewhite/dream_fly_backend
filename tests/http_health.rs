//! HTTP integration tests for `GET /health`: the full response body (status
//! code, overall status, per-dependency status) when every dependency is up.

mod common;

use common::http::spawn_test_app;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn health_up_body(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/health").await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(
        resp.json::<serde_json::Value>(),
        json!({
            "status": "healthy",
            "services": { "database": "up", "redis": "up", "kafka": "disabled" }
        })
    );
}
