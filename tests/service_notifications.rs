//! Service-layer tests for `modules::notifications::service`.
//!
//! Covers the read/write path and, importantly, the helper functions used
//! by other modules (`send_booking_confirmation`, `send_order_update`)
//! which the HTTP suite cannot reach directly.

mod common;

use common::seed_member;
use common::fixtures::seed_notification;
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

    let (is_read,): (bool,) =
        sqlx::query_as("SELECT is_read FROM notifications WHERE id = $1")
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
    let (is_read,): (bool,) =
        sqlx::query_as("SELECT is_read FROM notifications WHERE id = $1")
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
async fn send_booking_confirmation_inserts_row_with_metadata(db: PgPool) {
    // Exercises the helper that other modules (bookings service) call to
    // notify a user after a successful booking. Verifies both the row is
    // created and the `metadata` JSON contains the booking id.
    let user = seed_member(&db, "bn@example.com", "Password!234").await;
    let booking_id = Uuid::now_v7();

    service::send_booking_confirmation(&db, user, booking_id)
        .await
        .unwrap();

    let (title, metadata): (String, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT title, metadata FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user)
    .fetch_one(&db)
    .await
    .unwrap();

    assert_eq!(title, "Booking Confirmed");
    let md = metadata.expect("metadata column populated");
    assert_eq!(
        md["booking_id"].as_str().unwrap(),
        booking_id.to_string()
    );
}

#[sqlx::test]
async fn send_order_update_inserts_row_with_status_metadata(db: PgPool) {
    let user = seed_member(&db, "on@example.com", "Password!234").await;
    let order_id = Uuid::now_v7();

    service::send_order_update(&db, user, order_id, "paid")
        .await
        .unwrap();

    let (title, message, metadata): (String, String, Option<serde_json::Value>) =
        sqlx::query_as(
            "SELECT title, message, metadata FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();

    assert_eq!(title, "Order Update");
    assert!(message.contains("paid"));
    let md = metadata.expect("metadata populated");
    assert_eq!(md["status"].as_str().unwrap(), "paid");
    assert_eq!(md["order_id"].as_str().unwrap(), order_id.to_string());
}
