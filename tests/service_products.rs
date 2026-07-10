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
//! - `reserve_stock_tx`: mixed finite+unlimited stock reserves correctly
//! - `reserve_stock_tx`: insufficient stock -> Conflict, rollback leaves stock untouched
//! - `reserve_stock_tx`: descending input still reports the smallest product_id first (lock order)
//! - `reserve_stock_tx`: empty lines is a no-op returning an empty map

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
        all_active.meta.total as usize,
        all_ids.len(),
        "total must match the number of rows returned when results fit on one page",
    );
    assert_eq!(all_active.meta.page, 1);
    assert_eq!(all_active.meta.per_page, 100);

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

#[sqlx::test]
async fn reserve_stock_tx_decrements_mixed_finite_and_unlimited_stock(db: PgPool) {
    let finite = common::seed_product(&db, "reserve-finite", 1000, Some(10)).await;
    let unlimited = common::seed_product(&db, "reserve-unlimited", 500, None).await;

    let mut tx = db.begin().await.expect("begin tx");
    let reserved = service::reserve_stock_tx(
        &mut tx,
        &[(finite, 3, "Finite Stock"), (unlimited, 5, "Unlimited Stock")],
    )
    .await
    .expect("sufficient stock should reserve");
    tx.commit().await.expect("commit");

    assert_eq!(reserved.len(), 2, "both lines should appear in the returned map");
    assert_eq!(
        reserved.get(&finite).expect("finite product in map").stock,
        Some(7),
        "finite stock must be decremented by the requested quantity"
    );
    assert_eq!(
        reserved.get(&unlimited).expect("unlimited product in map").stock,
        None,
        "NULL (unlimited) stock must remain untouched"
    );

    // Independently verify persisted state via the pool, outside the
    // committed transaction.
    assert_eq!(common::product_stock(&db, finite).await, Some(7));
    assert_eq!(common::product_stock(&db, unlimited).await, None);
}

#[sqlx::test]
async fn reserve_stock_tx_insufficient_returns_conflict_and_rolls_back(db: PgPool) {
    let product = common::seed_product(&db, "reserve-short", 1000, Some(1)).await;

    let mut tx = db.begin().await.expect("begin tx");
    let err = service::reserve_stock_tx(&mut tx, &[(product, 2, "Widget")])
        .await
        .expect_err("insufficient stock should fail");
    // Roll back explicitly so the connection is cleanly back in the pool
    // before the independent verification query below.
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => {
            assert_eq!(msg, "insufficient stock for product Widget")
        }
        other => panic!("expected Conflict, got {other:?}"),
    }

    assert_eq!(
        common::product_stock(&db, product).await,
        Some(1),
        "stock must be unchanged after rollback"
    );
}

#[sqlx::test]
async fn reserve_stock_tx_insufficient_multiple_reports_smallest_product_id_despite_descending_input(
    db: PgPool,
) {
    // Two products, both insufficient for the requested quantity. The lock
    // order inside `reserve_stock_tx` sorts by product_id ascending before
    // touching any row, so whichever product has the smaller id must be
    // the one reported as the Conflict — regardless of the order the
    // caller listed the lines in. UUIDv7 creation order is not guaranteed
    // to match value order within the same millisecond, so the smaller/
    // larger id is determined by direct comparison rather than assumed
    // from creation order.
    let product_a = common::seed_product(&db, "reserve-race-a", 1000, Some(1)).await;
    let product_b = common::seed_product(&db, "reserve-race-b", 1000, Some(1)).await;

    let (smaller_id, smaller_name, larger_id, larger_name) = if product_a < product_b {
        (product_a, "Product A", product_b, "Product B")
    } else {
        (product_b, "Product B", product_a, "Product A")
    };

    let mut tx = db.begin().await.expect("begin tx");
    // Deliberately descending (larger id first) — the sort inside
    // `reserve_stock_tx`, not this input order, must decide which line is
    // reached (and fails) first.
    let err = service::reserve_stock_tx(
        &mut tx,
        &[(larger_id, 5, larger_name), (smaller_id, 5, smaller_name)],
    )
    .await
    .expect_err("both lines are insufficient (stock=1, requesting 5)");
    tx.rollback().await.expect("rollback");

    match err {
        AppError::Conflict(msg) => assert_eq!(
            msg,
            format!("insufficient stock for product {smaller_name}"),
            "the smallest product_id must be reported first, not the input order"
        ),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[sqlx::test]
async fn reserve_stock_tx_empty_lines_returns_empty_map(db: PgPool) {
    let mut tx = db.begin().await.expect("begin tx");
    let reserved = service::reserve_stock_tx(&mut tx, &[])
        .await
        .expect("empty lines must be a no-op success");
    tx.commit().await.expect("commit");

    assert!(reserved.is_empty(), "empty input must return an empty map");
}
