//! Service-layer tests for `modules::cart::service`.
//!
//! Exercises domain invariants (quantity bounds, inactive product rejection,
//! duplicate add merge semantics, overflow-safe totals) directly against the
//! service API without going through the HTTP layer.

mod common;

use common::{seed_member, seed_product};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::cart::service;

#[sqlx::test]
async fn add_item_first_time_creates_cart_item(db: PgPool) {
    let user = seed_member(&db, "c1@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-1", 500, Some(10)).await;

    let cart = service::add_item(&db, user, product, 2).await.unwrap();
    assert_eq!(cart.items.len(), 1);
    assert_eq!(cart.items[0].quantity, 2);
    assert_eq!(cart.items[0].subtotal_cents, 1000);
    assert_eq!(cart.total_cents, 1000);
}

#[sqlx::test]
async fn add_item_duplicate_merges_quantity(db: PgPool) {
    let user = seed_member(&db, "c2@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-2", 500, Some(10)).await;

    service::add_item(&db, user, product, 2).await.unwrap();
    let cart = service::add_item(&db, user, product, 3).await.unwrap();

    // Second `add_item` should have merged into the same row (repository
    // uses ON CONFLICT DO UPDATE SET quantity = quantity + excluded.quantity).
    assert_eq!(cart.items.len(), 1);
    assert_eq!(cart.items[0].quantity, 5);
}

#[sqlx::test]
async fn add_item_rejects_zero_quantity(db: PgPool) {
    let user = seed_member(&db, "c3@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-3", 500, Some(10)).await;

    let err = service::add_item(&db, user, product, 0).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("quantity")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn add_item_rejects_quantity_above_stock(db: PgPool) {
    let user = seed_member(&db, "c4@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-4", 500, Some(3)).await;

    let err = service::add_item(&db, user, product, 10).await.unwrap_err();
    assert!(
        matches!(err, AppError::Conflict(ref m) if m.contains("stock")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn add_item_unknown_product_returns_not_found(db: PgPool) {
    let user = seed_member(&db, "c5@example.com", "Password!234").await;
    let err = service::add_item(&db, user, Uuid::now_v7(), 1).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn add_item_inactive_product_is_rejected(db: PgPool) {
    let user = seed_member(&db, "c6@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-6", 500, Some(10)).await;
    sqlx::query("UPDATE products SET is_active = false WHERE id = $1")
        .bind(product)
        .execute(&db)
        .await
        .unwrap();

    let err = service::add_item(&db, user, product, 1).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("not available")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn update_quantity_changes_and_get_cart_reflects(db: PgPool) {
    let user = seed_member(&db, "c7@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-7", 500, Some(20)).await;
    service::add_item(&db, user, product, 1).await.unwrap();

    let cart = service::update_quantity(&db, user, product, 7).await.unwrap();
    assert_eq!(cart.items[0].quantity, 7);
    assert_eq!(cart.total_cents, 3500);

    let fetched = service::get_cart(&db, user).await.unwrap();
    assert_eq!(fetched.items[0].quantity, 7);
}

#[sqlx::test]
async fn remove_item_then_get_cart_empty(db: PgPool) {
    let user = seed_member(&db, "c8@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-8", 500, Some(5)).await;
    service::add_item(&db, user, product, 1).await.unwrap();

    let cart = service::remove_item(&db, user, product).await.unwrap();
    assert!(cart.items.is_empty());
    assert_eq!(cart.total_cents, 0);
}

#[sqlx::test]
async fn remove_item_not_in_cart_returns_not_found(db: PgPool) {
    let user = seed_member(&db, "c9@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-9", 500, Some(5)).await;
    let err = service::remove_item(&db, user, product).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn total_across_multiple_items_sums_correctly(db: PgPool) {
    let user = seed_member(&db, "c10@example.com", "Password!234").await;
    let a = seed_product(&db, "prod-a", 500, Some(10)).await;
    let b = seed_product(&db, "prod-b", 1234, Some(10)).await;

    service::add_item(&db, user, a, 2).await.unwrap(); // 1000
    let cart = service::add_item(&db, user, b, 3).await.unwrap(); // + 3702 = 4702

    assert_eq!(cart.items.len(), 2);
    assert_eq!(cart.total_cents, 4702);
}
