//! Integration tests for `venues::service`.
//!
//! Covered paths:
//! - `create_venue` auto-generates a slug from the name
//! - `create_venue` with explicit slug stored verbatim
//! - `create_venue` returns Conflict on a duplicate slug (unique index
//!   violation is translated, not leaked as 500)
//! - `get_by_slug` NotFound for a random slug
//! - `list_active` filters out deactivated venues

mod common;

use sqlx::PgPool;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::venues::dto::CreateVenueRequest;
use dream_fly_backend::modules::venues::service;

fn req(name: &str, slug: Option<&str>) -> CreateVenueRequest {
    CreateVenueRequest {
        name: name.into(),
        slug: slug.map(|s| s.to_string()),
        category_id: None,
        description: Some("A lovely venue".into()),
        features: vec!["mat".into(), "bar".into()],
        image_url: None,
    }
}

#[sqlx::test]
async fn create_venue_auto_generates_slug(db: PgPool) {
    let resp = service::create_venue(&db, &req("Main Gym", None))
        .await
        .expect("create");
    assert_eq!(resp.name, "Main Gym");
    assert_eq!(resp.slug, "main-gym");
    assert!(resp.is_active);
    assert_eq!(resp.features.len(), 2);
}

#[sqlx::test]
async fn create_venue_explicit_slug_is_preserved(db: PgPool) {
    let resp = service::create_venue(&db, &req("West Wing", Some("west-wing-a")))
        .await
        .expect("create");
    assert_eq!(resp.slug, "west-wing-a");
}

#[sqlx::test]
async fn create_venue_duplicate_slug_returns_conflict(db: PgPool) {
    service::create_venue(&db, &req("First", Some("duplicated-slug")))
        .await
        .unwrap();

    let err = service::create_venue(&db, &req("Second", Some("duplicated-slug")))
        .await
        .unwrap_err();

    match err {
        AppError::Conflict(msg) => assert!(msg.contains("duplicated-slug"), "msg: {msg}"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn get_by_slug_nonexistent_returns_not_found(db: PgPool) {
    let err = service::get_by_slug(&db, "never-seeded")
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn list_active_filters_out_inactive_venues(db: PgPool) {
    let keep = service::create_venue(&db, &req("Keep", None)).await.unwrap();
    let hide = service::create_venue(&db, &req("Hide", None)).await.unwrap();

    sqlx::query("UPDATE venues SET is_active = false WHERE id = $1")
        .bind(hide.id)
        .execute(&db)
        .await
        .unwrap();

    let venues = service::list_active(&db).await.expect("list");
    let ids: Vec<_> = venues.iter().map(|v| v.id).collect();
    assert!(ids.contains(&keep.id));
    assert!(!ids.contains(&hide.id));
}
