use axum::{Json, extract::State};

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::state::AppState;
use crate::utils::validation::ValidatedJson;

use super::dto::{
    CertificateResponse, CreateCertificateRequest, CreateReportCardRequest, ReportCardResponse,
};
use super::service;

/// `POST /report-cards` — coach (own courses only) or admin.
#[tracing::instrument(skip_all)]
pub async fn create_report_card(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateReportCardRequest>,
) -> Result<Json<ReportCardResponse>, AppError> {
    let created = service::create_report_card(&state.db, &auth, req).await?;
    Ok(Json(created))
}

/// `GET /report-cards/me` — the caller's own report cards.
#[tracing::instrument(skip_all)]
pub async fn my_report_cards(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<ReportCardResponse>>, AppError> {
    let cards = service::list_my_report_cards(&state.db, auth.user_id).await?;
    Ok(Json(cards))
}

/// `POST /certificates` — coach (own students only) or admin.
#[tracing::instrument(skip_all)]
pub async fn create_certificate(
    State(state): State<AppState>,
    auth: AuthUser,
    ValidatedJson(req): ValidatedJson<CreateCertificateRequest>,
) -> Result<Json<CertificateResponse>, AppError> {
    let created = service::create_certificate(&state.db, &auth, req).await?;
    Ok(Json(created))
}

/// `GET /certificates/me` — the caller's own certificates.
#[tracing::instrument(skip_all)]
pub async fn my_certificates(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<CertificateResponse>>, AppError> {
    let certs = service::list_my_certificates(&state.db, auth.user_id).await?;
    Ok(Json(certs))
}
