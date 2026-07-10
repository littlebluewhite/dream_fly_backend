use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, constraint_name};
use crate::extractors::auth::{AuthUser, invalidate_role_cache};
use crate::modules::auth::repository as auth_repository;
use crate::modules::users::repository as users_repository;

use super::dto::{
    ClockRecordResponse, CoachDetailResponse, CoachResponse, CoachScheduleResponse,
    CreateCoachRequest, ScheduleEntry, UpdateCoachRequest,
};
use super::repository;

fn coach_to_response(c: super::model::Coach) -> CoachResponse {
    CoachResponse {
        id: c.id,
        user_id: c.user_id,
        name: c.name,
        title: c.title,
        bio: c.bio,
        experience: c.experience,
        specialties: c.specialties,
        certifications: c.certifications,
        is_active: c.is_active,
        display_order: c.display_order,
        slug: c.slug,
        photo_url: c.photo_url,
        created_at: c.created_at,
    }
}

fn schedule_to_response(s: super::model::CoachSchedule) -> CoachScheduleResponse {
    CoachScheduleResponse {
        id: s.id,
        day_of_week: s.day_of_week,
        start_time: s.start_time,
        end_time: s.end_time,
        is_available: s.is_available,
    }
}

fn clock_record_to_response(r: super::model::ClockRecord) -> ClockRecordResponse {
    ClockRecordResponse {
        id: r.id,
        clock_in: r.clock_in,
        clock_out: r.clock_out,
        note: r.note,
        created_at: r.created_at,
    }
}

/// Load a coach by ID and verify that the caller is either the coach's own
/// user or an admin. Used as the shared authz helper for all ownership-gated
/// coach endpoints — the handlers MUST NOT do this themselves.
async fn require_own_coach_profile(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
) -> Result<super::model::Coach, AppError> {
    let coach = repository::find_by_id(db, coach_id)
        .await?
        .ok_or_else(|| AppError::NotFound("coach not found".into()))?;
    auth.owns_or_admin(coach.user_id, "not authorized")?;
    Ok(coach)
}

/// 呼叫者的教練身分:依 auth.user_id 查 coaches 列。None = 呼叫者沒有教練
/// profile。三態政策(403 gate / 範圍列表空集合 / 儀表板 404)由呼叫端決定,
/// 本函式只回答「這個使用者是哪個教練」。
pub async fn resolve(
    db: &PgPool,
    auth: &AuthUser,
) -> Result<Option<super::model::Coach>, AppError> {
    Ok(repository::find_by_user_id(db, auth.user_id).await?)
}

/// 課程教練所有權 gate:admin 直接放行;否則呼叫者必須是 course_coach_id 指到
/// 的那個教練,不是則 403(文案由呼叫端傳入,參數化以保各站點 byte-identical)。
pub async fn require_course_coach(
    db: &PgPool,
    auth: &AuthUser,
    course_coach_id: Option<Uuid>,
    forbidden_msg: &str,
) -> Result<(), AppError> {
    if auth.is_admin() {
        return Ok(());
    }

    let is_owner = match (resolve(db, auth).await?, course_coach_id) {
        (Some(coach), Some(course_coach_id)) => coach.id == course_coach_id,
        _ => false,
    };

    if is_owner {
        Ok(())
    } else {
        Err(AppError::Forbidden(forbidden_msg.into()))
    }
}

pub async fn list_active(db: &PgPool) -> Result<Vec<CoachResponse>, AppError> {
    let coaches = repository::find_all_active(db).await?;
    Ok(coaches.into_iter().map(coach_to_response).collect())
}

pub async fn get_detail(db: &PgPool, id: Uuid) -> Result<CoachDetailResponse, AppError> {
    let coach = repository::find_by_id(db, id)
        .await?
        .ok_or_else(|| AppError::NotFound("coach not found".into()))?;

    let schedules = repository::find_schedules(db, id).await?;

    Ok(CoachDetailResponse {
        coach: coach_to_response(coach),
        schedules: schedules.into_iter().map(schedule_to_response).collect(),
    })
}

/// `POST /coaches` (admin, checked by the handler). Binds an existing user
/// (created via `POST /users`) to a new coach profile and assigns them the
/// `coach` role, both inside one transaction so a role-assignment failure
/// can never leave an orphaned coach row (mirrors `users::service::create_user`
/// assigning `member` in the same transaction as the user insert).
///
/// After commit, invalidates the target user's Redis role cache
/// (`user_roles:{id}`, 15 min TTL) the same way
/// `permissions::service::assign_role_to_user` does, so the user's very next
/// request sees the `coach` role instead of a request within the TTL window
/// still evaluating against a pre-existing cached role set.
pub async fn create_coach(
    db: &PgPool,
    redis: &mut redis::aio::ConnectionManager,
    req: &CreateCoachRequest,
) -> Result<CoachResponse, AppError> {
    users_repository::find_by_id(db, req.user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("user not found".into()))?;

    let is_active = req.is_active.unwrap_or(true);
    let display_order = req.display_order.unwrap_or(0);

    let mut tx = db.begin().await?;

    let coach = repository::insert_tx(
        &mut tx,
        req.user_id,
        &req.title,
        req.bio.as_deref(),
        req.experience.as_deref(),
        &req.specialties,
        &req.certifications,
        is_active,
        display_order,
        req.slug.as_deref(),
        req.photo_url.as_deref(),
    )
    .await
    .map_err(|e| match constraint_name(&e) {
        Some("coaches_user_id_key") => AppError::Conflict("user is already a coach".into()),
        Some("coaches_slug_key") => {
            let slug = req.slug.as_deref().unwrap_or_default();
            AppError::Conflict(format!("coach slug '{}' already exists", slug))
        }
        _ => AppError::Database(e),
    })?;

    auth_repository::assign_role_tx(&mut tx, req.user_id, "coach").await?;

    tx.commit().await?;

    invalidate_role_cache(redis, req.user_id).await;

    Ok(coach_to_response(coach))
}

/// `PATCH /coaches/{id}` (admin, checked by the handler). Coach-owned fields
/// only — the coach's name lives on `users` and is edited via the existing
/// `PATCH /users/{id}`.
pub async fn update_coach(
    db: &PgPool,
    id: Uuid,
    req: &UpdateCoachRequest,
) -> Result<CoachResponse, AppError> {
    let coach = repository::update(
        db,
        id,
        req.title.as_deref(),
        req.bio.as_ref().map(|o| o.as_deref()),
        req.experience.as_ref().map(|o| o.as_deref()),
        req.specialties.as_deref(),
        req.certifications.as_deref(),
        req.is_active,
        req.display_order,
        req.slug.as_ref().map(|o| o.as_deref()),
        req.photo_url.as_ref().map(|o| o.as_deref()),
    )
    .await
    .map_err(|e| {
        let slug = req.slug.clone().flatten().unwrap_or_default();
        AppError::conflict_on_constraint(
            e,
            "coaches_slug_key",
            format!("coach slug '{}' already exists", slug),
        )
    })?
    .ok_or_else(|| AppError::NotFound("coach not found".into()))?;

    Ok(coach_to_response(coach))
}

pub async fn get_schedules(
    db: &PgPool,
    coach_id: Uuid,
) -> Result<Vec<CoachScheduleResponse>, AppError> {
    // Verify coach exists (public endpoint; no ownership check needed)
    repository::find_by_id(db, coach_id)
        .await?
        .ok_or_else(|| AppError::NotFound("coach not found".into()))?;

    let schedules = repository::find_schedules(db, coach_id).await?;
    Ok(schedules.into_iter().map(schedule_to_response).collect())
}

pub async fn update_schedules(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
    entries: &[ScheduleEntry],
) -> Result<Vec<CoachScheduleResponse>, AppError> {
    require_own_coach_profile(db, auth, coach_id).await?;

    let schedules = repository::replace_schedules(db, coach_id, entries).await?;
    Ok(schedules.into_iter().map(schedule_to_response).collect())
}

pub async fn clock_in(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
    note: Option<&str>,
) -> Result<ClockRecordResponse, AppError> {
    require_own_coach_profile(db, auth, coach_id).await?;

    // Double clock-in is prevented by the unique partial index
    // uq_clock_records_open (migration 00014). Translate that into a
    // friendly 409 instead of a raw DB error.
    let record = repository::clock_in(db, coach_id, note)
        .await
        .map_err(|e| AppError::conflict_on_unique(e, "already clocked in"))?;
    Ok(clock_record_to_response(record))
}

pub async fn clock_out(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
) -> Result<ClockRecordResponse, AppError> {
    require_own_coach_profile(db, auth, coach_id).await?;

    let record = repository::clock_out(db, coach_id)
        .await?
        .ok_or_else(|| AppError::NotFound("no active clock-in record found".into()))?;
    Ok(clock_record_to_response(record))
}

pub async fn get_clock_records(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
    limit: u32,
    offset: u32,
) -> Result<Vec<ClockRecordResponse>, AppError> {
    require_own_coach_profile(db, auth, coach_id).await?;

    let records = repository::find_clock_records(db, coach_id, limit, offset).await?;
    Ok(records.into_iter().map(clock_record_to_response).collect())
}
