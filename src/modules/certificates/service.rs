use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::coaches::service as coaches_service;
use crate::modules::notifications::service as notify;

use super::dto::{
    CertificateResponse, CreateCertificateRequest, CreateReportCardRequest, ReportCardResponse,
};
use super::repository;

/// `POST /report-cards` — coach (own course's enrolment only) or admin.
/// Duplicate `(enrolment_id, term_label)` trips the DB UNIQUE constraint,
/// mapped here to a friendly 409. Authorization runs on the pool, ahead of
/// the write transaction (same shape as `leave::decide_leave_request`);
/// insert + read-back share one transaction, so a duplicate rolls back with
/// zero rows written.
pub async fn create_report_card(
    db: &PgPool,
    auth: &AuthUser,
    req: CreateReportCardRequest,
) -> Result<ReportCardResponse, AppError> {
    let ctx = repository::find_enrolment_course_coach(db, req.enrolment_id)
        .await?
        .ok_or_else(|| AppError::NotFound("報名紀錄不存在".into()))?;

    coaches_service::require_course_coach(db, auth, ctx.coach_id, "非本課教練").await?;

    let mut tx = db.begin().await?;

    let rc = repository::insert_report_card_tx(
        &mut tx,
        req.enrolment_id,
        &req.term_label,
        req.comment.as_deref(),
        req.rating,
        auth.user_id,
    )
    .await
    .map_err(|e| AppError::conflict_on_unique(e, "此期別已建立過成績單"))?;

    // 同一筆 tx 讀自己剛插入的列,不可能落空——保留 Internal 分支是防禦性
    // 寫法,不是可觸達的執行路徑(落空即 early return,tx 未 commit 故自動
    // rollback)。
    let row = repository::find_report_card_row_tx(&mut tx, rc.id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "report_card {} vanished right after insert",
                rc.id
            ))
        })?;

    tx.commit().await?;

    Ok(ReportCardResponse::from(row))
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
/// notification to the recipient after the write transaction commits.
pub async fn create_certificate(
    db: &PgPool,
    auth: &AuthUser,
    req: CreateCertificateRequest,
) -> Result<CertificateResponse, AppError> {
    if !auth.is_admin() {
        let coach = coaches_service::resolve(db, auth)
            .await?
            .ok_or_else(|| AppError::Forbidden("僅能發給自己課程的學員".into()))?;

        let eligible =
            repository::user_has_enrolment_with_coach(db, req.user_id, coach.id).await?;
        if !eligible {
            return Err(AppError::Forbidden("僅能發給自己課程的學員".into()));
        }
    }

    let mut tx = db.begin().await?;

    let cert = repository::insert_certificate_tx(
        &mut tx,
        req.user_id,
        req.course_id,
        &req.title,
        req.level.as_deref(),
        req.issued_on,
        auth.user_id,
        req.note.as_deref(),
    )
    .await?;

    // 同一筆 tx 讀自己剛插入的列,不可能落空——保留 Internal 分支是防禦性
    // 寫法,不是可觸達的執行路徑(落空即 early return,tx 未 commit 故自動
    // rollback)。
    let row = repository::find_certificate_row_tx(&mut tx, cert.id)
        .await?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "certificate {} vanished right after insert",
                cert.id
            ))
        })?;

    tx.commit().await?;

    notify::certificate_issued(req.user_id, &cert.title)
        .deliver(db)
        .await;

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
