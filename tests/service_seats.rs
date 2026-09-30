//! Service-level tests for `courses::seats`' session seat model (實體座位
//! 模型, contract §3.20, controller ruling 2026-07-06), driven straight
//! through `lock_session_tx` → `require_room_tx`:
//! remaining = max_students - active + approved_leave_for_target
//!           - makeups_into_target, counting only still-active enrolments;
//! room iff remaining > 0, otherwise 409 `該場次名額已滿`.
//!
//! Moved down from `tests/http_leave.rs` — the formula is owned by
//! `courses::seats`, so its scenarios are tested at that seam rather than
//! through `POST /leave-requests/{id}/makeup`. The makeup endpoint's own
//! error precedence (seats 409 last) is pinned in `tests/service_leave.rs`.

mod common;

use chrono::{Duration, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::courses::seats;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::leave::model::LeaveStatus;

use common::fixtures::{
    CourseSeed, seed_course_session, seed_enrolment, seed_leave_request, set_makeup_session,
};

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

/// A course of capacity `max_students` with a past session (for leaves
/// that get made up elsewhere) and a future target session. Returns
/// (course_id, original_session_id, target_session_id).
async fn course_with_sessions(db: &PgPool, name: &str, max_students: i32) -> (Uuid, Uuid, Uuid) {
    let course_id = CourseSeed::new(name).max_students(max_students).insert(db).await;
    let original = (Utc::now() - Duration::days(1)).date_naive();
    let original_session = seed_course_session(db, course_id, original, t(9, 0), t(10, 0)).await;
    let target = (Utc::now() + Duration::days(3)).date_naive();
    let target_session = seed_course_session(db, course_id, target, t(14, 0), t(15, 0)).await;
    (course_id, original_session, target_session)
}

/// `n` fresh members enrolled in `course_id` with `status`; returns their
/// enrolment ids.
async fn enrolments(db: &PgPool, course_id: Uuid, status: EnrolmentStatus, n: usize) -> Vec<Uuid> {
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        let email = format!("seat-{}@example.com", Uuid::now_v7());
        let member = common::seed_member(db, &email, "Password!234").await;
        ids.push(seed_enrolment(db, member, course_id, status, Utc::now()).await);
    }
    ids
}

/// Does `session_id` have room? Locks the session row in a fresh
/// transaction, asks `require_room_tx`, then rolls back.
async fn room_at(db: &PgPool, session_id: Uuid) -> Result<(), AppError> {
    let mut tx = db.begin().await.expect("begin");
    let lock = seats::lock_session_tx(&mut tx, session_id)
        .await
        .expect("lock session")
        .expect("session exists");
    seats::require_room_tx(&mut tx, &lock).await
}

fn assert_full(result: Result<(), AppError>) {
    assert!(
        matches!(result, Err(AppError::Conflict(ref m)) if m == "該場次名額已滿"),
        "got: {result:?}"
    );
}

#[sqlx::test]
async fn full_session_has_no_room(db: PgPool) {
    // max=1, 1 active enrolment → 1 - 1 + 0 - 0 = 0 → 409 (strict `> 0`).
    let (course_id, _original, target) = course_with_sessions(&db, "Seat Full", 1).await;
    enrolments(&db, course_id, EnrolmentStatus::Active, 1).await;

    assert_full(room_at(&db, target).await);
}

#[sqlx::test]
async fn approved_leave_frees_seats_in_full_class(db: PgPool) {
    // Controller regression (a): max=10, 10 active (full class), 3 of them
    // have APPROVED LEAVE for the target, 0 makeups → 10 - 10 + 3 - 0 = 3
    // → room. (The pre-ruling formula computed 10 - 10 - 3 + 0 = -3.)
    let (course_id, _original, target) = course_with_sessions(&db, "Seat Leave Frees", 10).await;
    let active = enrolments(&db, course_id, EnrolmentStatus::Active, 10).await;
    for enrolment in &active[..3] {
        seed_leave_request(&db, *enrolment, target, LeaveStatus::Approved).await;
    }

    room_at(&db, target)
        .await
        .expect("approved leaves free seats");
}

#[sqlx::test]
async fn prior_makeups_fill_remaining_seats(db: PgPool) {
    // Controller regression (b): max=10, 8 active, 0 leave for the target,
    // 2 makeups already booked into it → 10 - 8 + 0 - 2 = 0 → 409. (The
    // pre-ruling formula computed 10 - 8 - 0 + 2 = 4 and would overbook.)
    let (course_id, original, target) = course_with_sessions(&db, "Seat Makeups Fill", 10).await;
    let active = enrolments(&db, course_id, EnrolmentStatus::Active, 8).await;
    for enrolment in &active[..2] {
        let leave = seed_leave_request(&db, *enrolment, original, LeaveStatus::Approved).await;
        set_makeup_session(&db, leave, target).await;
    }

    assert_full(room_at(&db, target).await);
}

#[sqlx::test]
async fn leave_by_cancelled_enrolment_frees_no_ghost_seat(db: PgPool) {
    // Active-only counts: max=1, 1 active; a CANCELLED enrolment holds an
    // approved leave for the target → 1 - 1 + 0 - 0 = 0 → 409 (counting the
    // cancelled enrolment's leave would wrongly yield 1).
    let (course_id, _original, target) = course_with_sessions(&db, "Seat Ghost", 1).await;
    enrolments(&db, course_id, EnrolmentStatus::Active, 1).await;
    let quitter = enrolments(&db, course_id, EnrolmentStatus::Cancelled, 1).await;
    seed_leave_request(&db, quitter[0], target, LeaveStatus::Approved).await;

    assert_full(room_at(&db, target).await);
}

#[sqlx::test]
async fn makeup_by_cancelled_enrolment_occupies_no_seat(db: PgPool) {
    // Symmetric: max=2, 1 active; a CANCELLED enrolment has a makeup booked
    // into the target → 2 - 1 + 0 - 0 = 1 → room (counting the cancelled
    // enrolment's makeup would wrongly yield 0).
    let (course_id, original, target) = course_with_sessions(&db, "Seat Freed", 2).await;
    enrolments(&db, course_id, EnrolmentStatus::Active, 1).await;
    let quitter = enrolments(&db, course_id, EnrolmentStatus::Cancelled, 1).await;
    let leave = seed_leave_request(&db, quitter[0], original, LeaveStatus::Approved).await;
    set_makeup_session(&db, leave, target).await;

    room_at(&db, target)
        .await
        .expect("a cancelled enrolment's makeup frees its seat");
}
