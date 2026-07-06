//! Integration tests for `leave::service` — specifically the concurrent
//! double-makeup race that needs direct `service::` access with
//! `tokio::spawn`, mirroring `service_enrolments.rs`'s
//! `concurrent_enrol_same_user_course_only_one_succeeds` /
//! `service_bookings.rs`'s `concurrent_book_last_slot_only_one_wins`.

mod common;

use chrono::{Duration, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::config::ServerConfig;
use dream_fly_backend::extractors::auth::AuthUser;
use dream_fly_backend::modules::leave::dto::MakeupRequest;
use dream_fly_backend::modules::leave::service;

use common::fixtures::{seed_course_session, seed_course_with_capacity, seed_enrolment, seed_leave_request};

fn member_auth(user_id: Uuid) -> AuthUser {
    AuthUser {
        user_id,
        email: "member@test".into(),
        roles: vec!["member".into()],
    }
}

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
    let auth = member_auth(user_id);
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
