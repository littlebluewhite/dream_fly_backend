//! Integration tests for `coaches::service`.
//!
//! Covered paths:
//! - `list_active` returns only active coaches (inactive row filtered out)
//! - `get_detail` nonexistent id → NotFound
//! - `get_schedules` returns entries in day_of_week order
//! - `update_schedules` by the owning user succeeds; by a stranger → Forbidden
//! - `update_schedules` by an admin on someone else's coach profile succeeds
//! - `update_schedules` with an unparseable time string → Validation (422)
//! - `update_schedules` with `end_time <= start_time` → Validation (422)
//! - `clock_in` twice without `clock_out` in between → Conflict
//!   (defends the `uq_clock_records_open` partial unique index)
//! - `clock_out` with no active clock-in → NotFound

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::coaches::dto::ScheduleEntry;
use dream_fly_backend::modules::coaches::service;

async fn set_coach_active(db: &PgPool, coach_id: Uuid, active: bool) {
    sqlx::query("UPDATE coaches SET is_active = $1 WHERE id = $2")
        .bind(active)
        .bind(coach_id)
        .execute(db)
        .await
        .expect("toggle coach active");
}

#[sqlx::test]
async fn list_active_filters_out_inactive_coaches(db: PgPool) {
    let u1 = common::seed_member(&db, "c1@example.com", "hunter22-secret").await;
    let u2 = common::seed_member(&db, "c2@example.com", "hunter22-secret").await;

    let active = common::fixtures::seed_coach(&db, u1, "Active Coach").await;
    let hidden = common::fixtures::seed_coach(&db, u2, "Retired Coach").await;
    set_coach_active(&db, hidden, false).await;

    let coaches = service::list_active(&db).await.expect("list_active");
    let ids: Vec<_> = coaches.iter().map(|c| c.id).collect();
    assert!(ids.contains(&active));
    assert!(!ids.contains(&hidden), "inactive coach leaked into list");
}

#[sqlx::test]
async fn get_detail_nonexistent_returns_not_found(db: PgPool) {
    let err = service::get_detail(&db, Uuid::now_v7()).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn update_schedules_by_owner_succeeds(db: PgPool) {
    let user_id = common::seed_member(&db, "owner@example.com", "hunter22-secret").await;
    let coach_id = common::fixtures::seed_coach(&db, user_id, "Owner").await;

    let entries = vec![
        ScheduleEntry {
            day_of_week: 1,
            start_time: "09:00:00".into(),
            end_time: "12:00:00".into(),
            is_available: true,
        },
        ScheduleEntry {
            day_of_week: 3,
            start_time: "14:00:00".into(),
            end_time: "17:00:00".into(),
            is_available: true,
        },
    ];

    let auth = common::auth_with_roles(user_id, &["member", "coach"]);
    let resp = service::update_schedules(&db, &auth, coach_id, &entries)
        .await
        .expect("owner may replace their schedule");

    assert_eq!(resp.len(), 2);
    // Repository should return rows in deterministic order by day_of_week.
    assert_eq!(resp[0].day_of_week, 1);
    assert_eq!(resp[1].day_of_week, 3);
}

#[sqlx::test]
async fn update_schedules_by_stranger_returns_forbidden(db: PgPool) {
    let owner_id = common::seed_member(&db, "owner@example.com", "hunter22-secret").await;
    let stranger_id = common::seed_member(&db, "stranger@example.com", "hunter22-secret").await;
    let coach_id = common::fixtures::seed_coach(&db, owner_id, "Owner").await;

    let err = service::update_schedules(
        &db,
        &common::member_auth(stranger_id),
        coach_id,
        &[],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)));
}

#[sqlx::test]
async fn update_schedules_by_admin_on_other_coach_succeeds(db: PgPool) {
    let owner_id = common::seed_member(&db, "owner@example.com", "hunter22-secret").await;
    let admin_id = common::seed_member(&db, "admin@example.com", "hunter22-secret").await;
    let coach_id = common::fixtures::seed_coach(&db, owner_id, "Owner").await;

    service::update_schedules(
        &db,
        &common::auth_with_roles(admin_id, &["member", "admin"]),
        coach_id,
        &[ScheduleEntry {
            day_of_week: 2,
            start_time: "10:00:00".into(),
            end_time: "11:00:00".into(),
            is_available: true,
        }],
    )
    .await
    .expect("admin may edit any coach's schedule");
}

#[sqlx::test]
async fn update_schedules_invalid_time_returns_422(db: PgPool) {
    // "99:99" / "aa:bb" are legal length (5 chars, passes ScheduleEntry's
    // `#[validate(length(min = 5, max = 8))]`) but an illegal time format —
    // before the service-layer parse, these reached the DB unparsed and
    // surfaced as a bare 500 (sqlx::Error::Protocol wrapped as
    // AppError::Database, not Validation).
    let user_id = common::seed_member(&db, "badtime@example.com", "hunter22-secret").await;
    let coach_id = common::fixtures::seed_coach(&db, user_id, "Owner").await;
    let auth = common::auth_with_roles(user_id, &["member", "coach"]);

    let err = service::update_schedules(
        &db,
        &auth,
        coach_id,
        &[ScheduleEntry {
            day_of_week: 1,
            start_time: "99:99".into(),
            end_time: "10:00".into(),
            is_available: true,
        }],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Validation(_)),
        "expected Validation for invalid start_time, got {err:?}"
    );

    let err = service::update_schedules(
        &db,
        &auth,
        coach_id,
        &[ScheduleEntry {
            day_of_week: 1,
            start_time: "09:00".into(),
            end_time: "aa:bb".into(),
            is_available: true,
        }],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Validation(_)),
        "expected Validation for invalid end_time, got {err:?}"
    );
}

#[sqlx::test]
async fn update_schedules_end_not_after_start_returns_422(db: PgPool) {
    // Both entries parse fine individually, so before the service-layer
    // `end <= start` check they reached the DB and tripped the
    // `coach_schedules_time_order CHECK (end_time > start_time)` constraint
    // — surfacing as a bare 500 (AppError::Database) instead of Validation.
    let user_id = common::seed_member(&db, "badrange@example.com", "hunter22-secret").await;
    let coach_id = common::fixtures::seed_coach(&db, user_id, "Owner").await;
    let auth = common::auth_with_roles(user_id, &["member", "coach"]);

    // end_time == start_time
    let err = service::update_schedules(
        &db,
        &auth,
        coach_id,
        &[ScheduleEntry {
            day_of_week: 1,
            start_time: "10:00".into(),
            end_time: "10:00".into(),
            is_available: true,
        }],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Validation(_)),
        "expected Validation for end_time == start_time, got {err:?}"
    );

    // end_time before start_time
    let err = service::update_schedules(
        &db,
        &auth,
        coach_id,
        &[ScheduleEntry {
            day_of_week: 1,
            start_time: "11:00".into(),
            end_time: "10:00".into(),
            is_available: true,
        }],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Validation(_)),
        "expected Validation for end_time < start_time, got {err:?}"
    );
}

#[sqlx::test]
async fn clock_in_twice_without_clock_out_returns_conflict(db: PgPool) {
    // The migration's uq_clock_records_open partial unique index means a
    // second open clock-in for the same coach hits a unique violation. The
    // service layer must translate that to Conflict — otherwise the raw
    // DB error would surface as a 500 to the client.
    let user_id = common::seed_member(&db, "coach@example.com", "hunter22-secret").await;
    let coach_id = common::fixtures::seed_coach(&db, user_id, "Coach").await;
    let auth = common::coach_auth(user_id);

    service::clock_in(&db, &auth, coach_id, Some("starting shift"))
        .await
        .expect("first clock-in ok");

    let err = service::clock_in(&db, &auth, coach_id, None)
        .await
        .unwrap_err();
    match err {
        AppError::Conflict(msg) => assert!(msg.contains("already clocked in"), "msg: {msg}"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn clock_out_with_no_open_record_returns_not_found(db: PgPool) {
    let user_id = common::seed_member(&db, "coach@example.com", "hunter22-secret").await;
    let coach_id = common::fixtures::seed_coach(&db, user_id, "Coach").await;

    let err = service::clock_out(&db, &common::coach_auth(user_id), coach_id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}
