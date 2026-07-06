use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;

use super::dto::{EnrolmentResponse, MyEnrolmentResponse};
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
    // can't read a stale capacity count.
    let max_students = repository::lock_course_capacity_tx(tx, course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    let active_count = repository::count_active_tx(tx, course_id).await?;
    if active_count >= max_students as i64 {
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

    if enrolment.user_id != auth.user_id && !auth.is_admin() {
        return Err(AppError::Forbidden(
            "you can only cancel your own enrolments".into(),
        ));
    }

    let updated = repository::cancel_if_active_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::Conflict("enrolment already cancelled".into()))?;

    tx.commit().await?;

    Ok(EnrolmentResponse::from(updated))
}
