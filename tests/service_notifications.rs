//! Service-layer tests for `modules::notifications::service`.
//!
//! Covers the read path and the public domain verbs used by other modules
//! (`order_placed`, `booking_confirmed`, `user_welcomed`) which the HTTP
//! suite cannot reach directly.

mod common;

use common::fixtures::seed_notification;
use common::seed_member;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::notifications::service;

fn default_pagination() -> PaginationParams {
    // Fields are `pub`, so construct directly.
    PaginationParams {
        page: 1,
        per_page: 20,
    }
}

#[sqlx::test]
async fn list_returns_only_own_notifications(db: PgPool) {
    let alice = seed_member(&db, "a@example.com", "Password!234").await;
    let bob = seed_member(&db, "b@example.com", "Password!234").await;
    seed_notification(&db, alice, "alice-1", false).await;
    seed_notification(&db, alice, "alice-2", true).await;
    seed_notification(&db, bob, "bob-1", false).await;

    let p = default_pagination();
    let alice_list = service::list_notifications(&db, alice, &p).await.unwrap();
    assert_eq!(alice_list.len(), 2);
    assert!(alice_list.iter().all(|n| n.title.starts_with("alice")));
}

#[sqlx::test]
async fn unread_count_excludes_already_read(db: PgPool) {
    let user = seed_member(&db, "u@example.com", "Password!234").await;
    seed_notification(&db, user, "u1", false).await;
    seed_notification(&db, user, "u2", false).await;
    seed_notification(&db, user, "u3", true).await;

    let count = service::get_unread_count(&db, user).await.unwrap();
    assert_eq!(count.count, 2);
}

#[sqlx::test]
async fn unread_count_for_user_with_no_notifications_is_zero(db: PgPool) {
    let user = seed_member(&db, "u0@example.com", "Password!234").await;
    let count = service::get_unread_count(&db, user).await.unwrap();
    assert_eq!(count.count, 0);
}

#[sqlx::test]
async fn mark_as_read_flips_flag_on_own_notification(db: PgPool) {
    let user = seed_member(&db, "m1@example.com", "Password!234").await;
    let id = seed_notification(&db, user, "hello", false).await;

    let resp = service::mark_as_read(&db, id, user).await.unwrap();
    assert!(resp.is_read);

    let (is_read,): (bool,) = sqlx::query_as("SELECT is_read FROM notifications WHERE id = $1")
        .bind(id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(is_read);
}

#[sqlx::test]
async fn mark_as_read_cross_user_returns_not_found(db: PgPool) {
    let alice = seed_member(&db, "ma@example.com", "Password!234").await;
    let bob = seed_member(&db, "mb@example.com", "Password!234").await;
    let id = seed_notification(&db, alice, "secret", false).await;

    // Bob tries to mark Alice's notification. The repository's WHERE clause
    // includes `user_id = $2`, so the row isn't found for Bob → NotFound.
    let err = service::mark_as_read(&db, id, bob).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));

    // Alice's notification is still unread.
    let (is_read,): (bool,) = sqlx::query_as("SELECT is_read FROM notifications WHERE id = $1")
        .bind(id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(!is_read);
}

#[sqlx::test]
async fn mark_as_read_unknown_id_returns_not_found(db: PgPool) {
    let user = seed_member(&db, "m2@example.com", "Password!234").await;
    let err = service::mark_as_read(&db, Uuid::now_v7(), user)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn order_placed_writes_exactly_one_correct_row(db: PgPool) {
    // The public verb other modules (orders service) call after checkout.
    // Verifies exactly one row with the converged type/title/message/metadata.
    let user = seed_member(&db, "on@example.com", "Password!234").await;
    let order_id = Uuid::now_v7();

    service::order_placed(user, order_id, "ORD-123")
        .deliver(&db)
        .await;

    let rows: Vec<(String, String, String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT type::text, title, message, metadata FROM notifications WHERE user_id = $1",
    )
    .bind(user)
    .fetch_all(&db)
    .await
    .unwrap();

    assert_eq!(rows.len(), 1, "exactly one notification row");
    let (notif_type, title, message, metadata) = &rows[0];
    assert_eq!(notif_type, "order_placed");
    assert_eq!(title, "Order Placed");
    assert_eq!(message, "Your order ORD-123 has been placed.");
    let md = metadata.as_ref().expect("metadata populated");
    assert_eq!(md["order_id"].as_str().unwrap(), order_id.to_string());
    assert_eq!(md["order_number"].as_str().unwrap(), "ORD-123");
}

#[sqlx::test]
async fn booking_confirmed_writes_exactly_one_correct_row(db: PgPool) {
    let user = seed_member(&db, "bn@example.com", "Password!234").await;
    let booking_id = Uuid::now_v7();

    service::booking_confirmed(user, booking_id)
        .deliver(&db)
        .await;

    let rows: Vec<(String, String, String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT type::text, title, message, metadata FROM notifications WHERE user_id = $1",
    )
    .bind(user)
    .fetch_all(&db)
    .await
    .unwrap();

    assert_eq!(rows.len(), 1, "exactly one notification row");
    let (notif_type, title, message, metadata) = &rows[0];
    assert_eq!(notif_type, "booking_confirmed");
    assert_eq!(title, "Booking Confirmed");
    assert_eq!(message, "Your booking has been confirmed.");
    let md = metadata.as_ref().expect("metadata populated");
    assert_eq!(md["booking_id"].as_str().unwrap(), booking_id.to_string());
}

#[sqlx::test]
async fn user_welcomed_writes_exactly_one_correct_row(db: PgPool) {
    let user = seed_member(&db, "wn@example.com", "Password!234").await;

    service::user_welcomed(user).deliver(&db).await;

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT type::text, title FROM notifications WHERE user_id = $1")
            .bind(user)
            .fetch_all(&db)
            .await
            .unwrap();

    assert_eq!(rows.len(), 1, "exactly one notification row");
    let (notif_type, title) = &rows[0];
    assert_eq!(notif_type, "system");
    assert_eq!(title, "Welcome to Dream Fly");
}

#[sqlx::test]
async fn pending_without_deliver_writes_nothing(db: PgPool) {
    // Constructing a PendingNotification must be pure IO-wise — only
    // `.deliver` writes. Build one for a real user (so a wrongly-called
    // `.deliver` would actually succeed and be visible), drop it without
    // ever delivering, and confirm the notifications table stays empty.
    let user = seed_member(&db, "nodeliver@example.com", "Password!234").await;
    let pending = service::order_placed(user, Uuid::now_v7(), "ORD-999");
    drop(pending);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "constructing a PendingNotification must not write anything"
    );
}
