//! Integration tests for `users::service`.
//!
//! Covered paths:
//! - `get_me` returns the seeded profile
//! - `get_user` returns NotFound for random UUIDs
//! - `update_me` partial patch preserves untouched fields
//! - `update_me` accepts a valid https avatar URL (the stricter
//!   scheme-rejection path is covered by the URL validator unit tests)
//! - `list_users` paginates correctly (offset/limit) and returns total

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::error::AppError;
use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::users::dto::UpdateProfileRequest;
use dream_fly_backend::modules::users::service;

#[sqlx::test]
async fn get_me_returns_seeded_profile(db: PgPool) {
    let user_id = common::seed_member(&db, "me@example.com", "hunter22-secret").await;

    let resp = service::get_me(&db, user_id).await.expect("get_me");

    assert_eq!(resp.id, user_id);
    assert_eq!(resp.email, "me@example.com");
    assert_eq!(resp.name, "Test Member");
    assert!(resp.is_active);
    assert!(!resp.phone_verified);
}

/// Task 18: `UserResponse` gained `points_balance` (frontend admin members page
/// needs it). `seed_member`'s raw INSERT omits the column (relies on the
/// commerce migration's `DEFAULT 0`), so this bumps it to a known non-zero
/// value first — proving the field is actually read off the row, not just
/// defaulting to 0 by coincidence.
#[sqlx::test]
async fn get_me_includes_points_balance(db: PgPool) {
    let user_id = common::seed_member(&db, "points@example.com", "hunter22-secret").await;
    sqlx::query("UPDATE users SET points_balance = $2 WHERE id = $1")
        .bind(user_id)
        .bind(750_i64)
        .execute(&db)
        .await
        .expect("bump points_balance");

    let resp = service::get_me(&db, user_id).await.expect("get_me");

    assert_eq!(resp.points_balance, 750);
}

#[sqlx::test]
async fn get_user_by_nonexistent_id_returns_not_found(db: PgPool) {
    let err = service::get_user(&db, Uuid::now_v7()).await.unwrap_err();
    match err {
        AppError::NotFound(msg) => assert!(msg.contains("user"), "msg: {msg}"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[sqlx::test]
async fn update_me_partial_patch_preserves_other_fields(db: PgPool) {
    let user_id = common::seed_member(&db, "patch@example.com", "hunter22-secret").await;

    // Only update `name` — `phone` and `avatar_url` should remain NULL.
    let resp = service::update_me(
        &db,
        user_id,
        UpdateProfileRequest {
            name: Some("Renamed User".into()),
            phone: None,
            avatar_url: None,
        },
    )
    .await
    .expect("update_me");

    assert_eq!(resp.name, "Renamed User");
    assert_eq!(resp.email, "patch@example.com");
    assert!(resp.phone.is_none());
    assert!(resp.avatar_url.is_none());

    // Follow-up patch: set phone only, name should stick at "Renamed User".
    let resp2 = service::update_me(
        &db,
        user_id,
        UpdateProfileRequest {
            name: None,
            phone: Some("0912345678".into()),
            avatar_url: None,
        },
    )
    .await
    .expect("second update_me");

    assert_eq!(resp2.name, "Renamed User");
    assert_eq!(resp2.phone.as_deref(), Some("0912345678"));
}

#[sqlx::test]
async fn update_me_can_set_avatar_to_https_url(db: PgPool) {
    // The URL passes `validate_stored_url` (tested at the utility level)
    // — service should happily persist it.
    let user_id = common::seed_member(&db, "avatar@example.com", "hunter22-secret").await;

    let resp = service::update_me(
        &db,
        user_id,
        UpdateProfileRequest {
            name: None,
            phone: None,
            avatar_url: Some("https://cdn.example.com/a.png".into()),
        },
    )
    .await
    .expect("update_me");

    assert_eq!(
        resp.avatar_url.as_deref(),
        Some("https://cdn.example.com/a.png")
    );
}

#[sqlx::test]
async fn list_users_pagination_returns_expected_slice(db: PgPool) {
    // Seed 5 members; request page 2 with per_page=2 → should see the
    // 3rd and 4th users (offset=2, limit=2).
    for i in 0..5 {
        common::seed_member(&db, &format!("u{i}@example.com"), "hunter22-secret").await;
    }

    let page_1 = service::list_users(
        &db,
        &PaginationParams {
            page: 1,
            per_page: 2,
        },
    )
    .await
    .expect("page 1");
    assert_eq!(page_1.users.len(), 2);
    assert_eq!(page_1.total, 5);
    assert_eq!(page_1.page, 1);
    assert_eq!(page_1.per_page, 2);

    let page_2 = service::list_users(
        &db,
        &PaginationParams {
            page: 2,
            per_page: 2,
        },
    )
    .await
    .expect("page 2");
    assert_eq!(page_2.users.len(), 2);
    // Different slice than page 1.
    let page_1_ids: Vec<_> = page_1.users.iter().map(|u| u.id).collect();
    for u in &page_2.users {
        assert!(!page_1_ids.contains(&u.id), "page 2 overlaps with page 1");
    }

    // Last page has only 1 user.
    let page_3 = service::list_users(
        &db,
        &PaginationParams {
            page: 3,
            per_page: 2,
        },
    )
    .await
    .expect("page 3");
    assert_eq!(page_3.users.len(), 1);
    assert_eq!(page_3.total, 5);
}

#[sqlx::test]
async fn list_users_clamps_per_page_to_100(db: PgPool) {
    // Requesting per_page=500 must be clamped to 100 in the response so
    // no caller can bypass the pagination limit and dump the entire table.
    let resp = service::list_users(
        &db,
        &PaginationParams {
            page: 1,
            per_page: 500,
        },
    )
    .await
    .expect("list");

    assert_eq!(resp.per_page, 100, "per_page should clamp to 100");
}
