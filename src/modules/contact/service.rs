use sqlx::PgPool;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;

use super::dto::{CreateInquiryRequest, InquiryListResponse, InquiryResponse};
use super::repository;

pub async fn submit_inquiry(
    db: &PgPool,
    req: CreateInquiryRequest,
) -> Result<InquiryResponse, AppError> {
    let inquiry = repository::create(
        db,
        &req.name,
        &req.email,
        req.phone.as_deref(),
        &req.subject,
        &req.message,
    )
    .await?;

    Ok(InquiryResponse::from(inquiry))
}

pub async fn list_inquiries(
    db: &PgPool,
    pagination: &PaginationParams,
) -> Result<InquiryListResponse, AppError> {
    let total = repository::count_all(db).await?;
    let inquiries =
        repository::find_all(db, pagination.limit(), pagination.offset()).await?;

    Ok(InquiryListResponse {
        inquiries: inquiries.into_iter().map(InquiryResponse::from).collect(),
        meta: pagination.meta(total),
    })
}
