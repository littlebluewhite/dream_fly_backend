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

use common::fixtures::seed_order_with_item;
use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::products::dto::{
    CreateProductRequest, UpdateProductRequest,
};
use dream_fly_backend::modules::products::service;

fn create(name: &str, slug: Option<&str>, product_type: &str) -> CreateProductRequest {
    create_with_stock(name, slug, product_type, Some(10))
}

fn create_with_stock(
    name: &str,
    slug: Option<&str>,
    product_type: &str,
    stock: Option<i32>,
) -> CreateProductRequest {
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
        stock,
        valid_days: None,
        session_count: None,
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
            valid_days: None,
            session_count: None,
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
            valid_days: None,
            session_count: None,
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

#[sqlx::test]
async fn quota_mirrors_stock_including_null(db: PgPool) {
    // quota is a direct mapping of products.stock — null (unlimited) must
    // pass through as null, not as some other sentinel.
    let unlimited = service::create(&db, create_with_stock("Unlimited Ticket", None, "ticket", None))
        .await
        .expect("create unlimited");
    assert_eq!(unlimited.stock, None);
    assert_eq!(unlimited.quota, None);

    let limited = service::create(
        &db,
        create_with_stock("Limited Shirt", None, "merchandise", Some(5)),
    )
    .await
    .expect("create limited");
    assert_eq!(limited.stock, Some(5));
    assert_eq!(limited.quota, Some(5));
}

#[sqlx::test]
async fn sold_is_zero_with_no_orders(db: PgPool) {
    let created = service::create(&db, create("Fresh Product", None, "merchandise"))
        .await
        .expect("create");
    assert_eq!(created.sold, 0);

    // Also zero via the read paths, not just the create response.
    let by_id = service::get_by_id(&db, created.id).await.expect("get_by_id");
    assert_eq!(by_id.sold, 0);
}

#[sqlx::test]
async fn sold_counts_only_paid_class_orders(db: PgPool) {
    // sold = SUM(order_items.quantity) across paid/processing/completed
    // orders only — pending/cancelled/refunded must not contribute.
    let user = common::seed_member(&db, "sold-buyer@example.com", "passw0rd!").await;
    let product = service::create(&db, create("Counted Product", None, "merchandise"))
        .await
        .expect("create");

    seed_order_with_item(&db, user, product.id, &product.name, 2, 1000, "paid").await;
    seed_order_with_item(&db, user, product.id, &product.name, 1, 1000, "processing").await;
    seed_order_with_item(&db, user, product.id, &product.name, 3, 1000, "completed").await;
    // These must NOT count:
    seed_order_with_item(&db, user, product.id, &product.name, 5, 1000, "pending").await;
    seed_order_with_item(&db, user, product.id, &product.name, 7, 1000, "cancelled").await;
    seed_order_with_item(&db, user, product.id, &product.name, 9, 1000, "refunded").await;

    let resp = service::get_by_id(&db, product.id).await.expect("get_by_id");
    assert_eq!(resp.sold, 6, "only paid+processing+completed (2+1+3) should count");
}

#[sqlx::test]
async fn list_aggregates_sold_across_products_in_one_batch(db: PgPool) {
    // The list endpoint must compute `sold` for every product on the page
    // via one batched aggregate query, not one query per row. This test
    // only asserts on the resulting values (query-count is verified by
    // code review of `repository::find_sold_counts`'s call sites), but it
    // does prove the aggregate is correctly keyed per-product across a
    // multi-row page — a naive un-keyed aggregate would leak counts
    // between products.
    let user = common::seed_member(&db, "list-sold-buyer@example.com", "passw0rd!").await;
    let a = service::create(&db, create("List Sold A", None, "merchandise"))
        .await
        .expect("create a");
    let b = service::create(&db, create("List Sold B", None, "merchandise"))
        .await
        .expect("create b");

    seed_order_with_item(&db, user, a.id, &a.name, 4, 1000, "paid").await;
    seed_order_with_item(&db, user, b.id, &b.name, 9, 1000, "completed").await;

    let list = service::list(&db, None, 1, 100).await.expect("list");
    let a_resp = list.products.iter().find(|p| p.id == a.id).expect("a in list");
    let b_resp = list.products.iter().find(|p| p.id == b.id).expect("b in list");
    assert_eq!(a_resp.sold, 4);
    assert_eq!(b_resp.sold, 9);
}
