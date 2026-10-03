//! Integration tests for `bookings::service`.
//!
//! Covers:
//! - happy-path create_booking occupies a seat (slot reads `booked = 1`)
//! - duplicate booking rejected by the `uq_bookings_user_slot_active` index
//! - full slot rejected by the locked count (`occupying_bookings >= capacity`)
//! - cancel_booking is idempotent and frees the seat exactly once
//! - 24-hour cancellation rule blocks non-admin cancels of imminent slots
//! - concurrent create_booking on a capacity=1 slot: only one wins
//! - the `occupying_bookings` view's status list equals `occupies_seat()`

mod common;

use chrono::{Duration, Utc};
use common::fixtures::TimeSlotSeed;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::bookings::dto::CreateBookingRequest;
use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::modules::bookings::service;

#[sqlx::test]
async fn create_booking_increments_slot_booked(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;

    let booking = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
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
    let (title, _) = common::latest_notification(&db, user, "booking_confirmed")
        .await
        .expect("booking confirmation notification row");
    assert_eq!(title, "Booking Confirmed");
}

#[sqlx::test]
async fn duplicate_booking_same_slot_rejected_by_unique_index(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;

    service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
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
        common::studio_now_utc(Utc::now()),
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

    // The slot should count exactly one booking. The failed second attempt
    // rolled back its insert.
    assert_eq!(common::slot_booked(&db, slot).await, 1);
}

#[sqlx::test]
async fn full_slot_rejects_new_booking(db: PgPool) {
    let user_a = common::seed_member(&db, "a@example.com", "passw0rd!").await;
    let user_b = common::seed_member(&db, "b@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(1).insert(&db).await;

    service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
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
        common::studio_now_utc(Utc::now()),
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

/// Step 8: an admin-closed slot (`is_closed`) rejects new bookings the same
/// way a full one does — `occupy_slot_tx` folds three causes
/// (missing/full/closed) into the same `None` branch, so the message is
/// shared across all three.
#[sqlx::test]
async fn closed_slot_rejects_new_booking(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;
    sqlx::query("UPDATE time_slots SET is_closed = true WHERE id = $1")
        .bind(slot)
        .execute(&db)
        .await
        .expect("close slot");

    let err = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect_err("closed slot should reject booking");
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m == "time slot is full or closed"),
        "got: {err:?}"
    );
    // The rejected attempt must not have occupied a seat.
    assert_eq!(common::slot_booked(&db, slot).await, 0);
}

#[sqlx::test]
async fn cancel_booking_frees_seat_and_is_idempotent(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;
    let auth = common::member_auth(user);

    let booking = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
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

    service::cancel_booking(&db, common::studio_now_utc(Utc::now()), &auth, booking.id, None)
        .await
        .expect("first cancel");
    assert_eq!(common::slot_booked(&db, slot).await, 0);

    // A "Booking Cancelled" notification is written post-commit.
    let (title, _) = common::latest_notification(&db, user, "booking_cancelled")
        .await
        .expect("booking cancellation notification row");
    assert_eq!(title, "Booking Cancelled");

    // Second cancel of the same booking should fail cleanly (not free the
    // seat twice).
    let err = service::cancel_booking(&db, common::studio_now_utc(Utc::now()), &auth, booking.id, None)
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
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let auth = common::member_auth(user);

    // Schedule a slot for a few hours from now (within 24h window, but
    // still in the future so create_booking doesn't reject it for being
    // in the past). We do this by seeding the slot row directly with the
    // current UTC date + a start_time in the near future.
    let soon = (Utc::now() + Duration::hours(3)).date_naive();
    let slot = TimeSlotSeed::new(5)
        .on(soon)
        .start((Utc::now() + Duration::hours(3)).time())
        .insert(&db)
        .await;

    let booking = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect("create booking");

    let err = service::cancel_booking(&db, common::studio_now_utc(Utc::now()), &auth, booking.id, None)
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
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;

    // `TimeSlotSeed` relies on the column default (0) — bump it to a
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
        common::studio_now_utc(Utc::now()),
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
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;
    sqlx::query("UPDATE time_slots SET price_cents = $2 WHERE id = $1")
        .bind(slot)
        .bind(12_345_i64)
        .execute(&db)
        .await
        .expect("bump slot price");
    let auth = common::member_auth(user);

    let booking = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
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

    let cancelled = service::cancel_booking(&db, common::studio_now_utc(Utc::now()), &auth, booking.id, None)
        .await
        .expect("cancel booking");
    assert_eq!(
        cancelled.price_cents, 12_345,
        "cancel must not touch price_cents — reports filter by status, not by zeroing this out"
    );
}

#[sqlx::test]
async fn concurrent_book_last_slot_only_one_wins(db: PgPool) {
    // Capacity 1, two users racing. Only one should succeed and the slot
    // should read booked = 1.
    let user_a = common::seed_member(&db, "a@example.com", "passw0rd!").await;
    let user_b = common::seed_member(&db, "b@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(1).insert(&db).await;

    let db_a = Arc::new(db.clone());
    let db_b = Arc::new(db.clone());

    let task_a = tokio::spawn(async move {
        service::create_booking(
            db_a.as_ref(),
            common::studio_now_utc(Utc::now()),
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
            common::studio_now_utc(Utc::now()),
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

// ---------------------------------------------------------------------
// Pin tests: `bookings::occupancy` 收攏前既有的三個分類/優先序行為——
// 釘住行為不變,不是新行為。
// ---------------------------------------------------------------------

/// 現況無測試釘住的分類:佔位的鎖列 `WHERE id = $1 AND is_closed = false`
/// 與其後的計數把「slot 不存在」與「已滿/已關閉」摺進同一個 `None`,一律
/// 報同一句 400,不升級為 404。
#[sqlx::test]
async fn create_booking_missing_slot_maps_to_full_or_closed_bad_request(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let missing_slot = Uuid::now_v7();

    let err = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
        user,
        CreateBookingRequest {
            time_slot_id: missing_slot,
            note: None,
        },
        None,
    )
    .await
    .expect_err("nonexistent slot should reject booking");

    assert!(
        matches!(err, AppError::BadRequest(ref m) if m == "time slot is full or closed"),
        "got: {err:?}"
    );
}

/// 錯誤優先序 = 不重排裁決的機器見證:佔位檢查先於已開始檢查執行,一個
/// 既滿又已開始的 slot 必須報「已滿」,不是「已開始」。
#[sqlx::test]
async fn create_booking_full_and_started_slot_reports_full_not_started(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let past_date = (Utc::now() - Duration::days(1)).date_naive();
    let past_time = (Utc::now() - Duration::days(1)).time();
    let slot = TimeSlotSeed::new(1).on(past_date).start(past_time).insert(&db).await;
    let other = common::seed_member(&db, "other@example.com", "passw0rd!").await;
    common::fixtures::seed_booking(&db, other, slot, BookingStatus::Confirmed, 0).await;

    let err = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
        user,
        CreateBookingRequest {
            time_slot_id: slot,
            note: None,
        },
        None,
    )
    .await
    .expect_err("full and already-started slot should reject booking");

    assert!(
        matches!(err, AppError::BadRequest(ref m) if m == "time slot is full or closed"),
        "got: {err:?} (must report full/closed, not 'already started')"
    );
}

/// closed 只 gate 新佔位(occupy),不 gate 既有預約的取消釋出——admin
/// 事後關閉的 slot,既有預約仍必須能正常取消並釋出座位。
#[sqlx::test]
async fn cancel_booking_on_closed_slot_still_releases_seat(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;
    let auth = common::member_auth(user);

    let booking = service::create_booking(
        &db,
        common::studio_now_utc(Utc::now()),
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

    // Admin closes the slot *after* the booking already exists.
    sqlx::query("UPDATE time_slots SET is_closed = true WHERE id = $1")
        .bind(slot)
        .execute(&db)
        .await
        .expect("close slot");

    service::cancel_booking(&db, common::studio_now_utc(Utc::now()), &auth, booking.id, None)
        .await
        .expect("cancel on closed slot should still succeed");

    assert_eq!(common::slot_booked(&db, slot).await, 0);
}

#[sqlx::test]
async fn seeded_confirmed_booking_occupies_seat_so_cancel_frees_it(db: PgPool) {
    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let slot = TimeSlotSeed::new(5).insert(&db).await;
    let auth = common::member_auth(user);
    let booking =
        common::fixtures::seed_booking(&db, user, slot, BookingStatus::Confirmed, 1_000).await;
    assert_eq!(
        common::slot_booked(&db, slot).await,
        1,
        "seeded booking occupies a seat"
    );

    let now = common::studio_now_utc(Utc::now());
    service::cancel_booking(&db, now, &auth, booking, None)
        .await
        .expect("cancel seeded booking");

    assert_eq!(common::slot_booked(&db, slot).await, 0);
}

/// `occupying_bookings` view 的狀態清單必須恰為 `BookingStatus::occupies_seat()`
/// 為真的變體——view 是 SQL 端的佔位謂詞,`occupies_seat` 是 Rust 端的,兩份
/// 定義靠本測試鎖在一起。每個變體各插一筆 booking,view 讀出的 id 集合必須
/// 等於 `occupies_seat()` 為真的那幾筆。
#[sqlx::test]
async fn occupying_bookings_view_matches_occupies_seat(db: PgPool) {
    let all_statuses = [
        BookingStatus::Pending,
        BookingStatus::Confirmed,
        BookingStatus::Cancelled,
        BookingStatus::Completed,
        BookingStatus::NoShow,
    ];
    for status in &all_statuses {
        // Tripwire:窮盡 match、無 `_` arm。新增 BookingStatus 變體時本行
        // 編譯錯誤——先決定它佔不佔位,同步 `occupies_seat()` 與
        // `occupying_bookings` view(新 migration),再把它加進上面的清單。
        match status {
            BookingStatus::Pending
            | BookingStatus::Confirmed
            | BookingStatus::Cancelled
            | BookingStatus::Completed
            | BookingStatus::NoShow => {}
        }
    }
    // DB 端的 tripwire:PG enum 多出 Rust 沒有的值(只加 migration、沒加
    // 變體)時,上面的手列清單就不再窮盡。
    let pg_labels: Vec<String> =
        sqlx::query_scalar("SELECT unnest(enum_range(NULL::booking_status))::text")
            .fetch_all(&db)
            .await
            .expect("booking_status labels");
    let rust_labels: Vec<&str> = all_statuses.iter().map(BookingStatus::as_str).collect();
    assert_eq!(pg_labels, rust_labels);

    let user = common::seed_member(&db, "u@example.com", "passw0rd!").await;
    let mut expected = Vec::new();
    for status in all_statuses {
        let slot = TimeSlotSeed::new(5).insert(&db).await;
        let occupies_seat = status.occupies_seat();
        let booking = common::fixtures::seed_booking(&db, user, slot, status, 0).await;
        if occupies_seat {
            expected.push(booking);
        }
    }

    let mut in_view: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM occupying_bookings")
        .fetch_all(&db)
        .await
        .expect("read occupying_bookings");
    in_view.sort();
    expected.sort();
    assert_eq!(in_view, expected);
}
