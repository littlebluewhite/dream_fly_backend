//! Harness smoke test. Verifies that `spawn_test_app` can build the full
//! router and that a call passes rate-limit middleware using the synthetic
//! X-Forwarded-For header, and pins the scene instant (`common::scene_now`).
//! The `/health` body lives in `tests/http_health.rs`.

mod common;

use chrono::{DateTime, Duration, SubsecRound, TimeZone, Utc};
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

/// `TestApp` opens with its clock pinned at the scene instant: it does not
/// move on its own, and `advance` moves it without a prior `set`.
#[sqlx::test]
async fn test_app_opens_with_the_clock_pinned(db: PgPool) {
    let app = spawn_test_app(db).await;
    let opened = app.studio_now().now;
    assert_eq!(opened, opened.trunc_subsecs(6), "pinned at DB precision");

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    assert_eq!(app.studio_now().now, opened, "the clock moved on its own");

    app.clock.advance(Duration::minutes(5));
    assert_eq!(app.studio_now().now, opened + Duration::minutes(5));
}

/// The users `TestApp` seeds join at the app's "now", not the wall clock.
#[sqlx::test]
async fn test_app_seeded_users_join_at_the_apps_now(db: PgPool) {
    let app = spawn_test_app(db).await;
    let t = Utc.with_ymd_and_hms(2020, 1, 15, 3, 0, 0).unwrap();
    app.clock.set(t);

    let (admin_id, _) = app.seed_admin().await;
    let coach = app.seed_coach_user().await;

    for user_id in [admin_id, coach.user_id] {
        let created_at: DateTime<Utc> =
            sqlx::query_scalar("SELECT created_at FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&app.db)
                .await
                .expect("load users.created_at");
        assert_eq!(created_at, t);
    }
}
