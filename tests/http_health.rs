//! HTTP integration tests for `GET /health`: the full response body (status
//! code, overall status, per-dependency status) for each `HealthProbe`
//! answer, plus the production `RedisHealthProbe` adapter against the test
//! Redis. The router tests use `StaticHealthProbe`, so they need no Redis.

mod common;

use std::sync::Arc;

use common::http::{spawn_test_app, spawn_test_app_with_health};
use common::mocks::StaticHealthProbe;
use dream_fly_backend::health::{HealthProbe, RedisHealthProbe};
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

#[sqlx::test]
async fn health_redis_down_body(db: PgPool) {
    let app = spawn_test_app_with_health(db, Arc::new(StaticHealthProbe::redis_down())).await;
    let resp = app.get("/api/v1/health").await;
    assert_eq!(resp.status_code(), 503, "body={}", resp.text());
    assert_eq!(
        resp.json::<serde_json::Value>(),
        json!({
            "status": "degraded",
            "services": { "database": "up", "redis": "down", "kafka": "disabled" }
        })
    );
}

#[sqlx::test]
async fn health_kafka_connected_body(db: PgPool) {
    let probe = StaticHealthProbe {
        redis_up: true,
        kafka_connected: true,
    };
    let app = spawn_test_app_with_health(db, Arc::new(probe)).await;
    let resp = app.get("/api/v1/health").await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    assert_eq!(
        resp.json::<serde_json::Value>(),
        json!({
            "status": "healthy",
            "services": { "database": "up", "redis": "up", "kafka": "connected" }
        })
    );
}

#[tokio::test]
async fn redis_probe_pings_test_redis() {
    let probe = RedisHealthProbe::new(common::test_redis().await, false);
    probe.ping_redis().await.expect("PING the test Redis");
    assert!(!probe.kafka_connected());
}
