//! Integration tests for `leave::service` — specifically the concurrent
//! makeup races that need direct `service::` access with `tokio::spawn`,
//! mirroring `service_enrolments.rs`'s
//! `concurrent_enrol_same_user_course_only_one_succeeds` /
//! `service_bookings.rs`'s `concurrent_book_last_slot_only_one_wins`:
//! - same leave request booked twice (leave-request row lock), and
//! - two different leave requests racing for a target session's last free
//!   seat (target-session row lock, controller ruling 2026-07-06).

mod common;

use chrono::{Duration, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::config::ServerConfig;
use dream_fly_backend::modules::leave::dto::MakeupRequest;
use dream_fly_backend::modules::leave::service;

use common::fixtures::{seed_course_session, seed_course_with_capacity, seed_enrolment, seed_leave_request};

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

async fn attempt_makeup(
    db: PgPool,
    server: ServerConfig,
    user_id: Uuid,
    leave_id: Uuid,
    target_session_id: Uuid,
) -> bool {
    let auth = common::member_auth(user_id);
    service::book_makeup(
        &db,
        &server,
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
    let server = common::test_server_config();
    let course_id = seed_course_with_capacity(&db, "Makeup Race Course", None, 10).await;
    let user_id = common::seed_member(&db, "makeup-race@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, user_id, course_id, "active", Utc::now()).await;

    let original = (Utc::now() - Duration::days(1)).date_naive();
    let session_id = seed_course_session(&db, course_id, original, t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id = seed_course_session(&db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let leave_id = seed_leave_request(&db, enrolment_id, session_id, "approved").await;

    let (res_a, res_b) = tokio::join!(
        tokio::spawn(attempt_makeup(
            db.clone(),
            server.clone(),
            user_id,
            leave_id,
            target_session_id
        )),
        tokio::spawn(attempt_makeup(
            db.clone(),
            server.clone(),
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
    let server = common::test_server_config();
    let course_id = seed_course_with_capacity(&db, "Makeup Last Seat Course", None, 3).await;
    let user_a = common::seed_member(&db, "makeup-last-seat-a@example.com", "Password!234").await;
    let user_b = common::seed_member(&db, "makeup-last-seat-b@example.com", "Password!234").await;
    let enrolment_a = seed_enrolment(&db, user_a, course_id, "active", Utc::now()).await;
    let enrolment_b = seed_enrolment(&db, user_b, course_id, "active", Utc::now()).await;

    let original = (Utc::now() - Duration::days(1)).date_naive();
    let session_id = seed_course_session(&db, course_id, original, t(9, 0), t(10, 0)).await;
    let target_date = (Utc::now() + Duration::days(3)).date_naive();
    let target_session_id =
        seed_course_session(&db, course_id, target_date, t(14, 0), t(15, 0)).await;

    let leave_a = seed_leave_request(&db, enrolment_a, session_id, "approved").await;
    let leave_b = seed_leave_request(&db, enrolment_b, session_id, "approved").await;

    let (res_a, res_b) = tokio::join!(
        tokio::spawn(attempt_makeup(
            db.clone(),
            server.clone(),
            user_a,
            leave_a,
            target_session_id
        )),
        tokio::spawn(attempt_makeup(
            db.clone(),
            server.clone(),
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
