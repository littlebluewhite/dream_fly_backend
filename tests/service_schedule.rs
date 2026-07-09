//! Service-layer tests for `modules::schedule::service`.
//!
//! These drive the service functions directly (no HTTP) so they can
//! exercise error branches that are either validator-rejected before
//! reaching the service in integration tests, or that would require
//! ugly test fixtures to hit through the full router.

mod common;

use chrono::{Datelike, Duration, Utc};
use common::fixtures::seed_time_slot_full;
use sqlx::PgPool;

use dream_fly_backend::config::ServerConfig;
use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::schedule::dto::{
    AvailabilityQuery, CreateSlotsRequest, ScheduleQuery, SlotEntry,
};
use dream_fly_backend::modules::schedule::service;

fn utc_server() -> ServerConfig {
    ServerConfig {
        host: "0.0.0.0".into(),
        port: 3000,
        allowed_origins: vec![],
        trust_proxy: false,
        studio_timezone: "UTC".into(),
    }
}

/// Build a `SlotEntry` `n` days in the future with explicit times.
fn future_slot(days: i64, start: &str, end: &str, capacity: i32) -> SlotEntry {
    let date = (Utc::now() + Duration::days(days)).date_naive();
    SlotEntry {
        date: date.to_string(),
        start_time: start.into(),
        end_time: end.into(),
        venue_id: None,
        course_id: None,
        capacity,
        price_cents: None,
    }
}

#[sqlx::test]
async fn create_slots_happy_path_persists_rows(db: PgPool) {
    let req = CreateSlotsRequest {
        slots: vec![
            future_slot(2, "09:00", "10:00", 10),
            future_slot(2, "10:00", "11:00", 8),
        ],
    };
    let resp = service::create_slots(&db, &utc_server(), req).await.unwrap();
    assert_eq!(resp.len(), 2);
    assert_eq!(resp[0].capacity, 10);
    assert_eq!(resp[1].capacity, 8);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM time_slots")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test]
async fn create_slots_rejects_end_before_start(db: PgPool) {
    let req = CreateSlotsRequest {
        slots: vec![future_slot(2, "11:00", "10:00", 10)],
    };
    let err = service::create_slots(&db, &utc_server(), req).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("end_time")),
        "got {err:?}"
    );
    // Nothing should have landed in the DB.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM time_slots")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn create_slots_rejects_past_date(db: PgPool) {
    let req = CreateSlotsRequest {
        slots: vec![future_slot(-1, "09:00", "10:00", 10)],
    };
    let err = service::create_slots(&db, &utc_server(), req).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("past")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn create_slots_rejects_zero_capacity(db: PgPool) {
    let req = CreateSlotsRequest {
        slots: vec![future_slot(2, "09:00", "10:00", 0)],
    };
    let err = service::create_slots(&db, &utc_server(), req).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("capacity")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn create_slots_rolls_back_on_mid_batch_failure(db: PgPool) {
    // First entry is valid, second is past → whole batch should fail and
    // the valid first slot must NOT be visible after the error returns.
    let req = CreateSlotsRequest {
        slots: vec![
            future_slot(2, "09:00", "10:00", 10),
            future_slot(-2, "09:00", "10:00", 10),
        ],
    };
    let err = service::create_slots(&db, &utc_server(), req).await.unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM time_slots")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 0, "validation failure must not persist any slot");
}

#[sqlx::test]
async fn get_monthly_schedule_validates_month_range(db: PgPool) {
    let err = service::get_monthly_schedule(
        &db,
        ScheduleQuery {
            year: 2030,
            month: 13,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)));
}

#[sqlx::test]
async fn get_monthly_schedule_returns_seeded_slot(db: PgPool) {
    let _slot_id = seed_time_slot_full(&db, None, None, 5).await;
    let now = Utc::now();
    // `seed_time_slot_full` inserts 2 days from now, which is usually the
    // same month — if we've rolled into a new month, query that one.
    let target_date = now + Duration::days(2);
    let resp = service::get_monthly_schedule(
        &db,
        ScheduleQuery {
            year: target_date.year(),
            month: target_date.month(),
        },
    )
    .await
    .unwrap();
    // Must contain exactly one day with one slot.
    let total: usize = resp.iter().map(|d| d.slots.len()).sum();
    assert_eq!(total, 1);
}

// ---------------------------------------------------------------------
// Task P4-B2: `time_slots.price_cents` (venue-rental price snapshot source)
// ---------------------------------------------------------------------

#[sqlx::test]
async fn create_slots_persists_price_cents_and_defaults_to_zero(db: PgPool) {
    let mut priced = future_slot(2, "09:00", "10:00", 10);
    priced.price_cents = Some(50_000);
    let unpriced = future_slot(2, "10:00", "11:00", 8);

    let req = CreateSlotsRequest {
        slots: vec![priced, unpriced],
    };
    let resp = service::create_slots(&db, &utc_server(), req).await.unwrap();
    assert_eq!(resp[0].price_cents, 50_000);
    // Omitted `price_cents` must default to 0, not fail to deserialize.
    assert_eq!(resp[1].price_cents, 0);
}

#[sqlx::test]
async fn get_availability_rejects_bad_date_format(db: PgPool) {
    let err = service::get_availability(
        &db,
        AvailabilityQuery {
            date: "2030/01/01".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("YYYY-MM-DD")),
        "got {err:?}"
    );
}
