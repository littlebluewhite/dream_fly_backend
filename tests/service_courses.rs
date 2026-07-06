//! Integration tests for `courses::service`.
//!
//! Covered paths:
//! - `create_course` auto-generates a slug from the name when none given
//! - `create_course` returns Conflict on a duplicate slug
//! - `create_course` rejects an invalid level string with Validation
//! - `create_course` rejects `min_age > max_age` with Validation
//! - `get_course_by_slug_or_id` resolves both forms and returns same row
//! - `update_course` allows slug update to a unique value and blocks on clash
//! - `list_courses` filters out inactive rows

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::courses::dto::{CreateCourseRequest, UpdateCourseRequest};
use dream_fly_backend::modules::courses::service;

fn minimal_create(name: &str) -> CreateCourseRequest {
    CreateCourseRequest {
        name: name.into(),
        slug: None,
        level: "beginner".into(),
        description: None,
        duration_minutes: 60,
        price_cents: 50_000,
        max_students: 12,
        min_age: None,
        max_age: None,
        features: None,
        coach_id: None,
        category: None,
        schedule_text: None,
        is_highlighted: false,
        schedule_slots: None,
    }
}

#[sqlx::test]
async fn create_course_auto_generates_slug_from_name(db: PgPool) {
    let resp = service::create_course(&db, minimal_create("Intro To Bars"))
        .await
        .expect("create_course");
    assert_eq!(resp.course.name, "Intro To Bars");
    assert_eq!(resp.course.slug, "intro-to-bars");
    assert_eq!(resp.course.level, "beginner");
    assert!(resp.course.is_active);
}

#[sqlx::test]
async fn create_course_duplicate_slug_returns_conflict(db: PgPool) {
    service::create_course(
        &db,
        CreateCourseRequest {
            slug: Some("shared-slug".into()),
            ..minimal_create("First")
        },
    )
    .await
    .unwrap();

    let err = service::create_course(
        &db,
        CreateCourseRequest {
            slug: Some("shared-slug".into()),
            ..minimal_create("Second")
        },
    )
    .await
    .unwrap_err();

    match err {
        AppError::Conflict(msg) => assert!(msg.contains("slug"), "msg: {msg}"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn create_course_invalid_level_returns_validation(db: PgPool) {
    let err = service::create_course(
        &db,
        CreateCourseRequest {
            level: "expert".into(),
            ..minimal_create("Weird Level")
        },
    )
    .await
    .unwrap_err();

    match err {
        AppError::Validation(msg) => assert!(msg.contains("level"), "msg: {msg}"),
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[sqlx::test]
async fn create_course_min_age_greater_than_max_returns_validation(db: PgPool) {
    // Cross-field rule enforced in the service layer (validator macros
    // can't express it).
    let err = service::create_course(
        &db,
        CreateCourseRequest {
            min_age: Some(12),
            max_age: Some(6),
            ..minimal_create("Bad Age Range")
        },
    )
    .await
    .unwrap_err();

    match err {
        AppError::Validation(msg) => {
            assert!(msg.contains("min_age"), "msg: {msg}")
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[sqlx::test]
async fn get_by_slug_and_id_return_same_course(db: PgPool) {
    let created = service::create_course(&db, minimal_create("Lookup Me"))
        .await
        .unwrap();

    let by_slug = service::get_course_by_slug_or_id(&db, &created.course.slug)
        .await
        .expect("by slug");
    let by_id = service::get_course_by_slug_or_id(&db, &created.course.id.to_string())
        .await
        .expect("by id");

    assert_eq!(by_slug.course.id, by_id.course.id);
    assert_eq!(by_slug.course.slug, by_id.course.slug);
}

#[sqlx::test]
async fn get_course_nonexistent_returns_not_found(db: PgPool) {
    let err = service::get_course_by_slug_or_id(&db, "no-such-slug")
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));

    let err = service::get_course_by_slug_or_id(&db, &Uuid::now_v7().to_string())
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn update_course_to_existing_other_slug_returns_conflict(db: PgPool) {
    let first = service::create_course(
        &db,
        CreateCourseRequest {
            slug: Some("taken".into()),
            ..minimal_create("First")
        },
    )
    .await
    .unwrap();
    let second = service::create_course(
        &db,
        CreateCourseRequest {
            slug: Some("free".into()),
            ..minimal_create("Second")
        },
    )
    .await
    .unwrap();

    // Updating `second` to use `taken` must fail — but updating `first`
    // to keep its own `taken` slug must NOT be flagged as a conflict
    // (that was a subtle bug the original service code guards against).
    let err = service::update_course(
        &db,
        second.course.id,
        UpdateCourseRequest {
            name: None,
            slug: Some("taken".into()),
            level: None,
            description: None,
            duration_minutes: None,
            price_cents: None,
            max_students: None,
            min_age: None,
            max_age: None,
            features: None,
            coach_id: None,
            category: None,
            schedule_text: None,
            is_highlighted: None,
            schedule_slots: None,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));

    // No-op slug update on first must succeed.
    service::update_course(
        &db,
        first.course.id,
        UpdateCourseRequest {
            name: Some("First Renamed".into()),
            slug: Some("taken".into()),
            level: None,
            description: None,
            duration_minutes: None,
            price_cents: None,
            max_students: None,
            min_age: None,
            max_age: None,
            features: None,
            coach_id: None,
            category: None,
            schedule_text: None,
            is_highlighted: None,
            schedule_slots: None,
        },
    )
    .await
    .expect("same-slug self-update must not conflict");
}

#[sqlx::test]
async fn list_courses_filters_out_inactive(db: PgPool) {
    let keep = service::create_course(&db, minimal_create("Active"))
        .await
        .unwrap();
    let hide = service::create_course(&db, minimal_create("Inactive"))
        .await
        .unwrap();

    sqlx::query("UPDATE courses SET is_active = false WHERE id = $1")
        .bind(hide.course.id)
        .execute(&db)
        .await
        .unwrap();

    let list = service::list_courses(&db, &PaginationParams::default())
        .await
        .expect("list");
    let ids: Vec<_> = list.courses.iter().map(|c| c.id).collect();
    assert!(ids.contains(&keep.course.id));
    assert!(!ids.contains(&hide.course.id));
}
