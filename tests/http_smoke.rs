//! Harness smoke test. Verifies that `spawn_test_app` can build the full
//! router and that a call passes rate-limit middleware using the synthetic
//! X-Forwarded-For header, and pins the scene instant (`common::scene_now`).
//! The `/health` body lives in `tests/http_health.rs`.

mod common;

use chrono::{DateTime, Utc};
use common::http::spawn_test_app;
use sqlx::PgPool;

#[sqlx::test]
async fn harness_missing_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    // `/users/me` requires AuthUser. Without a token we expect 401.
    let resp = app.get("/api/v1/users/me").await;
    assert_eq!(resp.status_code(), 401);
}

/// The scene instant is in UTC and already at Postgres precision, so an
/// instant written to a `timestamptz` column reads back equal.
#[sqlx::test]
async fn scene_now_is_utc_and_survives_a_timestamptz_round_trip(db: PgPool) {
    let scene = common::scene_now();
    assert_eq!(scene.tz, chrono_tz::UTC);

    let stored: DateTime<Utc> = sqlx::query_scalar("SELECT $1::timestamptz")
        .bind(scene.now)
        .fetch_one(&db)
        .await
        .expect("round-trip scene instant");
    assert_eq!(stored, scene.now);
}
