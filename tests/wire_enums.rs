//! Pins the wire value domain of every PG enum this round's B-3 candidate
//! touches: each label from `enum_range(NULL::x)` must parse via the Rust
//! `FromStr` impl, and `as_str()` on the parsed value must round-trip back to
//! exactly that label. This is a cross-language anchor — it fails the moment
//! the Rust enum and the Postgres type drift apart, in either direction.
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
/// and assert `parse` maps each one to a value whose `as_str()` returns that
/// same label, verbatim.
async fn assert_wire_labels(
    db: &PgPool,
    pg_type: &str,
    parse: impl Fn(&str) -> Option<&'static str>,
) {
    let labels: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT unnest(enum_range(NULL::{pg_type}))::text"
    )))
    .fetch_all(db)
    .await
    .unwrap_or_else(|e| panic!("enum_range(NULL::{pg_type}) failed: {e}"));

    assert!(!labels.is_empty(), "{pg_type} must have at least one label");

    for label in labels {
        let round_tripped =
            parse(&label).unwrap_or_else(|| panic!("{pg_type} label {label:?} failed to parse"));
        assert_eq!(
            round_tripped, label,
            "{pg_type} label {label:?} must round-trip through as_str()"
        );
    }
}

#[sqlx::test]
async fn attendance_status_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "attendance_status", |s| {
        s.parse::<AttendanceStatus>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn cart_item_type_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "cart_item_type", |s| {
        s.parse::<CartItemType>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn inquiry_status_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "inquiry_status", |s| {
        s.parse::<InquiryStatus>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn course_level_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "course_level", |s| {
        s.parse::<CourseLevel>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn leave_status_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "leave_status", |s| {
        s.parse::<LeaveStatus>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn order_status_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "order_status", |s| {
        s.parse::<OrderStatus>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn post_category_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "post_category", |s| {
        s.parse::<PostCategory>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn post_status_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "post_status", |s| {
        s.parse::<PostStatus>().ok().map(|v| v.as_str())
    })
    .await;
}

#[sqlx::test]
async fn product_type_labels_round_trip(db: PgPool) {
    assert_wire_labels(&db, "product_type", |s| {
        s.parse::<ProductType>().ok().map(|v| v.as_str())
    })
    .await;
}
