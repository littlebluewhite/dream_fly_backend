//! Integration tests for `bookings::service`.
//!
//! Covers:
//! - happy-path create_booking increments `time_slots.booked`
//! - duplicate booking rejected by the `uq_bookings_user_slot_active` index
//! - full slot rejected at the `booked < capacity` guard
//! - cancel_booking is idempotent and decrements the slot exactly once
//! - 24-hour cancellation rule blocks non-admin cancels of imminent slots
//! - concurrent create_booking on a capacity=1 slot: only one wins

mod common;

use chrono::{Duration, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::auth::AuthUser;
use dream_fly_backend::modules::bookings::dto::CreateBookingRequest;
use dream_fly_backend::modules::bookings::service;

fn member_auth(user_id: Uuid) -> AuthUser {
    AuthUser {
        user_id,
        email: "member@test".into(),
        roles: vec!["member".into()],
    }
}

#[sqlx::test]
async fn create_booking_increments_slot_booked(db: PgPool) {
    let server = common::test_server_config();
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = common::seed_time_slot(&db, 5).await;

    let booking = service::create_booking(
        &db,
        &server,
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("create booking");

    assert_eq!(booking.user_id, user);
    assert_eq!(booking.time_slot_id, slot);
    assert_eq!(common::slot_booked(&db, slot).await, 1);

    // A "Booking Confirmed" notification is written post-commit.
    let title: String = sqlx::query_scalar(
        "SELECT title FROM notifications WHERE user_id = $1 AND type = 'booking_confirmed'::notification_type",
    )
    .bind(user)
    .fetch_one(&db)
    .await
    .expect("booking confirmation notification row");
    assert_eq!(title, "Booking Confirmed");
}

#[sqlx::test]
async fn duplicate_booking_same_slot_rejected_by_unique_index(db: PgPool) {
    let server = common::test_server_config();
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = common::seed_time_slot(&db, 5).await;

    service::create_booking(
        &db,
        &server,
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("first booking");

    let err = service::create_booking(
        &db,
        &server,
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect_err("duplicate should fail");

    assert!(matches!(err, AppError::Conflict(_)), "got: {err:?}");

    // Slot counter should reflect exactly one successful booking. The failed
    // second attempt rolled back its increment.
    assert_eq!(common::slot_booked(&db, slot).await, 1);
}

#[sqlx::test]
async fn full_slot_rejects_new_booking(db: PgPool) {
    let server = common::test_server_config();
    let user_a = common::seed_member(&db, "a@example.com", "passw0rd!").await;
    let user_b = common::seed_member(&db, "b@example.com", "passw0rd!").await;
    let slot = common::seed_time_slot(&db, 1).await;

    service::create_booking(
        &db,
        &server,
        user_a,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("first booking");

    let err = service::create_booking(
        &db,
        &server,
        user_b,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect_err("second booking should fail");
    assert!(matches!(err, AppError::BadRequest(_)), "got: {err:?}");
}

#[sqlx::test]
async fn cancel_booking_decrements_slot_and_is_idempotent(db: PgPool) {
    let server = common::test_server_config();
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = common::seed_time_slot(&db, 5).await;
    let auth = member_auth(user);

    let booking = service::create_booking(
        &db,
        &server,
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("create booking");

    assert_eq!(common::slot_booked(&db, slot).await, 1);

    service::cancel_booking(&db, &server, &auth, booking.id, None)
        .await
        .expect("first cancel");
    assert_eq!(common::slot_booked(&db, slot).await, 0);

    // A "Booking Cancelled" notification is written post-commit.
    let title: String = sqlx::query_scalar(
        "SELECT title FROM notifications WHERE user_id = $1 AND type = 'booking_cancelled'::notification_type",
    )
    .bind(user)
    .fetch_one(&db)
    .await
    .expect("booking cancellation notification row");
    assert_eq!(title, "Booking Cancelled");

    // Second cancel of the same booking should fail cleanly (not underflow
    // the slot's booked counter).
    let err = service::cancel_booking(&db, &server, &auth, booking.id, None)
        .await
        .expect_err("second cancel should fail");
    // Either BadRequest("booking is already cancelled") or Conflict, both
    // are acceptable idempotency signals.
    assert!(
        matches!(err, AppError::BadRequest(_) | AppError::Conflict(_)),
        "got: {err:?}"
    );
    assert_eq!(common::slot_booked(&db, slot).await, 0);
}

#[sqlx::test]
async fn cancel_within_24h_rejected_for_non_admin(db: PgPool) {
    let server = common::test_server_config();
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let auth = member_auth(user);

    // Schedule a slot for a few hours from now (within 24h window, but
    // still in the future so create_booking doesn't reject it for being
    // in the past). We do this by seeding the slot row directly with the
    // current UTC date + a start_time in the near future.
    let soon = (Utc::now() + Duration::hours(3)).date_naive();
    let slot = common::seed_time_slot_on_with_start(
        &db,
        5,
        soon,
        (Utc::now() + Duration::hours(3)).time(),
    )
    .await;

    let booking = service::create_booking(
        &db,
        &server,
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("create booking");

    let err = service::cancel_booking(&db, &server, &auth, booking.id, None)
        .await
        .expect_err("within 24h should be rejected");
    assert!(matches!(err, AppError::BadRequest(_)), "got: {err:?}");

    // Booked count untouched since cancel failed.
    assert_eq!(common::slot_booked(&db, slot).await, 1);
}

// ---------------------------------------------------------------------
// Task P4-B2: `bookings.price_cents` (venue-rental price snapshot)
// ---------------------------------------------------------------------

#[sqlx::test]
async fn create_booking_snapshots_slot_price_and_survives_repricing(db: PgPool) {
    let server = common::test_server_config();
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = common::seed_time_slot(&db, 5).await;

    // `seed_time_slot` relies on the column default (0) — bump it to a
    // known non-zero price before booking so the snapshot assertion below
    // isn't trivially true.
    sqlx::query("UPDATE time_slots SET price_cents = $2 WHERE id = $1")
        .bind(slot)
        .bind(50_000_i64)
        .execute(&db)
        .await
        .expect("bump slot price");

    let booking = service::create_booking(
        &db,
        &server,
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("create booking");

    assert_eq!(booking.price_cents, 50_000);

    // Reprice the slot *after* booking — the existing booking's snapshot
    // must NOT change (price_cents is captured at booking time, not read
    // live off the slot on every fetch).
    sqlx::query("UPDATE time_slots SET price_cents = $2 WHERE id = $1")
        .bind(slot)
        .bind(99_999_i64)
        .execute(&db)
        .await
        .expect("reprice slot");

    let reloaded = dream_fly_backend::modules::bookings::repository::find_by_id(&db, booking.id)
        .await
        .expect("find booking")
        .expect("booking exists");
    assert_eq!(
        reloaded.price_cents, 50_000,
        "booking price must stay snapshotted after the slot is repriced"
    );
}

#[sqlx::test]
async fn cancel_booking_does_not_modify_price_cents(db: PgPool) {
    let server = common::test_server_config();
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = common::seed_time_slot(&db, 5).await;
    sqlx::query("UPDATE time_slots SET price_cents = $2 WHERE id = $1")
        .bind(slot)
        .bind(12_345_i64)
        .execute(&db)
        .await
        .expect("bump slot price");
    let auth = member_auth(user);

    let booking = service::create_booking(
        &db,
        &server,
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("create booking");
    assert_eq!(booking.price_cents, 12_345);

    let cancelled = service::cancel_booking(&db, &server, &auth, booking.id, None)
        .await
        .expect("cancel booking");
    assert_eq!(
        cancelled.price_cents, 12_345,
        "cancel must not touch price_cents — reports filter by status, not by zeroing this out"
    );
}

#[sqlx::test]
async fn concurrent_book_last_slot_only_one_wins(db: PgPool) {
    // Capacity 1, two users racing. Only one should succeed and
    // time_slots.booked should end at 1.
    let user_a = common::seed_member(&db, "a@example.com", "passw0rd!").await;
    let user_b = common::seed_member(&db, "b@example.com", "passw0rd!").await;
    let slot = common::seed_time_slot(&db, 1).await;

    let db_a = Arc::new(db.clone());
    let db_b = Arc::new(db.clone());
    let server = Arc::new(common::test_server_config());
    let server_a = server.clone();
    let server_b = server.clone();

    let task_a = tokio::spawn(async move {
        service::create_booking(
            db_a.as_ref(),
            server_a.as_ref(),
            user_a,
            CreateBookingRequest {
                time_slot_id: slot,
                note: None,
            },
            None,
        )
        .await
    });
    let task_b = tokio::spawn(async move {
        service::create_booking(
            db_b.as_ref(),
            server_b.as_ref(),
            user_b,
            CreateBookingRequest {
                time_slot_id: slot,
                note: None,
            },
            None,
        )
        .await
    });

    let (res_a, res_b) = tokio::join!(task_a, task_b);
    let res_a = res_a.expect("task a panicked");
    let res_b = res_b.expect("task b panicked");

    let ok_count = [res_a.is_ok(), res_b.is_ok()]
        .iter()
        .filter(|b| **b)
        .count();
    assert_eq!(ok_count, 1, "exactly one booking should succeed");

    assert_eq!(common::slot_booked(&db, slot).await, 1);

    let total_bookings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bookings")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(total_bookings, 1);
}
