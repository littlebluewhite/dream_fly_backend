use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::courses::seats;

use super::dto::{AttendanceEntryResponse, EnrolmentResponse, MyEnrolmentResponse};
use super::model::Enrolment;
use super::repository;

/// 容量與重複檢查後建立報名（在結帳交易內呼叫）。
/// 滿班 → AppError::Conflict("course is full")；已報 → Conflict("already enrolled")。
pub async fn enrol_from_purchase_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    course_id: Uuid,
    order_id: Uuid,
) -> Result<Enrolment, AppError> {
    // Lock the course row so a concurrent enrolment for the same course
    // can't read a stale capacity count (lock-then-count ordering lives in
    // `seats::lock_course_seats_tx`).
    let seats = seats::lock_course_seats_tx(tx, course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    if seats.is_full() {
        return Err(AppError::Conflict("course is full".into()));
    }

    // Pre-check for a friendly message; the partial unique index
    // (`uniq_enrolments_active`) is the race-proof second line of defense.
    if repository::exists_active_tx(tx, user_id, course_id).await? {
        return Err(AppError::Conflict("already enrolled".into()));
    }

    match repository::insert_tx(tx, user_id, course_id, order_id).await {
        Ok(enrolment) => Ok(enrolment),
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            Err(AppError::Conflict("already enrolled".into()))
        }
        Err(e) => Err(AppError::Database(e)),
    }
}

/// This user's enrolments, newest first, each with `attended`/`total`
/// attendance stats.
pub async fn list_my_enrolments(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<MyEnrolmentResponse>, AppError> {
    let rows = repository::find_by_user_with_course(db, user_id).await?;
    Ok(rows.into_iter().map(MyEnrolmentResponse::from).collect())
}

/// Cancel an enrolment. Owner or admin only; otherwise unconditional (no
/// 24-hour rule). Cancelling an already-cancelled enrolment is a 409.
pub async fn cancel_enrolment(
    db: &PgPool,
    auth: &AuthUser,
    id: Uuid,
) -> Result<EnrolmentResponse, AppError> {
    let mut tx = db.begin().await?;

    let enrolment = repository::find_by_id_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound("enrolment not found".into()))?;

    auth.owns_or_admin(enrolment.user_id, "you can only cancel your own enrolments")?;

    let updated = repository::cancel_if_active_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::Conflict("enrolment already cancelled".into()))?;

    tx.commit().await?;

    Ok(EnrolmentResponse::from(updated))
}

/// `GET /enrolments/{id}/attendance`. Owner or admin (mirrors
/// `cancel_enrolment`'s owner-or-admin convention); everyone else gets the
/// *same* 404 as a nonexistent id — unlike `cancel_enrolment`'s 403, this
/// endpoint deliberately masks existence so a non-owner can't probe which
/// enrolment ids are real.
pub async fn get_attendance(
    db: &PgPool,
    auth: &AuthUser,
    id: Uuid,
) -> Result<Vec<AttendanceEntryResponse>, AppError> {
    let owner_id = repository::find_owner(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("enrolment not found".into()))?;

    if owner_id != auth.user_id && !auth.is_admin() {
        return Err(AppError::NotFound("enrolment not found".into()));
    }

    let rows = repository::find_attendance_timeline(db, id).await?;
    Ok(rows.into_iter().map(AttendanceEntryResponse::from).collect())
}
