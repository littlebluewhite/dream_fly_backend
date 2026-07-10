use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::courses::seats;

use super::dto::WaitlistResponse;
use super::repository;

/// 加入候補：課程必須存在、上架、且已滿班才允許加入；重複候補會被擋下。
///
/// No `FOR UPDATE` lock on the fullness check here (unlike enrolments'
/// `enrol_from_purchase_tx`, which takes `seats::lock_course_seats_tx`): a
/// waitlist join racing a concurrent enrolment cancellation and reading a
/// stale "full" count is acceptable staleness for this feature — the
/// `&PgPool`-typed `seats::course_seats` declares exactly that (see
/// `courses::seats`'s lock-strategy doc).
pub async fn join_waitlist(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
) -> Result<WaitlistResponse, AppError> {
    let course = crate::modules::courses::repository::find_by_id(db, course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("course not found".into()))?;

    if !course.is_active {
        return Err(AppError::BadRequest("course is not available".into()));
    }

    let seats = seats::course_seats(db, course_id)
        .await?
        .ok_or_else(|| AppError::NotFound("course not found".into()))?;
    if !seats.is_full() {
        return Err(AppError::Conflict("course is not full".into()));
    }

    // Pre-check for a friendly message; the partial unique index
    // (`uniq_waitlist_waiting`) is the race-proof second line of defense.
    if repository::exists_waiting(db, user_id, course_id).await? {
        return Err(AppError::Conflict("already on waitlist".into()));
    }

    match repository::insert(db, user_id, course_id).await {
        Ok(entry) => Ok(WaitlistResponse {
            id: entry.id,
            course_id: entry.course_id,
            course_name: course.name,
            status: entry.status.as_str().to_string(),
            created_at: entry.created_at,
        }),
        Err(sqlx::Error::Database(ref db_err)) if db_err.is_unique_violation() => {
            Err(AppError::Conflict("already on waitlist".into()))
        }
        Err(e) => Err(AppError::Database(e)),
    }
}

/// This user's waitlist entries, newest first (includes cancelled ones).
pub async fn list_my_waitlist(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<WaitlistResponse>, AppError> {
    let rows = repository::find_by_user_with_course(db, user_id).await?;
    Ok(rows.into_iter().map(WaitlistResponse::from).collect())
}

/// Cancel a waitlist entry. Owner or admin only. Unlike enrolments (409 on
/// double-cancel), an already-cancelled entry 404s — it's treated as gone
/// since re-joining is the supported way back onto the list.
pub async fn cancel_waitlist_entry(db: &PgPool, auth: &AuthUser, id: Uuid) -> Result<(), AppError> {
    let mut tx = db.begin().await?;

    let entry = repository::find_by_id_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound("waitlist entry not found".into()))?;

    auth.owns_or_admin(entry.user_id, "you can only cancel your own waitlist entries")?;

    repository::cancel_if_waiting_tx(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::NotFound("waitlist entry not found".into()))?;

    tx.commit().await?;

    Ok(())
}

/// Waiting entries for a course, oldest first (queue order). Admin only.
pub async fn list_for_course(
    db: &PgPool,
    course_id: Uuid,
) -> Result<Vec<WaitlistResponse>, AppError> {
    let rows = repository::find_by_course_waiting(db, course_id).await?;
    Ok(rows.into_iter().map(WaitlistResponse::from).collect())
}
