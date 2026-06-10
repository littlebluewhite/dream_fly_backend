//! Integration tests for `products::service`.
//!
//! Covered paths:
//! - `create` auto-generates slug when omitted
//! - `create` rejects unknown product_type with Validation
//! - `create` returns Conflict on duplicate slug
//! - `get_by_slug` / `get_by_id` NotFound surface cleanly
//! - `list` filters inactive rows and applies product_type filter
//! - `update` NotFound when row doesn't exist
//! - `update` rejects unknown product_type

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::products::dto::{
    CreateProductRequest, UpdateProductRequest,
};
use dream_fly_backend::modules::products::service;

fn create(name: &str, slug: Option<&str>, product_type: &str) -> CreateProductRequest {
    CreateProductRequest {
        name: name.into(),
        slug: slug.map(|s| s.into()),
        product_type: product_type.into(),
        description: None,
        price_cents: 1_000,
        original_price_cents: None,
        features: vec![],
        is_highlighted: false,
        badge: None,
        stock: Some(10),
    }
}

#[sqlx::test]
async fn create_product_auto_generates_slug(db: PgPool) {
    let resp = service::create(&db, create("Jump Rope", None, "merchandise"))
        .await
        .expect("create");
    assert_eq!(resp.slug, "jump-rope");
    assert_eq!(resp.product_type, "merchandise");
    assert_eq!(resp.stock, Some(10));
}

#[sqlx::test]
async fn create_product_unknown_type_returns_validation(db: PgPool) {
    let err = service::create(&db, create("Mystery", None, "nft"))
        .await
        .unwrap_err();
    match err {
        AppError::Validation(msg) => assert!(msg.contains("nft"), "msg: {msg}"),
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[sqlx::test]
async fn create_product_duplicate_slug_returns_conflict(db: PgPool) {
    service::create(&db, create("First", Some("repeat"), "merchandise"))
        .await
        .unwrap();

    let err = service::create(&db, create("Second", Some("repeat"), "merchandise"))
        .await
        .unwrap_err();
    match err {
        AppError::Conflict(msg) => assert!(msg.contains("repeat"), "msg: {msg}"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn get_product_by_slug_or_id_not_found(db: PgPool) {
    assert!(matches!(
        service::get_by_slug(&db, "nope").await.unwrap_err(),
        AppError::NotFound(_)
    ));
    assert!(matches!(
        service::get_by_id(&db, Uuid::now_v7()).await.unwrap_err(),
        AppError::NotFound(_)
    ));
}

#[sqlx::test]
async fn list_products_filters_inactive_and_by_type(db: PgPool) {
    let tee = service::create(&db, create("T-Shirt", None, "merchandise"))
        .await
        .unwrap();
    let pass = service::create(&db, create("Day Pass", None, "ticket"))
        .await
        .unwrap();
    let hidden = service::create(&db, create("Old Sticker", None, "merchandise"))
        .await
        .unwrap();

    // Deactivate one merchandise row.
    sqlx::query("UPDATE products SET is_active = false WHERE id = $1")
        .bind(hidden.id)
        .execute(&db)
        .await
        .unwrap();

    // Use a big per_page so the test doesn't need to page through results.
    let all_active = service::list(&db, None, 1, 100).await.unwrap();
    let all_ids: Vec<_> = all_active.products.iter().map(|p| p.id).collect();
    assert!(all_ids.contains(&tee.id));
    assert!(all_ids.contains(&pass.id));
    assert!(
        !all_ids.contains(&hidden.id),
        "inactive product must not appear in list"
    );
    assert_eq!(
        all_active.total as usize,
        all_ids.len(),
        "total must match the number of rows returned when results fit on one page",
    );
    assert_eq!(all_active.page, 1);
    assert_eq!(all_active.per_page, 100);

    let only_tickets = service::list(&db, Some("ticket"), 1, 100).await.unwrap();
    assert!(only_tickets.products.iter().any(|p| p.id == pass.id));
    assert!(!only_tickets.products.iter().any(|p| p.id == tee.id));
}

#[sqlx::test]
async fn update_product_nonexistent_returns_not_found(db: PgPool) {
    let err = service::update(
        &db,
        Uuid::now_v7(),
        UpdateProductRequest {
            name: Some("Ghost".into()),
            slug: None,
            product_type: None,
            description: None,
            price_cents: None,
            original_price_cents: None,
            features: None,
            is_highlighted: None,
            badge: None,
            stock: None,
            is_active: None,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn update_product_invalid_type_returns_validation(db: PgPool) {
    let created = service::create(&db, create("Real", None, "merchandise"))
        .await
        .unwrap();

    let err = service::update(
        &db,
        created.id,
        UpdateProductRequest {
            name: None,
            slug: None,
            product_type: Some("invalid-type".into()),
            description: None,
            price_cents: None,
            original_price_cents: None,
            features: None,
            is_highlighted: None,
            badge: None,
            stock: None,
            is_active: None,
        },
    )
    .await
    .unwrap_err();

    match err {
        AppError::Validation(msg) => assert!(msg.contains("invalid-type"), "msg: {msg}"),
        other => panic!("expected Validation, got {other:?}"),
    }
}
