use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::courses::seats::{self, CourseLocks};

use super::dto::{AttendanceEntryResponse, EnrolmentResponse, MyEnrolmentResponse};
use super::model::Enrolment;
use super::repository;

/// 容量與重複檢查後建立報名（`enrol_batch_from_purchase_tx` 的私有單步）。
/// 滿班 → AppError::Conflict("course is full")；已報 → Conflict("already enrolled")。
async fn enrol_one_tx(
    tx: &mut Transaction<'_, Postgres>,
    locks: &CourseLocks,
    user_id: Uuid,
    course_id: Uuid,
    order_id: Uuid,
) -> Result<Enrolment, AppError> {
    // The course row is already locked (`locks`), so a concurrent enrolment
    // for the same course can't read a stale capacity count (lock-then-count
    // ordering lives in `seats::course_seats_tx`).
    let seats = seats::course_seats_tx(tx, locks, course_id)
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

    repository::insert_tx(tx, user_id, course_id, order_id)
        .await
        .map_err(|e| AppError::conflict_on_unique(e, "already enrolled"))
}

/// 批次課程報名(結帳交易內呼叫)——`checkout` 課程行的深函式對應物,鏡射
/// `products::service::reserve_stock_tx` 對商品行的角色。
///
/// 收 `&CourseLocks`(`courses::seats`):每門課的 `courses` 列必須已由
/// `lock_courses_tx` 鎖住(witness 外的課程 → `AppError::Internal`),並依
/// witness 的鎖序(`CourseLocks::in_lock_order`,course_id 升序)逐一走訪——
/// 排序歸 witness(訂單鎖協定,`orders::locks`),本函式不再自行排序。**不
/// dedup**:`uniq_cart_items_course` partial unique index(migration
/// `20260704000001`)保證同一使用者的購物車不會出現同一門課兩行,`course_ids`
/// 天生無重複。
///
/// 逐一委派私有 `enrol_one_tx`。任一門課額滿或重複報名回 `AppError::Conflict`
/// (多門同時失敗時點名鎖序最前、即 course_id 最小者),`?` 直接上拋,讓整筆
/// 結帳交易回滾——部分報名不是可接受的結果(見 ADR-0002)。
pub async fn enrol_batch_from_purchase_tx(
    tx: &mut Transaction<'_, Postgres>,
    locks: &CourseLocks,
    user_id: Uuid,
    course_ids: &[Uuid],
    order_id: Uuid,
) -> Result<Vec<Enrolment>, AppError> {
    let ordered = locks.in_lock_order(course_ids.to_vec(), |course_id| *course_id)?;

    let mut enrolments = Vec::with_capacity(ordered.len());
    for course_id in ordered {
        enrolments.push(enrol_one_tx(tx, locks, user_id, course_id, order_id).await?);
    }

    Ok(enrolments)
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

/// This order's enrolments, mapped to their response DTOs. The DTO wrapping
/// lives in this owning module (not in `orders::service`) per ADR-0005's
/// "DTO 包裝在 service.rs" placement rule; `orders::service::fetch_artifacts`
/// calls this seam so `orders` never reaches into this module's repository.
pub async fn list_by_order(
    db: &PgPool,
    order_id: Uuid,
) -> Result<Vec<EnrolmentResponse>, AppError> {
    let rows = repository::find_by_order(db, order_id).await?;
    Ok(rows.into_iter().map(EnrolmentResponse::from).collect())
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

/// Passthrough to `repository::cancel_by_order_tx` — the ADR-0005 seam
/// `orders::service`'s refund/cancel compensation
/// (`compensate_order_artifacts_tx`) calls, so
/// `orders` never imports this module's repository directly.
pub async fn cancel_by_order_tx(
    tx: &mut Transaction<'_, Postgres>,
    order_id: Uuid,
) -> Result<u64, AppError> {
    repository::cancel_by_order_tx(tx, order_id)
        .await
        .map_err(AppError::Database)
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

    auth.owns_or_admin_masked(owner_id, "enrolment not found")?;

    let rows = repository::find_attendance_timeline(db, id).await?;
    Ok(rows.into_iter().map(AttendanceEntryResponse::from).collect())
}
