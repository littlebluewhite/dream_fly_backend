//! Service-layer tests for `modules::cart::service`.
//!
//! Exercises domain invariants (quantity bounds, inactive item rejection,
//! duplicate add semantics, overflow-safe totals) directly against the
//! service API without going through the HTTP layer. Cart lines can target
//! either a product or a course (Task 3); product lines merge quantities on
//! repeat `add_item`, course lines are quantity-locked to 1 and reject a
//! repeat add with 409 instead of merging.

mod common;

use common::fixtures::seed_course;
use common::{seed_member, seed_product};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::cart::service;

#[sqlx::test]
async fn add_item_first_time_creates_cart_item(db: PgPool) {
    let user = seed_member(&db, "c1@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-1", 500, Some(10)).await;

    let cart = service::add_item(&db, user, "product", product, 2).await.unwrap();
    assert_eq!(cart.items.len(), 1);
    assert_eq!(cart.items[0].item_type, "product");
    assert_eq!(cart.items[0].item_id, product);
    assert_eq!(cart.items[0].quantity, 2);
    assert_eq!(cart.items[0].subtotal_cents, 1000);
    assert_eq!(cart.total_cents, 1000);
}

#[sqlx::test]
async fn add_item_duplicate_merges_quantity(db: PgPool) {
    let user = seed_member(&db, "c2@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-2", 500, Some(10)).await;

    service::add_item(&db, user, "product", product, 2).await.unwrap();
    let cart = service::add_item(&db, user, "product", product, 3).await.unwrap();

    // Second `add_item` should have merged into the same row (repository
    // uses ON CONFLICT DO UPDATE SET quantity = quantity + excluded.quantity).
    assert_eq!(cart.items.len(), 1);
    assert_eq!(cart.items[0].quantity, 5);
}

#[sqlx::test]
async fn add_item_rejects_zero_quantity(db: PgPool) {
    let user = seed_member(&db, "c3@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-3", 500, Some(10)).await;

    let err = service::add_item(&db, user, "product", product, 0).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("quantity")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn add_item_rejects_quantity_above_stock(db: PgPool) {
    let user = seed_member(&db, "c4@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-4", 500, Some(3)).await;

    let err = service::add_item(&db, user, "product", product, 10).await.unwrap_err();
    assert!(
        matches!(err, AppError::Conflict(ref m) if m.contains("stock")),
        "got {err:?}"
    );
}

// Characterization test (Task 7): locks in the *intentional* gap between
// `add_item`'s per-request increment check and the cart's accumulated
// total. `add_item` validates only the increment being added on each call
// (the repository accumulates via `ON CONFLICT DO UPDATE SET quantity =
// cart_items.quantity + $3`), so two additions that each individually clear
// the stock check can still push the cart's total past `stock`. This is not
// a bug to fix here — the authoritative decrement is
// `products::service::reserve_stock_tx` at checkout — this test exists so
// the gap is executable documentation instead of a comment someone could
// silently invalidate.
#[sqlx::test]
async fn add_item_repeated_can_accumulate_past_stock_by_design(db: PgPool) {
    let user = seed_member(&db, "c11@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-11", 500, Some(3)).await;

    let cart = service::add_item(&db, user, "product", product, 2).await.unwrap();
    assert_eq!(cart.items[0].quantity, 2);

    let cart = service::add_item(&db, user, "product", product, 2).await.unwrap();
    assert_eq!(cart.items.len(), 1);
    assert_eq!(cart.items[0].quantity, 4, "cart total exceeds stock of 3 by design");
}

#[sqlx::test]
async fn add_item_unknown_product_returns_not_found(db: PgPool) {
    let user = seed_member(&db, "c5@example.com", "Password!234").await;
    let err = service::add_item(&db, user, "product", Uuid::now_v7(), 1).await.unwrap_err();
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

    let err = service::add_item(&db, user, "product", product, 1).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("not available")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn add_item_invalid_item_type_is_rejected(db: PgPool) {
    let user = seed_member(&db, "c6b@example.com", "Password!234").await;
    let err = service::add_item(&db, user, "bogus", Uuid::now_v7(), 1).await.unwrap_err();
    assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
}

#[sqlx::test]
async fn update_quantity_changes_and_get_cart_reflects(db: PgPool) {
    let user = seed_member(&db, "c7@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-7", 500, Some(20)).await;
    let cart = service::add_item(&db, user, "product", product, 1).await.unwrap();
    let item_id = cart.items[0].id;

    let cart = service::update_quantity(&db, user, item_id, 7).await.unwrap();
    assert_eq!(cart.items[0].quantity, 7);
    assert_eq!(cart.total_cents, 3500);

    let fetched = service::get_cart(&db, user).await.unwrap();
    assert_eq!(fetched.items[0].quantity, 7);
}

// Task 7: the update-quantity call site validates the item's *final*
// quantity (unlike `add_item`'s increment check above) — this had zero
// direct test coverage before this task.
#[sqlx::test]
async fn update_quantity_above_stock_returns_conflict(db: PgPool) {
    let user = seed_member(&db, "c12@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-12", 500, Some(5)).await;
    let cart = service::add_item(&db, user, "product", product, 1).await.unwrap();
    let item_id = cart.items[0].id;

    let err = service::update_quantity(&db, user, item_id, 6).await.unwrap_err();
    assert!(
        matches!(err, AppError::Conflict(ref m) if m.contains("insufficient stock")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn remove_item_then_get_cart_empty(db: PgPool) {
    let user = seed_member(&db, "c8@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-8", 500, Some(5)).await;
    let cart = service::add_item(&db, user, "product", product, 1).await.unwrap();
    let item_id = cart.items[0].id;

    let cart = service::remove_item(&db, user, item_id).await.unwrap();
    assert!(cart.items.is_empty());
    assert_eq!(cart.total_cents, 0);
}

#[sqlx::test]
async fn remove_item_not_in_cart_returns_not_found(db: PgPool) {
    let user = seed_member(&db, "c9@example.com", "Password!234").await;
    let err = service::remove_item(&db, user, Uuid::now_v7()).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn total_across_multiple_items_sums_correctly(db: PgPool) {
    let user = seed_member(&db, "c10@example.com", "Password!234").await;
    let a = seed_product(&db, "prod-a", 500, Some(10)).await;
    let b = seed_product(&db, "prod-b", 1234, Some(10)).await;

    service::add_item(&db, user, "product", a, 2).await.unwrap(); // 1000
    let cart = service::add_item(&db, user, "product", b, 3).await.unwrap(); // + 3702 = 4702

    assert_eq!(cart.items.len(), 2);
    assert_eq!(cart.total_cents, 4702);
}

// ---------------------------------------------------------------------------
// Course lines (Task 3)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn add_course_item_creates_cart_item_with_quantity_one(db: PgPool) {
    let user = seed_member(&db, "cc1@example.com", "Password!234").await;
    let course = seed_course(&db, "Tumbling Basics", None).await;

    let cart = service::add_item(&db, user, "course", course, 1).await.unwrap();
    assert_eq!(cart.items.len(), 1);
    assert_eq!(cart.items[0].item_type, "course");
    assert_eq!(cart.items[0].item_id, course);
    assert_eq!(cart.items[0].quantity, 1);
    // seed_course hardcodes price_cents = 50000.
    assert_eq!(cart.items[0].unit_price_cents, 50000);
    assert_eq!(cart.items[0].subtotal_cents, 50000);
}

#[sqlx::test]
async fn add_course_item_rejects_quantity_other_than_one(db: PgPool) {
    let user = seed_member(&db, "cc2@example.com", "Password!234").await;
    let course = seed_course(&db, "Tumbling Basics", None).await;

    let err = service::add_item(&db, user, "course", course, 2).await.unwrap_err();
    assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
}

#[sqlx::test]
async fn add_course_item_duplicate_returns_conflict(db: PgPool) {
    let user = seed_member(&db, "cc3@example.com", "Password!234").await;
    let course = seed_course(&db, "Tumbling Basics", None).await;

    service::add_item(&db, user, "course", course, 1).await.unwrap();
    let err = service::add_item(&db, user, "course", course, 1).await.unwrap_err();
    assert!(
        matches!(err, AppError::Conflict(ref m) if m.contains("already in cart")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn add_course_item_unknown_course_returns_not_found(db: PgPool) {
    let user = seed_member(&db, "cc4@example.com", "Password!234").await;
    let err = service::add_item(&db, user, "course", Uuid::now_v7(), 1).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn add_course_item_inactive_course_is_rejected(db: PgPool) {
    let user = seed_member(&db, "cc5@example.com", "Password!234").await;
    let course = seed_course(&db, "Tumbling Basics", None).await;
    sqlx::query("UPDATE courses SET is_active = false WHERE id = $1")
        .bind(course)
        .execute(&db)
        .await
        .unwrap();

    let err = service::add_item(&db, user, "course", course, 1).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("not available")),
        "got {err:?}"
    );
}

#[sqlx::test]
async fn update_quantity_on_course_line_rejects_non_one(db: PgPool) {
    let user = seed_member(&db, "cc6@example.com", "Password!234").await;
    let course = seed_course(&db, "Tumbling Basics", None).await;
    let cart = service::add_item(&db, user, "course", course, 1).await.unwrap();
    let item_id = cart.items[0].id;

    let err = service::update_quantity(&db, user, item_id, 2).await.unwrap_err();
    assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
}
