//! Service-layer tests for `modules::cart::service`.
//!
//! Exercises domain invariants (quantity bounds, inactive item rejection,
//! duplicate add semantics, overflow-safe totals) directly against the
//! service API without going through the HTTP layer. Cart lines can target
//! either a product or a course (Task 3); product lines merge quantities on
//! repeat `add_item`, course lines are quantity-locked to 1 and reject a
//! repeat add with 409 instead of merging.

mod common;

use common::fixtures::{seed_course, seed_entitlement_product};
use common::{seed_member, seed_product};
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::modules::cart::model::LineTarget;
use dream_fly_backend::modules::cart::service;
use dream_fly_backend::modules::points::service as points_service;
use dream_fly_backend::modules::products::model::ProductType;

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

// Contract §3.8: repeat adds accumulate, and the *merged* quantity is what
// `Product::ensure_purchasable` judges — two adds that each clear the stock
// check on their own can no longer push the cart line past `stock`. A
// rejected add rolls back, so the line keeps its pre-add quantity. (This
// test replaces `add_item_repeated_can_accumulate_past_stock_by_design`.)
#[sqlx::test]
async fn add_item_merged_quantity_past_stock_is_409_and_cart_unchanged(db: PgPool) {
    let user = seed_member(&db, "c11@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-11", 500, Some(3)).await;

    let cart = service::add_item(&db, user, "product", product, 2).await.unwrap();
    assert_eq!(cart.items[0].quantity, 2);

    let err = service::add_item(&db, user, "product", product, 2).await.unwrap_err();
    assert!(
        matches!(err, AppError::Conflict(ref m) if m == "insufficient stock: only 3 available"),
        "got {err:?}"
    );

    let cart = service::get_cart(&db, user).await.unwrap();
    assert_eq!(cart.items.len(), 1);
    assert_eq!(cart.items[0].quantity, 2, "rejected add must not change the line");
}

// Two concurrent adds to the same line: the upsert's row lock (or, for the
// very first insert, the unique index) makes the second wait for the first
// to commit, so it judges the merged total — exactly one add fits in stock.
#[sqlx::test]
async fn concurrent_add_item_same_line_only_one_fits_in_stock(db: PgPool) {
    let user = seed_member(&db, "c16@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-16", 500, Some(3)).await;

    let (a, b) = tokio::join!(
        service::add_item(&db, user, "product", product, 2),
        service::add_item(&db, user, "product", product, 2),
    );

    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "got {results:?}");
    assert!(
        results
            .iter()
            .any(|r| matches!(r, Err(AppError::Conflict(m)) if m.contains("insufficient stock"))),
        "got {results:?}"
    );
    let cart = service::get_cart(&db, user).await.unwrap();
    assert_eq!(cart.items[0].quantity, 2);
}

#[sqlx::test]
async fn add_item_merged_quantity_past_999_is_400_and_cart_unchanged(db: PgPool) {
    let user = seed_member(&db, "c13@example.com", "Password!234").await;
    let product = seed_product(&db, "prod-13", 500, None).await;

    service::add_item(&db, user, "product", product, 999).await.unwrap();
    let err = service::add_item(&db, user, "product", product, 1).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m == "quantity must be between 1 and 999"),
        "got {err:?}"
    );

    let cart = service::get_cart(&db, user).await.unwrap();
    assert_eq!(cart.items[0].quantity, 999, "rejected add must not change the line");
}

#[sqlx::test]
async fn add_item_time_based_entitlement_twice_is_422_and_cart_unchanged(db: PgPool) {
    let user = seed_member(&db, "c14@example.com", "Password!234").await;
    let membership = seed_entitlement_product(
        &db,
        "prod-14-monthly",
        ProductType::Membership,
        5000,
        Some(30),
        None,
    )
    .await;

    service::add_item(&db, user, "product", membership, 1).await.unwrap();
    let err = service::add_item(&db, user, "product", membership, 1).await.unwrap_err();
    assert!(
        matches!(err, AppError::Validation(ref m) if m == "time-based subscription quantity must be 1"),
        "got {err:?}"
    );

    let cart = service::get_cart(&db, user).await.unwrap();
    assert_eq!(cart.items[0].quantity, 1, "rejected add must not change the line");
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
    match err {
        AppError::Validation(msg) => assert_eq!(msg, "invalid item_type: bogus"),
        other => panic!("expected Validation, got {other:?}"),
    }
}

/// Case policy: `item_type` is one of the fields the wire is case-sensitive
/// about — a legal value in the wrong case is rejected, not silently
/// accepted.
#[sqlx::test]
async fn add_item_mixed_case_item_type_returns_422(db: PgPool) {
    let user = seed_member(&db, "c6c@example.com", "Password!234").await;
    let err = service::add_item(&db, user, "PRODUCT", Uuid::now_v7(), 1)
        .await
        .unwrap_err();
    match err {
        AppError::Validation(msg) => assert_eq!(msg, "invalid item_type: PRODUCT"),
        other => panic!("expected Validation, got {other:?}"),
    }
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
// quantity (the same rule `add_item` applies to its merged quantity) — this
// had zero direct test coverage before this task.
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

// The final quantity goes through the same `Product::ensure_purchasable`
// as `add_item`'s merged quantity, so the time-based multiple rule applies
// here too.
#[sqlx::test]
async fn update_quantity_time_based_entitlement_above_one_is_422(db: PgPool) {
    let user = seed_member(&db, "c15@example.com", "Password!234").await;
    let membership = seed_entitlement_product(
        &db,
        "prod-15-monthly",
        ProductType::Membership,
        5000,
        Some(30),
        None,
    )
    .await;
    let cart = service::add_item(&db, user, "product", membership, 1).await.unwrap();
    let item_id = cart.items[0].id;

    let err = service::update_quantity(&db, user, item_id, 2).await.unwrap_err();
    assert!(
        matches!(err, AppError::Validation(ref m) if m == "time-based subscription quantity must be 1"),
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

// ---------------------------------------------------------------------------
// C3: `find_cart_items_for_checkout_tx` seam — `&BalanceLock` plumbing
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn checkout_cart_seam_reads_only_the_locked_users_cart(db: PgPool) {
    let user_a = seed_member(&db, "cart-lock-a@example.com", "Password!234").await;
    let user_b = seed_member(&db, "cart-lock-b@example.com", "Password!234").await;
    let product_a = seed_product(&db, "cart-lock-prod-a", 500, Some(10)).await;
    let product_b = seed_product(&db, "cart-lock-prod-b", 700, Some(10)).await;

    service::add_item(&db, user_a, "product", product_a, 1).await.unwrap();
    service::add_item(&db, user_b, "product", product_b, 1).await.unwrap();

    // Lock user_a's balance, then read "the" cart through the seam — it must
    // resolve to user_a's cart (via `lock.user_id()`), never user_b's, even
    // though both carts exist in the same database at the same time.
    let mut tx = db.begin().await.expect("begin tx");
    let lock = points_service::lock_balance_tx(&mut tx, user_a)
        .await
        .expect("lock user_a's balance");

    let lines = service::find_cart_items_for_checkout_tx(&mut tx, &lock)
        .await
        .expect("checkout read");
    tx.rollback().await.expect("rollback");

    assert_eq!(lines.len(), 1, "must read only the locked user's own cart line");
    assert_eq!(lines[0].target, LineTarget::Product(product_a));
}
