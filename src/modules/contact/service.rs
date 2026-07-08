use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::pagination::PaginationParams;

use super::dto::{
    CreateInquiryRequest, InquiryListResponse, InquiryResponse, UpdateInquiryRequest,
};
use super::model::InquiryStatus;
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
        &req.inquiry_type,
        req.metadata,
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

/// `PATCH /contact/inquiries/{id}` — admin-only (checked by the handler),
/// Round 4 Task B5 admin follow-up. `status`, when present, must parse as
/// `InquiryStatus` (new/in_progress/resolved/closed) — mirrors
/// `courses::service::create_course`'s `level` parsing.
pub async fn update_inquiry(
    db: &PgPool,
    id: Uuid,
    req: &UpdateInquiryRequest,
) -> Result<InquiryResponse, AppError> {
    let status = req
        .status
        .as_deref()
        .map(|s| {
            s.parse::<InquiryStatus>().map(|v| v.as_str()).map_err(|_| {
                AppError::Validation("status 僅接受 new/in_progress/resolved/closed".into())
            })
        })
        .transpose()?;

    let inquiry = repository::update(db, id, status, req.assigned_to)
        .await?
        .ok_or_else(|| AppError::NotFound("inquiry not found".into()))?;

    Ok(InquiryResponse::from(inquiry))
}
