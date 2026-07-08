//! Integration tests for `contact::service`.
//!
//! Covered paths:
//! - `submit_inquiry` persists the row and populates default status
//! - `list_inquiries` paginates correctly and returns total count
//! - `list_inquiries` clamps per_page to the pagination cap

mod common;

use sqlx::PgPool;

use dream_fly_backend::extractors::pagination::PaginationParams;
use dream_fly_backend::modules::contact::dto::CreateInquiryRequest;
use dream_fly_backend::modules::contact::service;

fn req(subject: &str) -> CreateInquiryRequest {
    CreateInquiryRequest {
        name: "Test Person".into(),
        email: "test@example.com".into(),
        phone: Some("0912345678".into()),
        subject: subject.into(),
        message: "Please help me with this thing.".into(),
        inquiry_type: "general".into(),
        metadata: None,
    }
}

#[sqlx::test]
async fn submit_inquiry_persists_and_returns_new_row(db: PgPool) {
    let resp = service::submit_inquiry(&db, req("Question about classes"))
        .await
        .expect("submit_inquiry");

    assert_eq!(resp.name, "Test Person");
    assert_eq!(resp.email, "test@example.com");
    assert_eq!(resp.subject, "Question about classes");
    // Default status on a newly created inquiry — the column is non-null
    // with an enum default, so the response should carry it.
    assert!(!resp.status.is_empty(), "status must be populated");

    // Row really exists in DB.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contact_inquiries WHERE id = $1")
        .bind(resp.id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn list_inquiries_paginates_and_returns_total(db: PgPool) {
    for i in 0..5 {
        service::submit_inquiry(&db, req(&format!("Subject {i}")))
            .await
            .unwrap();
    }

    let page_1 = service::list_inquiries(
        &db,
        &PaginationParams {
            page: 1,
            per_page: 2,
        },
    )
    .await
    .expect("page 1");
    assert_eq!(page_1.inquiries.len(), 2);
    assert_eq!(page_1.meta.total, 5);
    assert_eq!(page_1.meta.page, 1);
    assert_eq!(page_1.meta.per_page, 2);

    let page_3 = service::list_inquiries(
        &db,
        &PaginationParams {
            page: 3,
            per_page: 2,
        },
    )
    .await
    .expect("page 3");
    assert_eq!(page_3.inquiries.len(), 1, "last page has 1 item");
}

#[sqlx::test]
async fn list_inquiries_clamps_per_page(db: PgPool) {
    // Even asking for 9999, the response must cap at 100 so admins can't
    // accidentally dump the whole table.
    let resp = service::list_inquiries(
        &db,
        &PaginationParams {
            page: 1,
            per_page: 9_999,
        },
    )
    .await
    .expect("list");
    assert_eq!(resp.meta.per_page, 100);
}
