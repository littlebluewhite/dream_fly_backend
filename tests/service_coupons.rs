//! Integration tests for `coupons::service` (and `coupons::repository::find_valid_by_code_tx`,
//! the entry point Task 9's checkout flow depends on).
//!
//! Covered paths:
//! - `create_coupon` normalizes the code to uppercase and persists the row
//! - `create_coupon` surfaces a duplicate code as `AppError::Conflict`
//! - `validate_coupon` is case-insensitive and matches only active, unexpired coupons
//! - `validate_coupon` returns `NotFound` for expired / inactive / unknown codes
//! - `list_coupons` paginates correctly and returns total count
//! - `repository::find_valid_by_code_tx` applies the same normalization/filter rules

mod common;

use chrono::{Duration, Utc};
use sqlx::PgPool;

use common::fixtures::seed_coupon;
use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::coupons::dto::CreateCouponRequest;
use dream_fly_backend::modules::coupons::{repository, service};

fn req(code: &str, discount_cents: i64) -> CreateCouponRequest {
    CreateCouponRequest {
        code: code.into(),
        discount_cents,
        expires_at: None,
    }
}

#[sqlx::test]
async fn create_coupon_normalizes_code_and_persists(db: PgPool) {
    let resp = service::create_coupon(&db, req("  dreamfly100  ", 500))
        .await
        .expect("create_coupon");

    assert_eq!(resp.code, "DREAMFLY100");
    assert_eq!(resp.discount_cents, 500);
    assert!(resp.is_active);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM coupons WHERE code = $1")
        .bind("DREAMFLY100")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn create_coupon_duplicate_code_returns_conflict(db: PgPool) {
    service::create_coupon(&db, req("SUMMER50", 500))
        .await
        .expect("first create");

    let err = service::create_coupon(&db, req("summer50", 999))
        .await
        .expect_err("duplicate must fail");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "expected Conflict, got {err:?}"
    );
}

#[sqlx::test]
async fn validate_coupon_returns_active_unexpired(db: PgPool) {
    service::create_coupon(&db, req("DREAMFLY100", 1000))
        .await
        .expect("create");

    let resp = service::validate_coupon(&db, "DREAMFLY100")
        .await
        .expect("validate");
    assert_eq!(resp.code, "DREAMFLY100");
    assert_eq!(resp.discount_cents, 1000);
}

#[sqlx::test]
async fn validate_coupon_is_case_insensitive(db: PgPool) {
    service::create_coupon(&db, req("DREAMFLY100", 1000))
        .await
        .expect("create");

    let resp = service::validate_coupon(&db, "dreamfly100")
        .await
        .expect("validate lowercase");
    assert_eq!(resp.code, "DREAMFLY100");
    assert_eq!(resp.discount_cents, 1000);
}

#[sqlx::test]
async fn validate_coupon_expired_returns_not_found(db: PgPool) {
    seed_coupon(&db, "EXPIRED10", 100, true, Some(Utc::now() - Duration::days(1))).await;

    let err = service::validate_coupon(&db, "EXPIRED10")
        .await
        .expect_err("expired coupon must not validate");
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn validate_coupon_inactive_returns_not_found(db: PgPool) {
    seed_coupon(&db, "DISABLED10", 100, false, None).await;

    let err = service::validate_coupon(&db, "DISABLED10")
        .await
        .expect_err("inactive coupon must not validate");
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn validate_coupon_nonexistent_returns_not_found(db: PgPool) {
    let err = service::validate_coupon(&db, "NOSUCHCODE")
        .await
        .expect_err("unknown coupon must not validate");
    assert!(matches!(err, AppError::NotFound(_)));
}

#[sqlx::test]
async fn list_coupons_paginates_and_returns_total(db: PgPool) {
    for i in 0..5i64 {
        service::create_coupon(&db, req(&format!("CODE{i}"), 100 + i))
            .await
            .unwrap();
    }

    let page_1 = service::list_coupons(
        &db,
        &PaginationParams {
            page: 1,
            per_page: 2,
        },
    )
    .await
    .expect("page 1");
    assert_eq!(page_1.coupons.len(), 2);
    assert_eq!(page_1.total, 5);
    assert_eq!(page_1.page, 1);
    assert_eq!(page_1.per_page, 2);

    let page_3 = service::list_coupons(
        &db,
        &PaginationParams {
            page: 3,
            per_page: 2,
        },
    )
    .await
    .expect("page 3");
    assert_eq!(page_3.coupons.len(), 1, "last page has 1 item");
}

#[sqlx::test]
async fn list_coupons_clamps_per_page(db: PgPool) {
    let resp = service::list_coupons(
        &db,
        &PaginationParams {
            page: 1,
            per_page: 9_999,
        },
    )
    .await
    .expect("list");
    assert_eq!(resp.per_page, 100);
}

#[sqlx::test]
async fn find_valid_by_code_tx_normalizes_and_filters(db: PgPool) {
    seed_coupon(&db, "TXCODE1", 250, true, None).await;

    let mut tx = db.begin().await.expect("begin tx");
    let found = repository::find_valid_by_code_tx(&mut tx, "txcode1")
        .await
        .expect("query")
        .expect("coupon found");
    assert_eq!(found.code, "TXCODE1");
    assert_eq!(found.discount_cents, 250);
    tx.rollback().await.expect("rollback");
}

#[sqlx::test]
async fn find_valid_by_code_tx_excludes_expired(db: PgPool) {
    seed_coupon(&db, "TXEXPIRED", 250, true, Some(Utc::now() - Duration::days(1))).await;

    let mut tx = db.begin().await.expect("begin tx");
    let found = repository::find_valid_by_code_tx(&mut tx, "TXEXPIRED")
        .await
        .expect("query");
    assert!(found.is_none());
    tx.rollback().await.expect("rollback");
}
