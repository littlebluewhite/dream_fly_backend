//! Pins the wire value domain of every PG enum this round's B-3 candidate
//! touches: `X::ALL.map(|v| v.as_str())`, in order, must equal
//! `enum_range(NULL::x)`'s labels, in order. This is a cross-language anchor
//! — it fails the moment the Rust enum's `ALL` and the Postgres type drift
//! apart, in either set or order (same technique as
//! `revenue_predicate_matches_revenue_statuses_array`).
//!
//! Covers the 9 enums Phase 3 wires straight into the repository:
//! attendance_status, cart_item_type, inquiry_status, course_level,
//! leave_status, order_status, post_category, post_status, product_type.

mod common;

use sqlx::PgPool;

use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::cart::model::CartItemType;
use dream_fly_backend::modules::contact::model::InquiryStatus;
use dream_fly_backend::modules::courses::model::CourseLevel;
use dream_fly_backend::modules::leave::model::LeaveStatus;
use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::posts::model::{PostCategory, PostStatus};
use dream_fly_backend::modules::products::model::ProductType;

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
    let all: Vec<&str> = AttendanceStatus::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "attendance_status", &all).await;
}

#[sqlx::test]
async fn cart_item_type_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = CartItemType::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "cart_item_type", &all).await;
}

#[sqlx::test]
async fn inquiry_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = InquiryStatus::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "inquiry_status", &all).await;
}

#[sqlx::test]
async fn course_level_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = CourseLevel::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "course_level", &all).await;
}

#[sqlx::test]
async fn leave_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = LeaveStatus::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "leave_status", &all).await;
}

#[sqlx::test]
async fn order_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = OrderStatus::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "order_status", &all).await;
}

#[sqlx::test]
async fn post_category_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = PostCategory::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "post_category", &all).await;
}

#[sqlx::test]
async fn post_status_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = PostStatus::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "post_status", &all).await;
}

#[sqlx::test]
async fn product_type_all_matches_enum_range(db: PgPool) {
    let all: Vec<&str> = ProductType::ALL.map(|v| v.as_str()).to_vec();
    assert_all_matches_enum_range(&db, "product_type", &all).await;
}
