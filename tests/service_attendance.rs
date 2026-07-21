//! Repository-level tests for `attendance::repository::upsert_attendance_tx`'s
//! self-defending 核准恆勝 guard (ADR-0008). These call the repository directly
//! (no HTTP, no `marking::plan` pre-check) so they exercise the `ON CONFLICT
//! ... WHERE` clause in isolation — the deterministic, single-connection proxy
//! for the TOCTOU race the guard closes. They assert the *row state* after the
//! upsert, never `rows_affected` (the blocked case affects zero rows without
//! erroring, and the caller discards the result).

mod common;

use chrono::{Duration, NaiveTime, Utc};
use sqlx::PgPool;

use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::attendance::repository;

use common::fixtures::{seed_attendance, seed_course, seed_course_session, seed_enrolment, seed_leave_request};
use common::seed_member;

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

fn yesterday() -> chrono::NaiveDate {
    (Utc::now() - Duration::days(1)).date_naive()
}

/// Guard blocks the write: an `approved` leave, already projected to an
/// attendance `leave` row, cannot be overwritten present by a direct upsert.
/// All three OR branches of the guard are false — `EXCLUDED.status = 'present'`
/// (not leave), the existing row *is* `leave`, and an `approved` leave request
/// EXISTS — so zero rows change and the row stays `leave`. This is the
/// deterministic proxy for the concurrency window `marking::plan`'s pre-check
/// alone can't close.
#[sqlx::test]
async fn upsert_guard_blocks_present_over_approved_leave(db: PgPool) {
    let course_id = seed_course(&db, "Guard Blocked Course", None).await;
    let session_id = seed_course_session(&db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let member = seed_member(&db, "att-guard-blocked@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, member, course_id, "active", Utc::now()).await;
    seed_leave_request(&db, enrolment_id, session_id, "approved").await;
    seed_attendance(&db, session_id, enrolment_id, "leave", member).await;

    let mut tx = db.begin().await.expect("begin");
    repository::upsert_attendance_tx(
        &mut tx,
        session_id,
        enrolment_id,
        AttendanceStatus::Present,
        member,
    )
    .await
    .expect("upsert must not error even when the guard blocks the update");
    tx.commit().await.expect("commit");

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_one(&db)
    .await
    .expect("fetch status");
    assert_eq!(status, "leave", "the guard must keep the approved-leave row as leave");
}

/// Guard allows the write: a *verbal* leave (a `leave` attendance row with no
/// approved leave request behind it) stays freely overwritable. The guard's
/// third branch — `NOT EXISTS (approved leave)` — is true, so a present upsert
/// lands. This preserves the pre-existing correction path for verbal leave.
#[sqlx::test]
async fn upsert_guard_allows_present_over_verbal_leave(db: PgPool) {
    let course_id = seed_course(&db, "Verbal Leave Course", None).await;
    let session_id = seed_course_session(&db, course_id, yesterday(), t(9, 0), t(10, 0)).await;
    let member = seed_member(&db, "att-guard-verbal@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, member, course_id, "active", Utc::now()).await;
    // Verbal leave: an attendance `leave` row, but no approved leave_request.
    seed_attendance(&db, session_id, enrolment_id, "leave", member).await;

    let mut tx = db.begin().await.expect("begin");
    repository::upsert_attendance_tx(
        &mut tx,
        session_id,
        enrolment_id,
        AttendanceStatus::Present,
        member,
    )
    .await
    .expect("upsert");
    tx.commit().await.expect("commit");

    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM attendance_records WHERE session_id = $1 AND enrolment_id = $2",
    )
    .bind(session_id)
    .bind(enrolment_id)
    .fetch_one(&db)
    .await
    .expect("fetch status");
    assert_eq!(status, "present", "verbal leave (no approved request) must stay overwritable");
}
