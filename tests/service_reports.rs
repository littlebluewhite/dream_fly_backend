//! Integration tests for `reports::service` — the `/reports/admin`,
//! `/reports/coach`, `/reports/me` aggregation endpoints.
//!
//! Covered paths (task brief's Tests section):
//! - every endpoint on an empty DB returns all-zero/all-null, not a 500
//! - admin revenue counts only the "paid family" (paid/processing/
//!   completed) and never a refunded order, even one with a real `paid_at`
//! - fill_rate's divide-by-zero guard (pure unit test — see
//!   `reports::service`'s own `#[cfg(test)]` module; `courses_max_students_
//!   pos CHECK (max_students > 0)` makes this unreachable via real data, so
//!   it cannot be reproduced here as a DB-backed test)
//! - attendance_rate excludes `leave` from both numerator and denominator,
//!   for both the coach and member endpoints
//! - a coach's report only reflects their own domain (courses/students/
//!   sessions), never another coach's

mod common;

use chrono::{DateTime, Datelike, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::auth::AuthUser;
use dream_fly_backend::modules::reports::service;

use common::fixtures::{
    seed_coach, seed_course, seed_course_schedule_slot, seed_course_session,
    seed_course_with_capacity, seed_enrolment, seed_message, seed_waitlist_entry,
    set_points_balance,
};
use common::{seed_member, test_server_config};

fn auth_for(user_id: Uuid, roles: &[&str]) -> AuthUser {
    AuthUser {
        user_id,
        email: format!("{user_id}@example.com"),
        roles: roles.iter().map(|r| (*r).to_string()).collect(),
    }
}

fn t(h: u32, m: u32) -> chrono::NaiveTime {
    chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

/// `now` shifted back `n` calendar months, pinned to the 1st of that month
/// (sidesteps "day doesn't exist in target month" — e.g. Jan 31 minus one
/// month has no Feb 31). Used to seed orders/dates unambiguously inside a
/// specific month bucket, since "n * 30 days ago" can drift into the wrong
/// calendar month depending on which months it crosses.
fn months_ago(now: DateTime<Utc>, n: i32) -> DateTime<Utc> {
    let day1 = now.with_day(1).unwrap();
    let mut year = day1.year();
    let mut month = day1.month() as i32 - n;
    while month <= 0 {
        month += 12;
        year -= 1;
    }
    day1.with_year(year).unwrap().with_month(month as u32).unwrap()
}

/// Insert an order directly with an explicit `status` and `paid_at`
/// (bypassing `orders::service::checkout`, and leaner than the shared
/// `seed_order_with_item` fixture — these tests only ever read `orders.
/// total_cents`/`status`/`paid_at`, never `order_items`). Mirrors
/// `seed_order_with_item`'s UUID-based `order_number` (avoids a
/// same-millisecond UUIDv7-prefix collision across repeated calls in one
/// test).
async fn seed_order(
    db: &PgPool,
    user_id: Uuid,
    status: &str,
    total_cents: i64,
    paid_at: Option<DateTime<Utc>>,
) -> Uuid {
    let id = Uuid::now_v7();
    let order_number = format!("RPT-{id}");
    sqlx::query(
        "INSERT INTO orders (id, user_id, order_number, status, total_cents, discount_cents, paid_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4::order_status, $5, 0, $6, NOW(), NOW())",
    )
    .bind(id)
    .bind(user_id)
    .bind(&order_number)
    .bind(status)
    .bind(total_cents)
    .bind(paid_at)
    .execute(db)
    .await
    .expect("insert order");
    id
}

/// Insert an `attendance_records` row directly (bypassing `PUT
/// /sessions/{id}/attendance`), so tests can arrange present/absent/leave
/// combinations without a real coach HTTP round trip.
async fn seed_attendance(
    db: &PgPool,
    session_id: Uuid,
    enrolment_id: Uuid,
    status: &str,
    marked_by: Uuid,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO attendance_records (id, session_id, enrolment_id, status, marked_by, marked_at, created_at) \
         VALUES ($1, $2, $3, $4::attendance_status, $5, NOW(), NOW())",
    )
    .bind(id)
    .bind(session_id)
    .bind(enrolment_id)
    .bind(status)
    .bind(marked_by)
    .execute(db)
    .await
    .expect("insert attendance_record");
    id
}

// ---------------------------------------------------------------------------
// GET /reports/admin
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn admin_report_empty_db_is_all_zero(db: PgPool) {
    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    assert_eq!(report.revenue.this_month_cents, 0);
    assert_eq!(report.revenue.last_month_cents, 0);
    assert_eq!(report.revenue.trend.len(), 12);
    assert!(report.revenue.trend.iter().all(|p| p.revenue_cents == 0));
    assert_eq!(report.members.total, 0);
    assert_eq!(report.members.new_this_month, 0);
    assert_eq!(report.members.active, 0);
    assert!(report.courses.is_empty());
    assert!(report.coaches.is_empty());
}

#[sqlx::test]
async fn admin_report_revenue_counts_only_paid_family(db: PgPool) {
    let user_id = seed_member(&db, "revenue-buyer@example.com", "Password!234").await;
    let now = Utc::now();

    seed_order(&db, user_id, "paid", 10_000, Some(now)).await;
    seed_order(&db, user_id, "processing", 20_000, Some(now)).await;
    seed_order(&db, user_id, "completed", 30_000, Some(now)).await;
    // None of these should count, even though each has a real `paid_at` in
    // the current month — a refunded order keeps its original `paid_at`
    // (see `orders::repository::update_status_and_paid_at_tx`), so the
    // filter must be on `status`, not `paid_at IS NOT NULL`.
    seed_order(&db, user_id, "refunded", 999_999, Some(now)).await;
    seed_order(&db, user_id, "cancelled", 999_999, Some(now)).await;
    seed_order(&db, user_id, "pending", 999_999, None).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    assert_eq!(report.revenue.this_month_cents, 60_000);
    assert_eq!(report.revenue.trend.last().unwrap().revenue_cents, 60_000);
}

#[sqlx::test]
async fn admin_report_revenue_trend_buckets_by_month(db: PgPool) {
    let user_id = seed_member(&db, "revenue-trend@example.com", "Password!234").await;
    let now = Utc::now();
    let last_month = months_ago(now, 1);
    let oldest_month = months_ago(now, 11);

    seed_order(&db, user_id, "paid", 1_000, Some(now)).await;
    seed_order(&db, user_id, "paid", 2_000, Some(last_month)).await;
    seed_order(&db, user_id, "paid", 3_000, Some(oldest_month)).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let trend = &report.revenue.trend;
    assert_eq!(trend.len(), 12);
    assert_eq!(trend.last().unwrap().revenue_cents, 1_000, "this month bucket");
    assert_eq!(report.revenue.this_month_cents, 1_000);
    assert_eq!(report.revenue.last_month_cents, 2_000);
    assert_eq!(trend.first().unwrap().revenue_cents, 3_000, "oldest (11-months-ago) bucket");

    let nonzero_count = trend.iter().filter(|p| p.revenue_cents != 0).count();
    assert_eq!(nonzero_count, 3, "only 3 buckets should be nonzero, got {trend:?}");
}

#[sqlx::test]
async fn admin_report_members_total_new_and_active(db: PgPool) {
    let old_user = seed_member(&db, "old-member@example.com", "Password!234").await;
    let new_active_user = seed_member(&db, "new-active-member@example.com", "Password!234").await;
    let _new_plain_user = seed_member(&db, "new-plain-member@example.com", "Password!234").await;

    sqlx::query("UPDATE users SET created_at = NOW() - interval '3 months' WHERE id = $1")
        .bind(old_user)
        .execute(&db)
        .await
        .unwrap();

    let course_id = seed_course(&db, "Members Stats Course", None).await;
    seed_enrolment(&db, old_user, course_id, "active", Utc::now()).await;
    seed_enrolment(&db, new_active_user, course_id, "active", Utc::now()).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    assert_eq!(report.members.total, 3);
    assert_eq!(
        report.members.new_this_month, 2,
        "new_active_user + new_plain_user were both created just now"
    );
    assert_eq!(
        report.members.active, 2,
        "old_user and new_active_user both hold an active enrolment"
    );
}

#[sqlx::test]
async fn admin_report_course_fill_rate_and_waitlist(db: PgPool) {
    let course_id = seed_course_with_capacity(&db, "Fill Rate Course", None, 4).await;
    let u1 = seed_member(&db, "fill-1@example.com", "Password!234").await;
    let u2 = seed_member(&db, "fill-2@example.com", "Password!234").await;
    let u3 = seed_member(&db, "fill-3@example.com", "Password!234").await;
    let w1 = seed_member(&db, "fill-wait-1@example.com", "Password!234").await;

    seed_enrolment(&db, u1, course_id, "active", Utc::now()).await;
    seed_enrolment(&db, u2, course_id, "active", Utc::now()).await;
    seed_enrolment(&db, u3, course_id, "cancelled", Utc::now()).await; // must not count
    seed_waitlist_entry(&db, w1, course_id, "waiting", Utc::now()).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let row = report
        .courses
        .iter()
        .find(|c| c.course_id == course_id)
        .expect("course row present");
    assert_eq!(row.enrolled, 2);
    assert_eq!(row.max_students, 4);
    assert_eq!(row.fill_rate, Some(0.5));
    assert_eq!(row.waitlist_count, 1);
}

#[sqlx::test]
async fn admin_report_coach_course_and_student_count_scoped_per_coach(db: PgPool) {
    let coach_a_user = seed_member(&db, "admin-coach-a@example.com", "Password!234").await;
    let coach_b_user = seed_member(&db, "admin-coach-b@example.com", "Password!234").await;
    let coach_a = seed_coach(&db, coach_a_user, "Coach A").await;
    let coach_b = seed_coach(&db, coach_b_user, "Coach B").await;

    let course_a1 = seed_course(&db, "Coach A Course 1", Some(coach_a)).await;
    let course_a2 = seed_course(&db, "Coach A Course 2", Some(coach_a)).await;
    let course_b1 = seed_course(&db, "Coach B Course 1", Some(coach_b)).await;

    let student_1 = seed_member(&db, "admin-student-1@example.com", "Password!234").await;
    let student_2 = seed_member(&db, "admin-student-2@example.com", "Password!234").await;
    seed_enrolment(&db, student_1, course_a1, "active", Utc::now()).await;
    // Same student in 2 of coach A's courses -> distinct student_count is
    // still 1, not 2.
    seed_enrolment(&db, student_1, course_a2, "active", Utc::now()).await;
    seed_enrolment(&db, student_2, course_b1, "active", Utc::now()).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let row_a = report.coaches.iter().find(|c| c.coach_id == coach_a).unwrap();
    assert_eq!(row_a.course_count, 2);
    assert_eq!(row_a.student_count, 1);

    let row_b = report.coaches.iter().find(|c| c.coach_id == coach_b).unwrap();
    assert_eq!(row_b.course_count, 1);
    assert_eq!(row_b.student_count, 1);
}

// ---------------------------------------------------------------------------
// GET /reports/coach
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn coach_report_no_coach_row_returns_not_found(db: PgPool) {
    let user_id = seed_member(&db, "no-coach-row@example.com", "Password!234").await;
    let auth = auth_for(user_id, &["coach"]);

    let err = service::coach_report(&db, &test_server_config(), &auth)
        .await
        .expect_err("expected NotFound");

    assert!(matches!(err, AppError::NotFound(_)), "expected NotFound, got {err:?}");
}

#[sqlx::test]
async fn coach_report_empty_domain_is_all_zero_or_null(db: PgPool) {
    let user_id = seed_member(&db, "empty-coach@example.com", "Password!234").await;
    seed_coach(&db, user_id, "Empty Coach").await;
    let auth = auth_for(user_id, &["coach"]);

    let report = service::coach_report(&db, &test_server_config(), &auth)
        .await
        .expect("coach_report");

    assert_eq!(report.today_sessions, 0);
    assert_eq!(report.pending_attendance, 0);
    assert_eq!(report.unread_messages, 0);
    assert_eq!(report.student_count, 0);
    assert_eq!(report.attendance_rate_30d, None);
}

#[sqlx::test]
async fn coach_report_today_sessions_and_pending_attendance(db: PgPool) {
    let coach_user = seed_member(&db, "today-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user, "Today Coach").await;
    let course_id = seed_course(&db, "Today Sessions Course", Some(coach_id)).await;

    let today = Utc::now().date_naive();
    let dow = today.weekday().num_days_from_sunday() as i16;
    seed_course_schedule_slot(&db, course_id, dow, t(9, 0), t(10, 0)).await;

    let student = seed_member(&db, "today-student@example.com", "Password!234").await;
    seed_enrolment(&db, student, course_id, "active", Utc::now()).await;

    let auth = auth_for(coach_user, &["coach"]);
    let report = service::coach_report(&db, &test_server_config(), &auth)
        .await
        .expect("coach_report (before marking)");

    assert_eq!(report.today_sessions, 1);
    assert_eq!(report.pending_attendance, 1, "no attendance recorded yet for today's session");

    let session_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM course_sessions WHERE course_id = $1 AND session_date = $2",
    )
    .bind(course_id)
    .bind(today)
    .fetch_one(&db)
    .await
    .unwrap();
    let enrolment_id: Uuid =
        sqlx::query_scalar("SELECT id FROM enrolments WHERE user_id = $1 AND course_id = $2")
            .bind(student)
            .bind(course_id)
            .fetch_one(&db)
            .await
            .unwrap();
    seed_attendance(&db, session_id, enrolment_id, "present", coach_user).await;

    let report_after = service::coach_report(&db, &test_server_config(), &auth)
        .await
        .expect("coach_report (after marking)");
    assert_eq!(report_after.today_sessions, 1);
    assert_eq!(report_after.pending_attendance, 0);
}

#[sqlx::test]
async fn coach_report_attendance_rate_30d_excludes_leave_and_out_of_window(db: PgPool) {
    let coach_user = seed_member(&db, "rate-coach@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user, "Rate Coach").await;
    let course_id = seed_course(&db, "Attendance Rate Course", Some(coach_id)).await;

    let student_a = seed_member(&db, "rate-student-a@example.com", "Password!234").await;
    let student_b = seed_member(&db, "rate-student-b@example.com", "Password!234").await;
    let student_c = seed_member(&db, "rate-student-c@example.com", "Password!234").await;
    let enrolment_a = seed_enrolment(&db, student_a, course_id, "active", Utc::now()).await;
    let enrolment_b = seed_enrolment(&db, student_b, course_id, "active", Utc::now()).await;
    let enrolment_c = seed_enrolment(&db, student_c, course_id, "active", Utc::now()).await;

    let today = Utc::now().date_naive();
    let within_window = today - Duration::days(10);
    let outside_window = today - Duration::days(40);

    let session_in = seed_course_session(&db, course_id, within_window, t(9, 0), t(10, 0)).await;
    let session_out = seed_course_session(&db, course_id, outside_window, t(9, 0), t(10, 0)).await;

    // Within the 30-day window: 1 present, 1 absent, 1 leave — leave must
    // count toward neither the numerator nor the denominator.
    seed_attendance(&db, session_in, enrolment_a, "present", coach_user).await;
    seed_attendance(&db, session_in, enrolment_b, "absent", coach_user).await;
    seed_attendance(&db, session_in, enrolment_c, "leave", coach_user).await;
    // Outside the window: a present record that must not be counted.
    seed_attendance(&db, session_out, enrolment_a, "present", coach_user).await;

    let auth = auth_for(coach_user, &["coach"]);
    let report = service::coach_report(&db, &test_server_config(), &auth)
        .await
        .expect("coach_report");

    assert_eq!(
        report.attendance_rate_30d,
        Some(0.5),
        "1 present / (1 present + 1 absent) = 0.5 — leave and the out-of-window record excluded"
    );
}

#[sqlx::test]
async fn coach_report_scoped_to_own_domain(db: PgPool) {
    let coach_a_user = seed_member(&db, "scope-coach-a@example.com", "Password!234").await;
    let coach_b_user = seed_member(&db, "scope-coach-b@example.com", "Password!234").await;
    let coach_a = seed_coach(&db, coach_a_user, "Scope Coach A").await;
    let coach_b = seed_coach(&db, coach_b_user, "Scope Coach B").await;

    let course_a = seed_course(&db, "Scope Course A", Some(coach_a)).await;
    let course_b = seed_course(&db, "Scope Course B", Some(coach_b)).await;

    let today = Utc::now().date_naive();
    let dow = today.weekday().num_days_from_sunday() as i16;
    seed_course_schedule_slot(&db, course_a, dow, t(9, 0), t(10, 0)).await;
    seed_course_schedule_slot(&db, course_b, dow, t(11, 0), t(12, 0)).await;

    let student_a = seed_member(&db, "scope-student-a@example.com", "Password!234").await;
    let student_b = seed_member(&db, "scope-student-b@example.com", "Password!234").await;
    seed_enrolment(&db, student_a, course_a, "active", Utc::now()).await;
    seed_enrolment(&db, student_b, course_b, "active", Utc::now()).await;

    let auth_a = auth_for(coach_a_user, &["coach"]);
    let report_a = service::coach_report(&db, &test_server_config(), &auth_a)
        .await
        .expect("coach_report for coach A");

    assert_eq!(report_a.today_sessions, 1, "coach A should only see their own course's session");
    assert_eq!(report_a.student_count, 1, "coach A should not see coach B's student");
}

#[sqlx::test]
async fn coach_report_unread_messages_counts_only_incoming_unread(db: PgPool) {
    let coach_user = seed_member(&db, "msg-coach@example.com", "Password!234").await;
    seed_coach(&db, coach_user, "Msg Coach").await;
    let member_user = seed_member(&db, "msg-member@example.com", "Password!234").await;

    let conversation_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO conversations (id, member_id, coach_id, created_at, last_message_at) \
         VALUES ($1, $2, $3, NOW(), NOW())",
    )
    .bind(conversation_id)
    .bind(member_user)
    .bind(coach_user)
    .execute(&db)
    .await
    .unwrap();

    // Sent by the coach themself -> must not count as unread-to-me.
    seed_message(&db, conversation_id, coach_user, "hi", None, Utc::now()).await;
    // Sent by the member, already read -> must not count.
    seed_message(&db, conversation_id, member_user, "read already", Some(Utc::now()), Utc::now())
        .await;
    // Sent by the member, unread -> must count (x2).
    seed_message(&db, conversation_id, member_user, "please read", None, Utc::now()).await;
    seed_message(&db, conversation_id, member_user, "please read 2", None, Utc::now()).await;

    let auth = auth_for(coach_user, &["coach"]);
    let report = service::coach_report(&db, &test_server_config(), &auth)
        .await
        .expect("coach_report");

    assert_eq!(report.unread_messages, 2);
}

// ---------------------------------------------------------------------------
// GET /reports/me
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn member_report_empty_is_all_zero_or_null(db: PgPool) {
    let user_id = seed_member(&db, "empty-member@example.com", "Password!234").await;

    let report = service::member_report(&db, &test_server_config(), user_id)
        .await
        .expect("member_report");

    assert_eq!(report.attended_total, 0);
    assert_eq!(report.attendance_rate, None);
    assert_eq!(report.points_balance, 0);
    assert_eq!(report.active_enrolments, 0);
    assert_eq!(report.upcoming_sessions_7d, 0);
}

#[sqlx::test]
async fn member_report_attendance_rate_excludes_leave(db: PgPool) {
    let user_id = seed_member(&db, "rate-member@example.com", "Password!234").await;
    let course_id = seed_course(&db, "Member Rate Course", None).await;
    let enrolment_id = seed_enrolment(&db, user_id, course_id, "active", Utc::now()).await;

    let today = Utc::now().date_naive();
    let session_1 = seed_course_session(&db, course_id, today - Duration::days(5), t(9, 0), t(10, 0)).await;
    let session_2 = seed_course_session(&db, course_id, today - Duration::days(3), t(9, 0), t(10, 0)).await;
    let session_3 = seed_course_session(&db, course_id, today - Duration::days(1), t(9, 0), t(10, 0)).await;

    seed_attendance(&db, session_1, enrolment_id, "present", user_id).await;
    seed_attendance(&db, session_2, enrolment_id, "present", user_id).await;
    seed_attendance(&db, session_3, enrolment_id, "leave", user_id).await;

    let report = service::member_report(&db, &test_server_config(), user_id)
        .await
        .expect("member_report");

    assert_eq!(report.attended_total, 2);
    assert_eq!(
        report.attendance_rate,
        Some(1.0),
        "2 present / (2 present + 0 absent) — leave excluded from both"
    );
}

#[sqlx::test]
async fn member_report_points_balance_reflects_users_table(db: PgPool) {
    let user_id = seed_member(&db, "points-member@example.com", "Password!234").await;
    set_points_balance(&db, user_id, 1_250).await;

    let report = service::member_report(&db, &test_server_config(), user_id)
        .await
        .expect("member_report");

    assert_eq!(report.points_balance, 1_250);
}

#[sqlx::test]
async fn member_report_active_enrolments_excludes_cancelled(db: PgPool) {
    let user_id = seed_member(&db, "enrol-member@example.com", "Password!234").await;
    let course_1 = seed_course(&db, "Active Enrol Course 1", None).await;
    let course_2 = seed_course(&db, "Active Enrol Course 2", None).await;
    seed_enrolment(&db, user_id, course_1, "active", Utc::now()).await;
    seed_enrolment(&db, user_id, course_2, "cancelled", Utc::now()).await;

    let report = service::member_report(&db, &test_server_config(), user_id)
        .await
        .expect("member_report");

    assert_eq!(report.active_enrolments, 1);
}

#[sqlx::test]
async fn member_report_upcoming_sessions_7d_materializes_and_respects_window(db: PgPool) {
    let user_id = seed_member(&db, "upcoming-member@example.com", "Password!234").await;
    let course_id = seed_course(&db, "Upcoming Sessions Course", None).await;
    seed_enrolment(&db, user_id, course_id, "active", Utc::now()).await;

    let today = Utc::now().date_naive();
    let dow_today = today.weekday().num_days_from_sunday() as i16;
    seed_course_schedule_slot(&db, course_id, dow_today, t(9, 0), t(10, 0)).await;

    let far_date = today + Duration::days(10);
    let dow_far = far_date.weekday().num_days_from_sunday() as i16;
    seed_course_schedule_slot(&db, course_id, dow_far, t(11, 0), t(12, 0)).await;

    let report = service::member_report(&db, &test_server_config(), user_id)
        .await
        .expect("member_report");

    assert!(
        report.upcoming_sessions_7d >= 1,
        "today's weekly slot should materialize into an upcoming session"
    );

    let far_session_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM course_sessions WHERE course_id = $1 AND session_date = $2)",
    )
    .bind(course_id)
    .bind(far_date)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!far_session_exists, "a date 10 days out must not be materialized by the 7-day window");
}
