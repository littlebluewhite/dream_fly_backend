use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::coaches::repository as coaches_repository;
use crate::modules::notifications::service as notify;

use super::dto::{
    CertificateResponse, CreateCertificateRequest, CreateReportCardRequest, ReportCardResponse,
};
use super::repository;

/// Shared coach-ownership gate for `POST /report-cards`: an admin always
/// passes; a coach passes only if the enrolment's course belongs to them.
/// Mirrors `leave::service::authorize_course_coach` (copied rather than
/// shared — established per-module convention in this codebase).
async fn authorize_course_coach(
    db: &PgPool,
    auth: &AuthUser,
    course_coach_id: Option<Uuid>,
) -> Result<(), AppError> {
    if auth.is_admin() {
        return Ok(());
    }

    let is_owner = match (
        coaches_repository::find_by_user_id(db, auth.user_id).await?,
        course_coach_id,
    ) {
        (Some(coach), Some(course_coach_id)) => coach.id == course_coach_id,
        _ => false,
    };

    if is_owner {
        Ok(())
    } else {
        Err(AppError::Forbidden("非本課教練".into()))
    }
}

/// `POST /report-cards` — coach (own course's enrolment only) or admin.
/// Duplicate `(enrolment_id, term_label)` trips the DB UNIQUE constraint,
/// mapped here to a friendly 409.
pub async fn create_report_card(
    db: &PgPool,
    auth: &AuthUser,
    req: CreateReportCardRequest,
) -> Result<ReportCardResponse, AppError> {
    let ctx = repository::find_enrolment_course_coach(db, req.enrolment_id)
        .await?
        .ok_or_else(|| AppError::NotFound("報名紀錄不存在".into()))?;

    authorize_course_coach(db, auth, ctx.coach_id).await?;

    match repository::insert_report_card(
        db,
        req.enrolment_id,
        &req.term_label,
        req.comment.as_deref(),
        req.rating,
        auth.user_id,
    )
    .await
    {
        Ok(rc) => {
            let row = repository::find_report_card_row(db, rc.id).await?.ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "report_card {} vanished right after insert",
                    rc.id
                ))
            })?;
            Ok(ReportCardResponse::from(row))
        }
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            Err(AppError::Conflict("此期別已建立過成績單".into()))
        }
        Err(e) => Err(AppError::Database(e)),
    }
}

/// `GET /report-cards/me` — plain array (mirrors `leave-requests/me`'s `/me`
/// convention: no pagination), newest first.
pub async fn list_my_report_cards(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<ReportCardResponse>, AppError> {
    let rows = repository::find_my_report_cards(db, user_id).await?;
    Ok(rows.into_iter().map(ReportCardResponse::from).collect())
}

/// `POST /certificates` — coach (only for students who have or had an
/// enrolment in one of their own courses — active or cancelled, contract
/// §3.22) or admin (no restriction). Writes a "you got a new certificate"
/// notification to the recipient once the row is written.
pub async fn create_certificate(
    db: &PgPool,
    auth: &AuthUser,
    req: CreateCertificateRequest,
) -> Result<CertificateResponse, AppError> {
    if !auth.is_admin() {
        let coach = coaches_repository::find_by_user_id(db, auth.user_id)
            .await?
            .ok_or_else(|| AppError::Forbidden("僅能發給自己課程的學員".into()))?;

        let eligible =
            repository::user_has_enrolment_with_coach(db, req.user_id, coach.id).await?;
        if !eligible {
            return Err(AppError::Forbidden("僅能發給自己課程的學員".into()));
        }
    }

    let cert = repository::insert_certificate(
        db,
        req.user_id,
        req.course_id,
        &req.title,
        req.level.as_deref(),
        req.issued_on,
        auth.user_id,
        req.note.as_deref(),
    )
    .await?;

    notify::certificate_issued(db, req.user_id, &cert.title).await;

    let row = repository::find_certificate_row(db, cert.id).await?.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "certificate {} vanished right after insert",
            cert.id
        ))
    })?;
    Ok(CertificateResponse::from(row))
}

/// `GET /certificates/me` — plain array, newest first.
pub async fn list_my_certificates(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<CertificateResponse>, AppError> {
    let rows = repository::find_my_certificates(db, user_id).await?;
    Ok(rows.into_iter().map(CertificateResponse::from).collect())
}
