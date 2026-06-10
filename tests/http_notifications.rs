//! HTTP integration tests for `/notifications/*` endpoints.

mod common;

use common::fixtures::seed_notification;
use common::http::spawn_test_app;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn list_notifications_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/notifications").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn list_notifications_returns_only_own(db: PgPool) {
    let app = spawn_test_app(db).await;
    let alice = app.register_member("alice-n@example.com", "Password!234").await;
    let bob = app.register_member("bob-n@example.com", "Password!234").await;
    seed_notification(&app.db, alice.user_id, "Alice#1", false).await;
    seed_notification(&app.db, alice.user_id, "Alice#2", false).await;
    seed_notification(&app.db, bob.user_id, "Bob#1", false).await;

    let resp = app
        .get("/api/v1/notifications")
        .authorization_bearer(&alice.access_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|n| n["title"].as_str().unwrap().starts_with("Alice")));
}

#[sqlx::test]
async fn unread_count_returns_correct_number(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("n-u@example.com", "Password!234").await;
    seed_notification(&app.db, user.user_id, "a", false).await;
    seed_notification(&app.db, user.user_id, "b", false).await;
    seed_notification(&app.db, user.user_id, "c", true).await; // already read

    let resp = app
        .get("/api/v1/notifications/unread-count")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: serde_json::Value = resp.json();
    // Either field name: `count` or `unread_count` — just check the i64 value.
    let count = body
        .as_object()
        .unwrap()
        .values()
        .find_map(|v| v.as_i64())
        .expect("numeric count field");
    assert_eq!(count, 2);
}

#[sqlx::test]
async fn mark_read_flips_flag(db: PgPool) {
    let app = spawn_test_app(db).await;
    let user = app.register_member("n-m@example.com", "Password!234").await;
    let nid = seed_notification(&app.db, user.user_id, "hello", false).await;

    let resp = app
        .patch(&format!("/api/v1/notifications/{nid}/read"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());

    // DB check.
    let (is_read,): (bool,) = sqlx::query_as("SELECT is_read FROM notifications WHERE id = $1")
        .bind(nid)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(is_read);
}

#[sqlx::test]
async fn mark_read_other_users_notification_is_forbidden(db: PgPool) {
    let app = spawn_test_app(db).await;
    let alice = app.register_member("a-n@example.com", "Password!234").await;
    let bob = app.register_member("b-n@example.com", "Password!234").await;
    let nid = seed_notification(&app.db, alice.user_id, "secret", false).await;

    let resp = app
        .patch(&format!("/api/v1/notifications/{nid}/read"))
        .authorization_bearer(&bob.access_token)
        .await;
    // The service should refuse a cross-user mark: either Forbidden or NotFound.
    assert!(matches!(resp.status_code().as_u16(), 403 | 404));
    // Confirm Alice's record is still unread.
    let (is_read,): (bool,) = sqlx::query_as("SELECT is_read FROM notifications WHERE id = $1")
        .bind(nid)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(!is_read);
}

#[sqlx::test]
async fn unread_count_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app.get("/api/v1/notifications/unread-count").await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn mark_read_without_auth_returns_401(db: PgPool) {
    let app = spawn_test_app(db).await;
    let resp = app
        .patch(&format!("/api/v1/notifications/{}/read", Uuid::now_v7()))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[sqlx::test]
async fn unread_count_decreases_after_mark_read(db: PgPool) {
    // Seed two unread → count=2. Mark one as read → count=1.
    // Defends the `unread_count` query path against any future change
    // that forgets to filter on `is_read = false`.
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("n-dec@example.com", "Password!234")
        .await;
    let first = seed_notification(&app.db, user.user_id, "#1", false).await;
    seed_notification(&app.db, user.user_id, "#2", false).await;

    let resp = app
        .get("/api/v1/notifications/unread-count")
        .authorization_bearer(&user.access_token)
        .await;
    let before: serde_json::Value = resp.json();
    let before_count = before
        .as_object()
        .unwrap()
        .values()
        .find_map(|v| v.as_i64())
        .unwrap();
    assert_eq!(before_count, 2);

    let mr = app
        .patch(&format!("/api/v1/notifications/{first}/read"))
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(mr.status_code(), 200);

    let resp2 = app
        .get("/api/v1/notifications/unread-count")
        .authorization_bearer(&user.access_token)
        .await;
    let after: serde_json::Value = resp2.json();
    let after_count = after
        .as_object()
        .unwrap()
        .values()
        .find_map(|v| v.as_i64())
        .unwrap();
    assert_eq!(after_count, 1);
}

#[sqlx::test]
async fn list_notifications_respects_pagination(db: PgPool) {
    // Seed 5 notifications. page=1 per_page=2 → 2 rows; page=3 per_page=2 → 1 row.
    let app = spawn_test_app(db).await;
    let user = app
        .register_member("n-pag@example.com", "Password!234")
        .await;
    for i in 0..5 {
        seed_notification(&app.db, user.user_id, &format!("n{i}"), false).await;
    }

    let p1 = app
        .get("/api/v1/notifications?page=1&per_page=2")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(p1.status_code(), 200);
    assert_eq!(p1.json::<serde_json::Value>().as_array().unwrap().len(), 2);

    let p3 = app
        .get("/api/v1/notifications?page=3&per_page=2")
        .authorization_bearer(&user.access_token)
        .await;
    assert_eq!(p3.status_code(), 200);
    assert_eq!(p3.json::<serde_json::Value>().as_array().unwrap().len(), 1);
}
