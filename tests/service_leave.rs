//! Integration tests for `leave::service` — specifically the concurrent
//! makeup races that need direct `service::` access with `tokio::spawn`,
//! mirroring `service_enrolments.rs`'s
//! `concurrent_enrol_same_user_course_only_one_succeeds` /
//! `service_bookings.rs`'s `concurrent_book_last_slot_only_one_wins`:
//! - same leave request booked twice (leave-request row lock), and
//! - two different leave requests racing for a target session's last free
//!   seat (target-session row lock, controller ruling 2026-07-06).
//!
//! Plus `book_makeup`'s error-precedence pins (404/409/422 ordering).

mod common;

use chrono::{Duration, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::leave::dto::MakeupRequest;
use dream_fly_backend::modules::leave::model::LeaveStatus;
use dream_fly_backend::modules::leave::service;

use common::fixtures::{CourseSeed, seed_course_session, seed_enrolment, seed_leave_request};

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

async fn attempt_makeup(
    db: PgPool,
    user_id: Uuid,
    leave_id: Uuid,
    target_session_id: Uuid,
) -> bool {
    let auth = common::auth_for(&db, user_id).await;
    service::book_makeup(
        &db,
        common::studio_now_utc(Utc::now()),
        &auth,
        leave_id,
        MakeupRequest { session_id: target_session_id },
    )
    .await
    .is_ok()
}

#[sqlx::test]
async fn concurrent_makeup_same_leave_request_only_one_succeeds(db: PgPool) {
    // Two concurrent `book_makeup` calls for the *same* approved leave
    // request, both targeting the same (roomy-capacity) session. The
    // `FOR UPDATE OF lr` lock in `find_for_makeup_tx` must serialize them so
    // only the first can observe `makeup_session_id IS NULL` and win.
    let course_id = CourseSeed::new("Makeup Race Course").max_students(10).insert(&db).await;
    let user_id = common::seed_member(&db, "makeup-race@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&db, user_id, course_id, EnrolmentStatus::Active, Utc::now()).await;

    let original = (Utc::now() - Duration::days(1)).date_naive();
    let session_id = seed_course_session(&db, course_id, original, t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id = seed_course_session(&db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let leave_id = seed_leave_request(&db, enrolment_id, session_id, LeaveStatus::Approved).await;

    let (res_a, res_b) = tokio::join!(
        tokio::spawn(attempt_makeup(
            db.clone(),
            user_id,
            leave_id,
            target_session_id
        )),
        tokio::spawn(attempt_makeup(
            db.clone(),
            user_id,
            leave_id,
            target_session_id
        )),
    );
    let ok_count = [res_a.expect("task a panicked"), res_b.expect("task b panicked")]
        .iter()
        .filter(|ok| **ok)
        .count();
    assert_eq!(ok_count, 1, "exactly one concurrent makeup booking should succeed");

    let makeup: Option<Uuid> =
        sqlx::query_scalar("SELECT makeup_session_id FROM leave_requests WHERE id = $1")
            .bind(leave_id)
            .fetch_one(&db)
            .await
            .expect("fetch leave request");
    assert_eq!(makeup, Some(target_session_id));
}

#[sqlx::test]
async fn concurrent_makeup_different_requests_last_seat_only_one_wins(db: PgPool) {
    // Two DIFFERENT members' approved leave requests race to book a makeup
    // into a target session with exactly one free seat (max=3, 2 active
    // enrolments → remaining = 3 - 2 + 0 - 0 = 1). The target-session row
    // lock (`lock_session_tx`, controller ruling 2026-07-06) must serialize
    // the two capacity checks: the loser recounts after the winner's commit,
    // sees remaining = 3 - 2 + 0 - 1 = 0, and gets the capacity 409.
    let course_id = CourseSeed::new("Makeup Last Seat Course").max_students(3).insert(&db).await;
    let user_a = common::seed_member(&db, "makeup-last-seat-a@example.com", "Password!234").await;
    let user_b = common::seed_member(&db, "makeup-last-seat-b@example.com", "Password!234").await;
    let enrolment_a =
        seed_enrolment(&db, user_a, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let enrolment_b =
        seed_enrolment(&db, user_b, course_id, EnrolmentStatus::Active, Utc::now()).await;

    let original = (Utc::now() - Duration::days(1)).date_naive();
    let session_id = seed_course_session(&db, course_id, original, t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let leave_a = seed_leave_request(&db, enrolment_a, session_id, LeaveStatus::Approved).await;
    let leave_b = seed_leave_request(&db, enrolment_b, session_id, LeaveStatus::Approved).await;

    let (res_a, res_b) = tokio::join!(
        tokio::spawn(attempt_makeup(
            db.clone(),
            user_a,
            leave_a,
            target_session_id
        )),
        tokio::spawn(attempt_makeup(
            db.clone(),
            user_b,
            leave_b,
            target_session_id
        )),
    );
    let ok_count = [res_a.expect("task a panicked"), res_b.expect("task b panicked")]
        .iter()
        .filter(|ok| **ok)
        .count();
    assert_eq!(
        ok_count, 1,
        "exactly one of two racing makeup bookings should win the last seat"
    );

    let booked_into_target: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM leave_requests WHERE makeup_session_id = $1")
            .bind(target_session_id)
            .fetch_one(&db)
            .await
            .expect("count makeups into target");
    assert_eq!(booked_into_target, 1, "the target session must not be overbooked");
}

// ---------------------------------------------------------------------------
// 補課錯誤優先序釘住(`book_makeup`):404 → 403 → source 409 → 場次 404 →
// target 422/400 → 座位 409。重排 `book_makeup` 的讀取順序時,這些測試守住
// 對外可見的錯誤優先序不變。
// ---------------------------------------------------------------------------

async fn makeup(
    db: &PgPool,
    user_id: Uuid,
    leave_id: Uuid,
    target_session_id: Uuid,
) -> Result<(), AppError> {
    service::book_makeup(
        db,
        common::studio_now_utc(Utc::now()),
        &common::auth_for(&db, user_id).await,
        leave_id,
        MakeupRequest { session_id: target_session_id },
    )
    .await
    .map(|_| ())
}

/// 已核准、可補課的假單(課程 `max_students`,申請人是唯一 active 報名)。
/// 回傳 (course_id, user_id, leave_id)。
async fn approved_leave(db: &PgPool, name: &str, max_students: i32) -> (Uuid, Uuid, Uuid) {
    let course_id = CourseSeed::new(name).max_students(max_students).insert(db).await;
    let email = format!("makeup-pin-{}@example.com", Uuid::now_v7());
    let user_id = common::seed_member(db, &email, "Password!234").await;
    let enrolment_id =
        seed_enrolment(db, user_id, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let original = (Utc::now() - Duration::days(1)).date_naive();
    let session_id = seed_course_session(db, course_id, original, t(9, 0), t(10, 0)).await;
    let leave_id = seed_leave_request(db, enrolment_id, session_id, LeaveStatus::Approved).await;
    (course_id, user_id, leave_id)
}

#[sqlx::test]
async fn makeup_unknown_target_session_returns_404(db: PgPool) {
    let (_course_id, user_id, leave_id) = approved_leave(&db, "Makeup Pin 404", 10).await;

    let err = makeup(&db, user_id, leave_id, Uuid::now_v7())
        .await
        .expect_err("unknown target session must be rejected");
    assert!(
        matches!(err, AppError::NotFound(ref m) if m == "場次不存在"),
        "got: {err:?}"
    );
}

#[sqlx::test]
async fn makeup_source_conflict_precedes_unknown_target_404(db: PgPool) {
    // 假單還是 pending(source 409),目標場次又不存在(404)——source 先。
    let course_id = CourseSeed::new("Makeup Pin Source").max_students(10).insert(&db).await;
    let user_id = common::seed_member(&db, "makeup-pin-source@example.com", "Password!234").await;
    let enrolment_id =
        seed_enrolment(&db, user_id, course_id, EnrolmentStatus::Active, Utc::now()).await;
    let original = (Utc::now() - Duration::days(1)).date_naive();
    let session_id = seed_course_session(&db, course_id, original, t(9, 0), t(10, 0)).await;
    let leave_id = seed_leave_request(&db, enrolment_id, session_id, LeaveStatus::Pending).await;

    let err = makeup(&db, user_id, leave_id, Uuid::now_v7())
        .await
        .expect_err("pending leave must be rejected");
    assert!(
        matches!(err, AppError::Conflict(ref m) if m == "僅已核准的假單可預約補課"),
        "got: {err:?}"
    );
}

#[sqlx::test]
async fn makeup_target_rule_precedes_seat_count(db: PgPool) {
    // max=1、申請人是唯一 active 報名 → 座位 1 - 1 + 0 - 0 = 0(會 409);
    // 目標場次又已開始(422)——target 規則先於座位。
    let (course_id, user_id, leave_id) = approved_leave(&db, "Makeup Pin Target", 1).await;
    let started = (Utc::now() - Duration::days(1)).date_naive();
    let target = seed_course_session(&db, course_id, started, t(14, 0), t(15, 0)).await;

    let err = makeup(&db, user_id, leave_id, target)
        .await
        .expect_err("already-started target must be rejected");
    assert!(
        matches!(err, AppError::Validation(ref m) if m == "補課場次已開始"),
        "got: {err:?}"
    );
}

#[sqlx::test]
async fn makeup_into_full_session_returns_409(db: PgPool) {
    // max=1、申請人是唯一 active 報名 → 1 - 1 + 0 - 0 = 0 → 409。
    let (course_id, user_id, leave_id) = approved_leave(&db, "Makeup Pin Full", 1).await;
    let future = (Utc::now() + Duration::days(3)).date_naive();
    let target = seed_course_session(&db, course_id, future, t(14, 0), t(15, 0)).await;

    let err = makeup(&db, user_id, leave_id, target)
        .await
        .expect_err("full target session must be rejected");
    assert!(
        matches!(err, AppError::Conflict(ref m) if m == "該場次名額已滿"),
        "got: {err:?}"
    );
}
