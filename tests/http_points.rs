//! HTTP integration tests for `/points/me`.

mod common;

use chrono::{Duration, Utc};
use common::fixtures::{seed_point_ledger_entry, set_points_balance};
use common::http::spawn_test_app;
use sqlx::PgPool;

#[sqlx::test]
async fn me_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/points/me").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn me_returns_balance_and_ledger_newest_first_only_mine(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user_a = app
        .register_member("pts-me-a@example.com", "Password!234")
        .await;
    let user_b = app
        .register_member("pts-me-b@example.com", "Password!234")
        .await;

    set_points_balance(&app.db, user_a.user_id, 150).await;

    // Someone else's ledger row must not leak into user_a's response.
    seed_point_ledger_entry(
        &app.db,
        user_b.user_id,
        10,
        10,
        "checkout_earn",
        None,
        Utc::now(),
    )
    .await;

    let older_id = seed_point_ledger_entry(
        &app.db,
        user_a.user_id,
        200,
        200,
        "checkout_earn",
        None,
        Utc::now() - Duration::days(1),
    )
    .await;
    let newer_id = seed_point_ledger_entry(
        &app.db,
        user_a.user_id,
        -50,
        150,
        "checkout_redeem",
        None,
        Utc::now(),
    )
    .await;

    let resp = app
        .get("/api/v1/points/me")
        .authorization_bearer(&user_a.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();

    assert_eq!(body["balance"], 150);
    assert_eq!(body["total"], 2, "must not count other users' entries");
    assert_eq!(body["page"], 1);
    assert_eq!(body["per_page"], 20, "default per_page");

    let ledger = body["ledger"].as_array().expect("ledger array");
    assert_eq!(ledger.len(), 2, "must not include other users' entries");

    assert_eq!(ledger[0]["id"], newer_id.to_string(), "newest first");
    assert_eq!(ledger[0]["delta"], -50);
    assert_eq!(ledger[0]["balance_after"], 150);
    assert_eq!(ledger[0]["reason"], "checkout_redeem");
    assert!(ledger[0]["order_id"].is_null());
    assert!(ledger[0]["created_at"].as_str().is_some());

    assert_eq!(ledger[1]["id"], older_id.to_string());
    assert_eq!(ledger[1]["delta"], 200);
    assert_eq!(ledger[1]["balance_after"], 200);
    assert_eq!(ledger[1]["reason"], "checkout_earn");
    assert!(ledger[1]["order_id"].is_null());
}

#[sqlx::test]
async fn me_paginates_newest_first(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("pts-page@example.com", "Password!234")
        .await;

    // Oldest to newest; ids[0] is oldest, ids[2] is newest.
    let mut ids = Vec::new();
    for i in 0..3i64 {
        let id = seed_point_ledger_entry(
            &app.db,
            user.user_id,
            10,
            10 * (i + 1),
            "checkout_earn",
            None,
            Utc::now() - Duration::hours(3 - i),
        )
        .await;
        ids.push(id);
    }

    let resp = app
        .get("/api/v1/points/me?page=2&per_page=2")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: serde_json::Value = resp.json();

    assert_eq!(body["total"], 3);
    assert_eq!(body["page"], 2);
    assert_eq!(body["per_page"], 2);

    let ledger = body["ledger"].as_array().expect("ledger array");
    assert_eq!(ledger.len(), 1, "last page has 1 item");
    assert_eq!(
        ledger[0]["id"],
        ids[0].to_string(),
        "oldest entry lands on page 2 of a newest-first, per_page=2 listing"
    );
}
