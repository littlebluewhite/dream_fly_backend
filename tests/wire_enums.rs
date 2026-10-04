//! Pins the wire value domain of every PG enum this round's B-3 candidate
//! touches: `X::ALL.iter().map(|v| v.as_str())`, in order, must equal
//! `enum_range(NULL::x)`'s labels, in order. This is a cross-language anchor
//! — it fails the moment the Rust enum's `ALL` and the Postgres type drift
//! apart, in either set or order (same technique as
//! `revenue_predicate_matches_revenue_statuses_array`).
//!
//! Covers the 9 enums Phase 3 wires straight into the repository:
//! attendance_status, cart_item_type, inquiry_status, course_level,
//! leave_status, order_status, post_category, post_status, product_type.
//! W-1 adds booking_status, enrolment_status, notification_type, point_reason,
//! subscription_status, waitlist_status (their serde spelling is pinned in
//! `wire_serde.rs`).
//! Plus `Role`, whose value domain is the `roles` table rather than a PG enum
//! (`role_all_matches_roles_table`, ADR-0014).

mod common;

use sqlx::PgPool;

use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::modules::cart::model::CartItemType;
use dream_fly_backend::modules::contact::model::InquiryStatus;
use dream_fly_backend::modules::courses::model::CourseLevel;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::leave::model::LeaveStatus;
use dream_fly_backend::modules::notifications::model::NotificationType;
use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::permissions::model::Role;
use dream_fly_backend::modules::points::model::PointReason;
use dream_fly_backend::modules::posts::model::{PostCategory, PostStatus};
use dream_fly_backend::modules::products::model::ProductType;
use dream_fly_backend::modules::subscriptions::model::SubscriptionStatus;
use dream_fly_backend::modules::waitlist::model::WaitlistStatus;

/// Fetch every label of a PG enum type (`SELECT unnest(enum_range(NULL::x))::text`)
/// and assert it equals `all_labels`, in order.
async fn assert_all_matches_enum_range(db: &PgPool, pg_type: &str, all_labels: &[&str]) {
    let labels: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT unnest(enum_range(NULL::{pg_type}))::text"
    )))
    .fetch_all(db)
    .await
    .unwrap_or_else(|e| panic!("enum_range(NULL::{pg_type}) failed: {e}"));

    assert_eq!(
        labels, all_labels,
        "{pg_type}: PG enum_range labels must equal ALL's as_str() labels, in order"
    );
}

#[sqlx::test]
async fn attendance_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = AttendanceStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "attendance_status", &all).await;
}

#[sqlx::test]
async fn cart_item_type_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = CartItemType::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "cart_item_type", &all).await;
}

#[sqlx::test]
async fn inquiry_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = InquiryStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "inquiry_status", &all).await;
}

#[sqlx::test]
async fn course_level_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = CourseLevel::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "course_level", &all).await;
}

#[sqlx::test]
async fn leave_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = LeaveStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "leave_status", &all).await;
}

#[sqlx::test]
async fn order_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = OrderStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "order_status", &all).await;
}

#[sqlx::test]
async fn post_category_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = PostCategory::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "post_category", &all).await;
}

#[sqlx::test]
async fn post_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = PostStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "post_status", &all).await;
}

#[sqlx::test]
async fn product_type_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = ProductType::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "product_type", &all).await;
}

#[sqlx::test]
async fn booking_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = BookingStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "booking_status", &all).await;
}

#[sqlx::test]
async fn enrolment_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = EnrolmentStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "enrolment_status", &all).await;
}

#[sqlx::test]
async fn notification_type_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = NotificationType::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "notification_type", &all).await;
}

#[sqlx::test]
async fn point_reason_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = PointReason::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "point_reason", &all).await;
}

#[sqlx::test]
async fn subscription_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = SubscriptionStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "subscription_status", &all).await;
}

#[sqlx::test]
async fn waitlist_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = WaitlistStatus::ALL.iter().map(|v| v.as_str()).collect();
    assert_all_matches_enum_range(&db, "waitlist_status", &all).await;
}

/// `roles` is a table, not a PG enum, so there is no `enum_range` to compare
/// against: `Role::ALL` must name exactly the rows the init migration seeds
/// (as a set — the table has no inherent order).
#[sqlx::test]
async fn role_all_matches_roles_table(db: PgPool) {
    let mut table: Vec<String> = sqlx::query_scalar("SELECT name FROM roles")
        .fetch_all(&db)
        .await
        .expect("select role names");
    table.sort();

    let mut all: Vec<&str> = Role::ALL.iter().map(|r| r.as_str()).collect();
    all.sort();

    assert_eq!(table, all, "roles table rows must equal Role::ALL's as_str() labels");
}
