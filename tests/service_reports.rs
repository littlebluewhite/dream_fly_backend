//! Integration tests for `reports::service` — the `/reports/admin`,
//! `/reports/coach`, `/reports/me`, `/reports/admin/activity` aggregation
//! endpoints.
//!
//! Covered paths (task brief's Tests section):
//! - every endpoint on an empty DB returns all-zero/all-null/empty, not a 500
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
//! - `admin_activity` (Round 4 Task B8): all four `kind`s surface, results
//!   are sorted `occurred_at` descending, and the merge caps at 20 even with
//!   more candidate rows available

mod common;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::reports::service;

use common::fixtures::{
    SeedOrderLine, backdate_user, seed_attendance, seed_booking, seed_coach, seed_course,
    seed_course_schedule_slot, seed_course_schedule_slot_with_venue, seed_course_session,
    seed_course_with_capacity, seed_enrolment, seed_entitlement_product, seed_message,
    seed_order_bare, seed_order_with_items, seed_waitlist_entry, set_birth_date, set_points_balance,
};
use common::{seed_member, seed_product, seed_time_slot_on, test_server_config};

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

/// Insert a `contact_inquiries` row directly (no shared fixture exists for
/// this table), so the activity tests can control `created_at`/
/// `inquiry_type`/`subject`/`name` precisely. Unlike `seed_order` (now
/// `seed_order_bare`), `seed_attendance`, and `backdate_user` — which
/// graduated to `tests/common/fixtures.rs` once other reports tests needed
/// them too — this one stays local: no other test file touches
/// `contact_inquiries`.
async fn seed_inquiry(
    db: &PgPool,
    name: &str,
    subject: &str,
    inquiry_type: &str,
    created_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO contact_inquiries \
         (id, name, email, subject, message, status, inquiry_type, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'Test message', 'new'::inquiry_status, $5, $6, $6)",
    )
    .bind(id)
    .bind(name)
    .bind(format!("{id}@example.com"))
    .bind(subject)
    .bind(inquiry_type)
    .bind(created_at)
    .execute(db)
    .await
    .expect("insert contact_inquiry");
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

    // Round 4 Phase 4 金流 sections: zero-filled, never a 500.
    assert_eq!(report.kpis.new_members.this_month, 0);
    assert_eq!(report.kpis.new_members.last_month, 0);
    assert_eq!(report.kpis.new_enrolments.this_month, 0);
    assert_eq!(report.kpis.new_enrolments.last_month, 0);
    assert_eq!(report.kpis.paid_orders_count.this_month, 0);
    assert_eq!(report.kpis.paid_orders_count.last_month, 0);
    assert_eq!(report.kpis.attendance_rate.this_month, None, "no-data month must be null");
    assert_eq!(report.kpis.attendance_rate.last_month, None);

    // Always all 6 sources for the current month, in canonical order.
    let breakdown_sources: Vec<&str> =
        report.revenue_breakdown.iter().map(|r| r.source.as_str()).collect();
    assert_eq!(
        breakdown_sources,
        ["course", "ticket", "membership", "course_package", "merchandise", "venue_rental"]
    );
    assert!(report.revenue_breakdown.iter().all(|r| r.gross_cents == 0));
    assert!(report.revenue_breakdown.iter().all(|r| r.orders_count == 0 && r.units == 0));

    // 12 months × 6 sources, zero-filled like `revenue.trend`, oldest first.
    assert_eq!(report.income_sources_12m.len(), 12 * 6);
    assert!(report.income_sources_12m.iter().all(|r| r.gross_cents == 0));
    assert_eq!(
        report.income_sources_12m.first().unwrap().month,
        report.revenue.trend.first().unwrap().month,
        "12m income series must start at the same oldest month as revenue.trend"
    );
    assert_eq!(
        report.income_sources_12m.last().unwrap().month,
        report.revenue.trend.last().unwrap().month
    );

    // Order-line sources only (no venue_rental) — ratio undefined on a
    // zero-gross month, not 0/NaN.
    let split_sources: Vec<&str> =
        report.category_split.iter().map(|r| r.source.as_str()).collect();
    assert_eq!(split_sources, ["course", "ticket", "membership", "course_package", "merchandise"]);
    assert!(report.category_split.iter().all(|r| r.gross_cents == 0 && r.ratio.is_none()));

    assert!(report.payment_split.is_empty());

    // Round 4 Phase 4 人流 sections: fixed-bucket zero-fills, never a 500.
    assert_eq!(report.attendance_distribution.len(), 4, "attDist always 4 fixed bands");
    assert!(report.attendance_distribution.iter().all(|r| r.count == 0));

    let age_buckets: Vec<&str> =
        report.age_distribution.iter().map(|r| r.bucket.as_str()).collect();
    assert_eq!(age_buckets, ["0-6", "7-12", "13-17", "18-25", "26-40", "41+"]);
    assert!(report.age_distribution.iter().all(|r| r.count == 0));

    let tier_buckets: Vec<&str> =
        report.tier_distribution.iter().map(|r| r.bucket.as_str()).collect();
    assert_eq!(tier_buckets, ["regular", "bronze", "silver", "gold"]);
    assert!(report.tier_distribution.iter().all(|r| r.count == 0));

    // 6 retention buckets, newest last, all zero with a null rate.
    assert_eq!(report.retention.len(), 6);
    assert!(
        report
            .retention
            .iter()
            .all(|r| r.new_count == 0 && r.returning_count == 0 && r.rate.is_none())
    );

    assert_eq!(report.funnel.trial_inquiries, 0);
    assert_eq!(report.funnel.new_enrolments, 0);

    // 7 weekday buckets, 0=Sunday..6=Saturday (§3.18 convention), zero-filled.
    let weekdays: Vec<i16> = report.weekday_load.iter().map(|r| r.weekday).collect();
    assert_eq!(weekdays, [0, 1, 2, 3, 4, 5, 6]);
    assert!(report.weekday_load.iter().all(|r| r.present_count == 0));

    // No sessions -> no venue rows (not a fixed-bucket dimension).
    assert!(report.venue_usage.is_empty());
}

#[sqlx::test]
async fn admin_report_revenue_counts_only_paid_family(db: PgPool) {
    let user_id = seed_member(&db, "revenue-buyer@example.com", "Password!234").await;
    let now = Utc::now();

    seed_order_bare(&db, user_id, "paid", 10_000, Some(now)).await;
    seed_order_bare(&db, user_id, "processing", 20_000, Some(now)).await;
    seed_order_bare(&db, user_id, "completed", 30_000, Some(now)).await;
    // None of these should count, even though each has a real `paid_at` in
    // the current month — a refunded order keeps its original `paid_at`
    // (see `orders::repository::update_status_and_paid_at_tx`), so the
    // filter must be on `status`, not `paid_at IS NOT NULL`.
    seed_order_bare(&db, user_id, "refunded", 999_999, Some(now)).await;
    seed_order_bare(&db, user_id, "cancelled", 999_999, Some(now)).await;
    seed_order_bare(&db, user_id, "pending", 999_999, None).await;

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

    seed_order_bare(&db, user_id, "paid", 1_000, Some(now)).await;
    seed_order_bare(&db, user_id, "paid", 2_000, Some(last_month)).await;
    seed_order_bare(&db, user_id, "paid", 3_000, Some(oldest_month)).await;

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

    backdate_user(&db, old_user, months_ago(Utc::now(), 3)).await;

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
// GET /reports/admin — Round 4 Phase 4 金流 sections
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn admin_report_kpis_split_this_and_last_month(db: PgPool) {
    let now = Utc::now();
    let last_month = months_ago(now, 1);
    let three_months_ago = months_ago(now, 3);

    // Helper users whose creation must not pollute the new-member KPI.
    let student = seed_member(&db, "kpi-student@example.com", "Password!234").await;
    let buyer = seed_member(&db, "kpi-buyer@example.com", "Password!234").await;
    backdate_user(&db, student, three_months_ago).await;
    backdate_user(&db, buyer, three_months_ago).await;

    // new_members: 1 this month, 1 last month.
    let _member_this = seed_member(&db, "kpi-new-this@example.com", "Password!234").await;
    let member_last = seed_member(&db, "kpi-new-last@example.com", "Password!234").await;
    backdate_user(&db, member_last, last_month).await;

    // new_enrolments: 1 this month, 1 last month; a cancelled one this
    // month must not count.
    let course_a = seed_course(&db, "KPI Course A", None).await;
    let course_b = seed_course(&db, "KPI Course B", None).await;
    let course_c = seed_course(&db, "KPI Course C", None).await;
    let enrolment_a = seed_enrolment(&db, student, course_a, "active", now).await;
    let enrolment_b = seed_enrolment(&db, student, course_b, "cancelled", now).await;
    let enrolment_c = seed_enrolment(&db, student, course_c, "active", last_month).await;

    // paid_orders_count: refunded/pending never count, even with the same
    // month's `paid_at`.
    seed_order_bare(&db, buyer, "paid", 1_000, Some(now)).await;
    seed_order_bare(&db, buyer, "completed", 2_000, Some(last_month)).await;
    seed_order_bare(&db, buyer, "refunded", 3_000, Some(now)).await;
    seed_order_bare(&db, buyer, "pending", 4_000, None).await;

    // attendance_rate this month: 1 present / (1 present + 1 absent) = 0.5,
    // leave in neither numerator nor denominator; last month has no records
    // at all -> null, not 0. Sessions pinned to the 1st of this month so
    // "today near a month boundary" can't skew the bucket.
    let first_of_this_month = months_ago(now, 0).date_naive();
    let s1 = seed_course_session(&db, course_a, first_of_this_month, t(9, 0), t(10, 0)).await;
    let s2 = seed_course_session(&db, course_b, first_of_this_month, t(9, 0), t(10, 0)).await;
    let s3 = seed_course_session(&db, course_c, first_of_this_month, t(9, 0), t(10, 0)).await;
    seed_attendance(&db, s1, enrolment_a, "present", student).await;
    seed_attendance(&db, s2, enrolment_b, "absent", student).await;
    seed_attendance(&db, s3, enrolment_c, "leave", student).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    assert_eq!(report.kpis.new_members.this_month, 1);
    assert_eq!(report.kpis.new_members.last_month, 1);
    assert_eq!(report.kpis.new_enrolments.this_month, 1, "cancelled enrolment must not count");
    assert_eq!(report.kpis.new_enrolments.last_month, 1);
    assert_eq!(report.kpis.paid_orders_count.this_month, 1);
    assert_eq!(report.kpis.paid_orders_count.last_month, 1);
    assert_eq!(report.kpis.attendance_rate.this_month, Some(0.5));
    assert_eq!(report.kpis.attendance_rate.last_month, None, "no-data month must be null");
}

#[sqlx::test]
async fn admin_report_breakdown_excludes_pending_and_refunded(db: PgPool) {
    let now = Utc::now();
    let buyer = seed_member(&db, "breakdown-buyer@example.com", "Password!234").await;
    let course_id = seed_course(&db, "Breakdown Course", None).await;
    let merch_id = seed_product(&db, "breakdown-merch", 5_000, Some(10)).await;

    let excluded_lines = [
        SeedOrderLine::Course { course_id, unit_price_cents: 50_000 },
        SeedOrderLine::Product { product_id: merch_id, quantity: 2, unit_price_cents: 5_000 },
    ];
    // `refunded` keeps its real `paid_at` — exclusion must come from the
    // status filter, not from `paid_at IS NULL`.
    seed_order_with_items(&db, buyer, "refunded", None, Some(now), &excluded_lines).await;
    seed_order_with_items(&db, buyer, "pending", None, None, &excluded_lines).await;

    seed_order_with_items(
        &db,
        buyer,
        "paid",
        None,
        Some(now),
        &[SeedOrderLine::Course { course_id, unit_price_cents: 10_000 }],
    )
    .await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let course_row =
        report.revenue_breakdown.iter().find(|r| r.source == "course").expect("course row");
    assert_eq!(course_row.gross_cents, 10_000, "only the paid order's course line counts");
    assert_eq!(course_row.orders_count, 1);
    assert_eq!(course_row.units, 1);

    let merch_row =
        report.revenue_breakdown.iter().find(|r| r.source == "merchandise").expect("merch row");
    assert_eq!(merch_row.gross_cents, 0, "refunded/pending product lines must not count");
    assert_eq!(merch_row.orders_count, 0);
}

#[sqlx::test]
async fn admin_report_category_split_ticket_bucket_only_product_type_ticket(db: PgPool) {
    let now = Utc::now();
    let buyer = seed_member(&db, "split-buyer@example.com", "Password!234").await;
    let course_id = seed_course(&db, "Split Course", None).await;
    let ticket_id =
        seed_entitlement_product(&db, "split-ticket", "ticket", 10_000, Some(30), Some(10)).await;
    let merch_id = seed_product(&db, "split-merch", 5_000, Some(10)).await;

    // Gross this month: ticket 20_000 + merchandise 5_000 + course 25_000
    // = 50_000. A single order carrying all three lines proves per-line
    // (not per-order) bucketing.
    seed_order_with_items(
        &db,
        buyer,
        "paid",
        None,
        Some(now),
        &[
            SeedOrderLine::Product { product_id: ticket_id, quantity: 2, unit_price_cents: 10_000 },
            SeedOrderLine::Product { product_id: merch_id, quantity: 1, unit_price_cents: 5_000 },
            SeedOrderLine::Course { course_id, unit_price_cents: 25_000 },
        ],
    )
    .await;

    // A venue booking this month must show up in revenue_breakdown but stay
    // out of category_split (order-line 毛額 only) and its ratios.
    let slot_id = seed_time_slot_on(&db, 10, months_ago(now, 0).date_naive()).await;
    seed_booking(&db, buyer, slot_id, "confirmed", 100_000).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let find = |source: &str| {
        report.category_split.iter().find(|r| r.source == source).expect("split row")
    };
    assert_eq!(find("ticket").gross_cents, 20_000, "ticket bucket only product_type=ticket");
    assert_eq!(find("ticket").ratio, Some(0.4));
    assert_eq!(find("merchandise").gross_cents, 5_000);
    assert_eq!(find("merchandise").ratio, Some(0.1));
    assert_eq!(find("course").gross_cents, 25_000);
    assert_eq!(find("course").ratio, Some(0.5));
    assert_eq!(find("membership").gross_cents, 0);
    assert_eq!(find("membership").ratio, Some(0.0));
    assert!(
        report.category_split.iter().all(|r| r.source != "venue_rental"),
        "venue rental is not an order-line category"
    );

    let venue_row = report
        .revenue_breakdown
        .iter()
        .find(|r| r.source == "venue_rental")
        .expect("venue row");
    assert_eq!(venue_row.gross_cents, 100_000, "…but it does appear in revenue_breakdown");
}

#[sqlx::test]
async fn admin_report_venue_rental_counts_only_confirmed_completed(db: PgPool) {
    let now = Utc::now();
    let this_month_date = months_ago(now, 0).date_naive();
    let last_month_date = months_ago(now, 1).date_naive();

    // One user per booking — `uq_bookings_user_slot_active` forbids one
    // user holding two non-cancelled bookings on the same slot.
    let u1 = seed_member(&db, "venue-1@example.com", "Password!234").await;
    let u2 = seed_member(&db, "venue-2@example.com", "Password!234").await;
    let u3 = seed_member(&db, "venue-3@example.com", "Password!234").await;
    let u4 = seed_member(&db, "venue-4@example.com", "Password!234").await;
    let u5 = seed_member(&db, "venue-5@example.com", "Password!234").await;

    let slot_this = seed_time_slot_on(&db, 10, this_month_date).await;
    let slot_last = seed_time_slot_on(&db, 10, last_month_date).await;

    seed_booking(&db, u1, slot_this, "confirmed", 5_000).await;
    seed_booking(&db, u2, slot_this, "completed", 3_000).await;
    seed_booking(&db, u3, slot_this, "cancelled", 99_999).await;
    seed_booking(&db, u4, slot_this, "no_show", 99_999).await;
    seed_booking(&db, u5, slot_this, "pending", 99_999).await;
    // Booked *now*, but the slot's use date is last month — 歸屬 slot 使用日.
    seed_booking(&db, u1, slot_last, "confirmed", 7_000).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let venue_row = report
        .revenue_breakdown
        .iter()
        .find(|r| r.source == "venue_rental")
        .expect("venue row");
    assert_eq!(venue_row.gross_cents, 8_000, "confirmed+completed only, this month only");
    assert_eq!(venue_row.orders_count, 2);
    assert_eq!(venue_row.units, 2);

    let last_month_key = last_month_date.format("%Y-%m").to_string();
    let venue_last = report
        .income_sources_12m
        .iter()
        .find(|r| r.source == "venue_rental" && r.month == last_month_key)
        .expect("last month venue row");
    assert_eq!(
        venue_last.gross_cents, 7_000,
        "booking made today for a last-month slot lands in the slot's month"
    );
}

#[sqlx::test]
async fn admin_report_income_sources_12m_buckets_by_paid_month(db: PgPool) {
    let now = Utc::now();
    let buyer = seed_member(&db, "sources-12m@example.com", "Password!234").await;
    let course_id = seed_course(&db, "Sources 12m Course", None).await;

    let line = [SeedOrderLine::Course { course_id, unit_price_cents: 1_000 }];
    seed_order_with_items(&db, buyer, "paid", None, Some(now), &line).await;
    seed_order_with_items(&db, buyer, "paid", None, Some(months_ago(now, 1)), &line).await;
    // 12 months back = outside the 12-slot window (current + 11 previous).
    seed_order_with_items(&db, buyer, "paid", None, Some(months_ago(now, 12)), &line).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let course_by_month: Vec<(&str, i64)> = report
        .income_sources_12m
        .iter()
        .filter(|r| r.source == "course")
        .map(|r| (r.month.as_str(), r.gross_cents))
        .collect();
    assert_eq!(course_by_month.len(), 12);

    let this_key = now.format("%Y-%m").to_string();
    let last_key = months_ago(now, 1).format("%Y-%m").to_string();
    for (month, gross) in &course_by_month {
        let expected = if *month == this_key || *month == last_key { 1_000 } else { 0 };
        assert_eq!(*gross, expected, "month {month}");
    }
    assert_eq!(
        course_by_month.iter().map(|(_, g)| g).sum::<i64>(),
        2_000,
        "the 12-months-ago order must have fallen off the window"
    );
}

#[sqlx::test]
async fn admin_report_payment_split_null_method_is_unknown(db: PgPool) {
    let now = Utc::now();
    let buyer = seed_member(&db, "paysplit-buyer@example.com", "Password!234").await;

    // payment_split counts orders, not lines — no items needed.
    seed_order_with_items(&db, buyer, "paid", Some("credit_card"), Some(now), &[]).await;
    seed_order_with_items(&db, buyer, "completed", Some("credit_card"), Some(now), &[]).await;
    seed_order_with_items(&db, buyer, "processing", Some("line_pay"), Some(now), &[]).await;
    seed_order_with_items(&db, buyer, "paid", None, Some(now), &[]).await;
    // Excluded: wrong status / wrong month.
    seed_order_with_items(&db, buyer, "refunded", Some("credit_card"), Some(now), &[]).await;
    seed_order_with_items(&db, buyer, "paid", Some("line_pay"), Some(months_ago(now, 1)), &[])
        .await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    assert_eq!(report.payment_split.len(), 3, "got {:?}", report.payment_split);
    let count_of = |method: &str| {
        report
            .payment_split
            .iter()
            .find(|r| r.method == method)
            .map(|r| r.count)
            .unwrap_or_else(|| panic!("missing method {method}"))
    };
    assert_eq!(count_of("credit_card"), 2);
    assert_eq!(count_of("line_pay"), 1);
    assert_eq!(count_of("unknown"), 1, "NULL payment_method surfaces as the literal 'unknown' key");
}

#[sqlx::test]
async fn admin_report_coach_revenue_only_course_lines(db: PgPool) {
    let now = Utc::now();
    let buyer = seed_member(&db, "coachrev-buyer@example.com", "Password!234").await;
    let coach_a_user = seed_member(&db, "coachrev-a@example.com", "Password!234").await;
    let coach_b_user = seed_member(&db, "coachrev-b@example.com", "Password!234").await;
    let coach_a = seed_coach(&db, coach_a_user, "Coach Rev A").await;
    let coach_b = seed_coach(&db, coach_b_user, "Coach Rev B").await;

    let course_a = seed_course(&db, "Coach Rev Course A", Some(coach_a)).await;
    let course_b = seed_course(&db, "Coach Rev Course B", Some(coach_b)).await;
    let course_orphan = seed_course(&db, "Coach Rev Orphan Course", None).await;
    let ticket_id =
        seed_entitlement_product(&db, "coachrev-ticket", "ticket", 20_000, Some(30), Some(10))
            .await;

    // Mixed order: the course line goes to coach A, the ticket line goes to
    // no coach at all.
    seed_order_with_items(
        &db,
        buyer,
        "paid",
        None,
        Some(now),
        &[
            SeedOrderLine::Course { course_id: course_a, unit_price_cents: 50_000 },
            SeedOrderLine::Product { product_id: ticket_id, quantity: 1, unit_price_cents: 20_000 },
        ],
    )
    .await;
    // Oldest in-window month (11 months back) still counts for coach B…
    seed_order_with_items(
        &db,
        buyer,
        "paid",
        None,
        Some(months_ago(now, 11)),
        &[SeedOrderLine::Course { course_id: course_b, unit_price_cents: 30_000 }],
    )
    .await;
    // …but 12 months back is outside the window, and refunded never counts.
    seed_order_with_items(
        &db,
        buyer,
        "paid",
        None,
        Some(months_ago(now, 12)),
        &[SeedOrderLine::Course { course_id: course_a, unit_price_cents: 99_999 }],
    )
    .await;
    seed_order_with_items(
        &db,
        buyer,
        "refunded",
        None,
        Some(now),
        &[SeedOrderLine::Course { course_id: course_a, unit_price_cents: 88_888 }],
    )
    .await;
    // A coachless course's line is attributed to nobody (and must not 500).
    seed_order_with_items(
        &db,
        buyer,
        "paid",
        None,
        Some(now),
        &[SeedOrderLine::Course { course_id: course_orphan, unit_price_cents: 7_777 }],
    )
    .await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let row_a = report.coaches.iter().find(|c| c.coach_id == coach_a).expect("coach A row");
    assert_eq!(
        row_a.revenue_cents_12m, 50_000,
        "course line only — the same order's ticket line must not attribute to the coach"
    );
    let row_b = report.coaches.iter().find(|c| c.coach_id == coach_b).expect("coach B row");
    assert_eq!(row_b.revenue_cents_12m, 30_000, "11 months back is still inside the window");
}

// ---------------------------------------------------------------------------
// GET /reports/admin — Round 4 Phase 4 人流 sections
// ---------------------------------------------------------------------------

/// A `birth_date` that lands a member at exactly `years` old today: born
/// Jan 1 of `today.year() - years`, which is always on or before today's
/// month/day within the current year, so the full-year age is exactly
/// `years` regardless of when the test runs.
fn birth_for_age(today: NaiveDate, years: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(today.year() - years, 1, 1).expect("valid Jan-1 birth date")
}

#[sqlx::test]
async fn admin_report_att_dist_excludes_leave_and_unmarked(db: PgPool) {
    let course_id = seed_course(&db, "AttDist Course", None).await;

    // A member marked present once -> rate 1.0 -> gte_95.
    let m_present = seed_member(&db, "attdist-present@example.com", "Password!234").await;
    let e_present = seed_enrolment(&db, m_present, course_id, "active", Utc::now()).await;
    // A member present once + absent once -> 0.5 -> lt_75.
    let m_low = seed_member(&db, "attdist-low@example.com", "Password!234").await;
    let e_low = seed_enrolment(&db, m_low, course_id, "active", Utc::now()).await;
    // A member marked only `leave` -> denominator 0 -> excluded.
    let m_leave = seed_member(&db, "attdist-leave@example.com", "Password!234").await;
    let e_leave = seed_enrolment(&db, m_leave, course_id, "active", Utc::now()).await;
    // A member enrolled but never marked -> excluded.
    let _m_unmarked = seed_member(&db, "attdist-unmarked@example.com", "Password!234").await;

    let today = Utc::now().date_naive();
    let s1 = seed_course_session(&db, course_id, today - Duration::days(5), t(9, 0), t(10, 0)).await;
    let s2 = seed_course_session(&db, course_id, today - Duration::days(4), t(9, 0), t(10, 0)).await;
    seed_attendance(&db, s1, e_present, "present", m_present).await;
    seed_attendance(&db, s1, e_low, "present", m_low).await;
    seed_attendance(&db, s2, e_low, "absent", m_low).await;
    seed_attendance(&db, s1, e_leave, "leave", m_leave).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let count = |bucket: &str| {
        report.attendance_distribution.iter().find(|r| r.bucket == bucket).unwrap().count
    };
    assert_eq!(count("gte_95"), 1, "the always-present member");
    assert_eq!(count("lt_75"), 1, "the 50% member");
    assert_eq!(count("85_94"), 0);
    assert_eq!(count("75_84"), 0);
    assert_eq!(
        report.attendance_distribution.iter().map(|r| r.count).sum::<i64>(),
        2,
        "leave-only and never-marked members must not appear in the distribution"
    );
}

#[sqlx::test]
async fn admin_report_retention_new_returning_and_null_rate(db: PgPool) {
    let now = Utc::now();
    let course_id = seed_course(&db, "Retention Course", None).await;
    let user_id = seed_member(&db, "retention-user@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, user_id, course_id, "active", now).await;

    // Present in last month (first-ever active month) and again this month.
    // Sessions pinned to the 1st of each month so bucketing can't straddle a
    // boundary.
    let s_last = seed_course_session(&db, course_id, months_ago(now, 1).date_naive(), t(9, 0), t(10, 0)).await;
    let s_this = seed_course_session(&db, course_id, months_ago(now, 0).date_naive(), t(9, 0), t(10, 0)).await;
    seed_attendance(&db, s_last, enrolment_id, "present", user_id).await;
    seed_attendance(&db, s_this, enrolment_id, "present", user_id).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let this_key = now.format("%Y-%m").to_string();
    let last_key = months_ago(now, 1).format("%Y-%m").to_string();

    let this_row = report.retention.iter().find(|r| r.month == this_key).expect("this month row");
    assert_eq!(this_row.new_count, 0, "user's first active month was last month, not this");
    assert_eq!(this_row.returning_count, 1, "active this month with a prior active month");
    assert_eq!(this_row.rate, Some(1.0), "the one last-month-active user stayed active this month");

    let last_row = report.retention.iter().find(|r| r.month == last_key).expect("last month row");
    assert_eq!(last_row.new_count, 1, "first active month = new");
    assert_eq!(last_row.returning_count, 0);
    assert_eq!(last_row.rate, None, "month before had no active users -> rate null, not 0");
}

#[sqlx::test]
async fn admin_report_age_dist_excludes_null_birth_date(db: PgPool) {
    let today = Utc::now().date_naive();

    let m_child = seed_member(&db, "age-child@example.com", "Password!234").await;
    set_birth_date(&db, m_child, Some(birth_for_age(today, 5))).await; // -> 0-6
    let m_teen = seed_member(&db, "age-teen@example.com", "Password!234").await;
    set_birth_date(&db, m_teen, Some(birth_for_age(today, 15))).await; // -> 13-17
    // NULL birth_date -> excluded from the distribution entirely.
    let _m_null = seed_member(&db, "age-null@example.com", "Password!234").await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let count = |bucket: &str| {
        report.age_distribution.iter().find(|r| r.bucket == bucket).unwrap().count
    };
    assert_eq!(count("0-6"), 1);
    assert_eq!(count("13-17"), 1);
    assert_eq!(
        report.age_distribution.iter().map(|r| r.count).sum::<i64>(),
        2,
        "the NULL-birth_date member must be excluded"
    );
}

#[sqlx::test]
async fn admin_report_funnel_honest_two_stages_90_day_window(db: PgPool) {
    let now = Utc::now();

    // trial_inquiries: only inquiry_type='trial' inside the 90-day window.
    seed_inquiry(&db, "Recent Trial", "試上諮詢", "trial", now - Duration::days(5)).await;
    seed_inquiry(&db, "Old Trial", "試上諮詢", "trial", now - Duration::days(91)).await; // out of window
    seed_inquiry(&db, "General", "一般諮詢", "general", now - Duration::days(1)).await; // wrong type

    // new_enrolments: enrolments created (not cancelled) inside the window.
    let user_id = seed_member(&db, "funnel-user@example.com", "Password!234").await;
    let c1 = seed_course(&db, "Funnel Course 1", None).await;
    let c2 = seed_course(&db, "Funnel Course 2", None).await;
    let c3 = seed_course(&db, "Funnel Course 3", None).await;
    seed_enrolment(&db, user_id, c1, "active", now - Duration::days(5)).await;
    seed_enrolment(&db, user_id, c2, "cancelled", now - Duration::days(5)).await; // excluded
    seed_enrolment(&db, user_id, c3, "active", now - Duration::days(91)).await; // out of window

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    assert_eq!(
        report.funnel.trial_inquiries, 1,
        "only the recent trial counts (91-days-ago and general excluded)"
    );
    assert_eq!(
        report.funnel.new_enrolments, 1,
        "cancelled and 91-days-ago enrolments excluded"
    );
}

#[sqlx::test]
async fn admin_report_weekday_load_indexes_sunday_as_zero(db: PgPool) {
    let course_id = seed_course(&db, "Weekday Course", None).await;
    let u1 = seed_member(&db, "weekday-1@example.com", "Password!234").await;
    let u2 = seed_member(&db, "weekday-2@example.com", "Password!234").await;
    let u3 = seed_member(&db, "weekday-3@example.com", "Password!234").await;
    let e1 = seed_enrolment(&db, u1, course_id, "active", Utc::now()).await;
    let e2 = seed_enrolment(&db, u2, course_id, "active", Utc::now()).await;
    let e3 = seed_enrolment(&db, u3, course_id, "active", Utc::now()).await;

    let today = Utc::now().date_naive();
    // The most recent Sunday (index 0) and Wednesday (index 3) on/before today
    // — both within the trailing-30-day window.
    let sunday = today - Duration::days(i64::from(today.weekday().num_days_from_sunday()));
    let wednesday = today - Duration::days(i64::from((today.weekday().num_days_from_sunday() + 4) % 7));
    assert_eq!(sunday.weekday().num_days_from_sunday(), 0);
    assert_eq!(wednesday.weekday().num_days_from_sunday(), 3);

    let s_sun = seed_course_session(&db, course_id, sunday, t(9, 0), t(10, 0)).await;
    let s_wed = seed_course_session(&db, course_id, wednesday, t(11, 0), t(12, 0)).await;
    // 2 present on Sunday, 1 present on Wednesday.
    seed_attendance(&db, s_sun, e1, "present", u1).await;
    seed_attendance(&db, s_sun, e2, "present", u2).await;
    seed_attendance(&db, s_wed, e3, "present", u3).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let by_day = |weekday: i16| {
        report.weekday_load.iter().find(|r| r.weekday == weekday).unwrap().present_count
    };
    assert_eq!(report.weekday_load.len(), 7);
    assert_eq!(by_day(0), 2, "Sunday present headcount (index 0)");
    assert_eq!(by_day(3), 1, "Wednesday present headcount (index 3)");
    assert_eq!(
        report.weekday_load.iter().map(|r| r.present_count).sum::<i64>(),
        3,
        "no attendance leaked into other weekdays"
    );
}

#[sqlx::test]
async fn admin_report_tier_dist_threshold_boundaries(db: PgPool) {
    // <500 regular / 500–1999 bronze / 2000–4999 silver / ≥5000 gold.
    let cases = [499_i64, 500, 1_999, 2_000, 4_999, 5_000];
    for (i, balance) in cases.iter().enumerate() {
        let u = seed_member(&db, &format!("tier-{i}@example.com"), "Password!234").await;
        set_points_balance(&db, u, *balance).await;
    }

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let count = |bucket: &str| {
        report.tier_distribution.iter().find(|r| r.bucket == bucket).unwrap().count
    };
    assert_eq!(count("regular"), 1, "499 -> regular");
    assert_eq!(count("bronze"), 2, "500 and 1999 -> bronze");
    assert_eq!(count("silver"), 2, "2000 and 4999 -> silver");
    assert_eq!(count("gold"), 1, "5000 -> gold");
}

#[sqlx::test]
async fn admin_report_venue_usage_sums_minutes_per_venue(db: PgPool) {
    // Two venues each backed by a weekly schedule slot; the admin report
    // materializes this month's sessions, so the summed minutes are exactly
    // (this-month session count) × (per-session duration). A NULL-venue slot
    // and a session matching no slot must both be excluded.
    let course_a = seed_course(&db, "Venue A Course", None).await;
    let course_b = seed_course(&db, "Venue B Course", None).await;
    let course_c = seed_course(&db, "Venue C Course", None).await;
    let course_d = seed_course(&db, "Venue D Course", None).await;

    // A館: 09:00–11:00 = 120 min; B教室: 14:00–15:00 = 60 min.
    seed_course_schedule_slot_with_venue(&db, course_a, 1, t(9, 0), t(11, 0), "A 訓練館").await;
    seed_course_schedule_slot_with_venue(&db, course_b, 2, t(14, 0), t(15, 0), "B 教室").await;
    // NULL venue -> excluded.
    seed_course_schedule_slot(&db, course_c, 3, t(10, 0), t(11, 0)).await;

    // A directly-seeded session for course_d at a time no slot matches -> the
    // slot join finds no venue -> excluded. Pinned to the 1st of this month so
    // it is inside the venue-usage window.
    let month_start = Utc::now().date_naive().with_day(1).unwrap();
    seed_course_session(&db, course_d, month_start, t(23, 0), t(23, 30)).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let n_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM course_sessions WHERE course_id = $1")
        .bind(course_a)
        .fetch_one(&db)
        .await
        .unwrap();
    let n_b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM course_sessions WHERE course_id = $1")
        .bind(course_b)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(n_a >= 1 && n_b >= 1, "materialize should create ≥1 session for each slot this month");

    let minutes = |venue: &str| report.venue_usage.iter().find(|r| r.venue == venue).map(|r| r.minutes);
    assert_eq!(minutes("A 訓練館"), Some(n_a * 120));
    assert_eq!(minutes("B 教室"), Some(n_b * 60));
    assert!(
        report.venue_usage.iter().all(|r| r.venue == "A 訓練館" || r.venue == "B 教室"),
        "NULL-venue slot and no-slot session must be excluded, got {:?}",
        report.venue_usage.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>()
    );
}

#[sqlx::test]
async fn admin_report_coach_attendance_rate_excludes_leave(db: PgPool) {
    let coach_user = seed_member(&db, "coachatt-user@example.com", "Password!234").await;
    let coach_id = seed_coach(&db, coach_user, "Coach Att").await;
    let course_id = seed_course(&db, "Coach Att Course", Some(coach_id)).await;
    let student = seed_member(&db, "coachatt-student@example.com", "Password!234").await;
    let enrolment_id = seed_enrolment(&db, student, course_id, "active", Utc::now()).await;

    // A second coach with a course but no attendance at all -> rate null.
    let coach_b_user = seed_member(&db, "coachatt-b@example.com", "Password!234").await;
    let coach_b = seed_coach(&db, coach_b_user, "Coach Att B").await;
    seed_course(&db, "Coach Att B Course", Some(coach_b)).await;

    let today = Utc::now().date_naive();
    let s1 = seed_course_session(&db, course_id, today - Duration::days(5), t(9, 0), t(10, 0)).await;
    let s2 = seed_course_session(&db, course_id, today - Duration::days(4), t(9, 0), t(10, 0)).await;
    let s3 = seed_course_session(&db, course_id, today - Duration::days(3), t(9, 0), t(10, 0)).await;
    seed_attendance(&db, s1, enrolment_id, "present", coach_user).await;
    seed_attendance(&db, s2, enrolment_id, "absent", coach_user).await;
    seed_attendance(&db, s3, enrolment_id, "leave", coach_user).await;

    let report = service::admin_report(&db, &test_server_config())
        .await
        .expect("admin_report");

    let row_a = report.coaches.iter().find(|c| c.coach_id == coach_id).expect("coach A row");
    assert_eq!(
        row_a.attendance_rate,
        Some(0.5),
        "1 present / (1 present + 1 absent) — leave excluded from the denominator"
    );
    let row_b = report.coaches.iter().find(|c| c.coach_id == coach_b).expect("coach B row");
    assert_eq!(row_b.attendance_rate, None, "a coach with no attendance records -> null");
}

// ---------------------------------------------------------------------------
// GET /reports/coach
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn coach_report_no_coach_row_returns_not_found(db: PgPool) {
    let user_id = seed_member(&db, "no-coach-row@example.com", "Password!234").await;
    let auth = common::coach_auth(user_id);

    let err = service::coach_report(&db, &test_server_config(), &auth)
        .await
        .expect_err("expected NotFound");

    assert!(matches!(err, AppError::NotFound(_)), "expected NotFound, got {err:?}");
}

#[sqlx::test]
async fn coach_report_empty_domain_is_all_zero_or_null(db: PgPool) {
    let user_id = seed_member(&db, "empty-coach@example.com", "Password!234").await;
    seed_coach(&db, user_id, "Empty Coach").await;
    let auth = common::coach_auth(user_id);

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

    let auth = common::coach_auth(coach_user);
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

    let auth = common::coach_auth(coach_user);
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

    let auth_a = common::coach_auth(coach_a_user);
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

    let auth = common::coach_auth(coach_user);
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

// ---------------------------------------------------------------------------
// GET /reports/admin/activity
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn admin_activity_empty_db_returns_empty_items(db: PgPool) {
    let report = service::admin_activity(&db).await.expect("admin_activity");
    assert!(report.items.is_empty());
}

#[sqlx::test]
async fn admin_activity_includes_all_four_kinds_sorted_desc(db: PgPool) {
    let now = Utc::now();

    // A brand-new user (kind=user) — oldest of the four headline events.
    let new_user_id = seed_member(&db, "activity-newuser@example.com", "Password!234").await;
    sqlx::query("UPDATE users SET created_at = $2, name = $3 WHERE id = $1")
        .bind(new_user_id)
        .bind(now - Duration::minutes(40))
        .bind("Activity New User")
        .execute(&db)
        .await
        .expect("backdate new user");

    // A buyer + paid order (kind=order).
    let buyer_id = seed_member(&db, "activity-buyer@example.com", "Password!234").await;
    seed_order_bare(&db, buyer_id, "paid", 50_000, Some(now - Duration::minutes(30))).await;

    // A course + enrolment (kind=enrolment).
    let course_id = seed_course(&db, "Activity Feed Course", None).await;
    let student_id = seed_member(&db, "activity-student@example.com", "Password!234").await;
    seed_enrolment(&db, student_id, course_id, "active", now - Duration::minutes(20)).await;

    // A contact inquiry (kind=inquiry) — newest of the four.
    seed_inquiry(&db, "Activity Asker", "課程諮詢", "general", now - Duration::minutes(10)).await;

    let report = service::admin_activity(&db).await.expect("admin_activity");

    let kinds: Vec<&str> = report.items.iter().map(|i| i.kind.as_str()).collect();
    for expected in ["user", "order", "enrolment", "inquiry"] {
        assert!(kinds.contains(&expected), "missing kind {expected} in {kinds:?}");
    }

    // The whole merged+sorted list (including the buyer/student's own
    // incidental `user` rows) must be non-increasing by `occurred_at`.
    for pair in report.items.windows(2) {
        assert!(
            pair[0].occurred_at >= pair[1].occurred_at,
            "items must be sorted occurred_at desc, got {:?}",
            report.items.iter().map(|i| (&i.kind, i.occurred_at)).collect::<Vec<_>>()
        );
    }

    let user_item = report
        .items
        .iter()
        .find(|i| i.label == "新會員註冊:Activity New User")
        .expect("headline user activity present");
    let order_item = report.items.iter().find(|i| i.kind == "order").expect("order activity present");
    let enrolment_item =
        report.items.iter().find(|i| i.kind == "enrolment").expect("enrolment activity present");
    let inquiry_item =
        report.items.iter().find(|i| i.kind == "inquiry").expect("inquiry activity present");

    assert!(order_item.label.starts_with("訂單 RPT-"), "got label={}", order_item.label);
    assert!(order_item.label.contains("已付款:NT$500"), "got label={}", order_item.label);
    assert_eq!(enrolment_item.label, "新報名:Activity Feed Course");
    assert_eq!(inquiry_item.label, "新洽詢(general):課程諮詢");

    // The four headline events' relative order must reflect their
    // `occurred_at` offsets (-10m newest .. -40m oldest).
    assert!(inquiry_item.occurred_at > enrolment_item.occurred_at, "inquiry should be newer than enrolment");
    assert!(enrolment_item.occurred_at > order_item.occurred_at, "enrolment should be newer than order");
    assert!(order_item.occurred_at > user_item.occurred_at, "order should be newer than the headline user");
}

#[sqlx::test]
async fn admin_activity_caps_at_20_across_sources(db: PgPool) {
    let now = Utc::now();
    for i in 0..25i64 {
        let user_id = seed_member(&db, &format!("activity-cap-{i}@example.com"), "Password!234").await;
        backdate_user(&db, user_id, now - Duration::minutes(i)).await;
    }

    let report = service::admin_activity(&db).await.expect("admin_activity");
    assert_eq!(report.items.len(), 20, "must cap at 20 even with 25 candidate rows");

    // The 20 most recent (offsets 0..19 minutes ago) must all be present;
    // the 5 oldest (20..24 minutes ago) must have been dropped.
    let cutoff = now - Duration::minutes(19);
    assert!(
        report.items.iter().all(|i| i.occurred_at >= cutoff),
        "oldest rows must have been dropped by the 20-cap, got {:?}",
        report.items.iter().map(|i| i.occurred_at).collect::<Vec<_>>()
    );
}
