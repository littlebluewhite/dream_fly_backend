use axum::{
    Json,
    extract::{Query, State},
};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::extractors::pagination::PaginationParams;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{CreateInquiryRequest, InquiryListResponse, InquiryResponse};
use super::service;

/// Submit a contact inquiry (public, no auth required)
#[tracing::instrument(skip_all)]
pub async fn submit(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<CreateInquiryRequest>,
) -> Result<Json<InquiryResponse>, AppError> {
    let inquiry = service::submit_inquiry(&state.db, req).await?;
    Ok(Json(inquiry))
}

/// List all contact inquiries (admin only)
#[tracing::instrument(skip_all)]
pub async fn list(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<PaginationParams>,
) -> Result<Json<InquiryListResponse>, AppError> {
    auth.require_role("admin")?;
    let result = service::list_inquiries(&state.db, &params).await?;
    Ok(Json(result))
}
