use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;

use super::dto::{
    ClockRecordResponse, CoachDetailResponse, CoachResponse, CoachScheduleResponse,
    ScheduleEntry,
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
async fn require_coach_access(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
) -> Result<super::model::Coach, AppError> {
    let coach = repository::find_by_id(db, coach_id)
        .await?
        .ok_or_else(|| AppError::NotFound("coach not found".into()))?;
    if coach.user_id != auth.user_id && !auth.is_admin() {
        return Err(AppError::Forbidden("not authorized".into()));
    }
    Ok(coach)
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
    require_coach_access(db, auth, coach_id).await?;

    let schedules = repository::replace_schedules(db, coach_id, entries).await?;
    Ok(schedules.into_iter().map(schedule_to_response).collect())
}

pub async fn clock_in(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
    note: Option<&str>,
) -> Result<ClockRecordResponse, AppError> {
    require_coach_access(db, auth, coach_id).await?;

    let record = repository::clock_in(db, coach_id, note).await.map_err(|e| {
        // Double clock-in is prevented by the unique partial index
        // uq_clock_records_open (migration 00014). Translate that into a
        // friendly 409 instead of a raw DB error.
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.is_unique_violation() {
                return AppError::Conflict("already clocked in".into());
            }
        }
        AppError::Database(e)
    })?;
    Ok(clock_record_to_response(record))
}

pub async fn clock_out(
    db: &PgPool,
    auth: &AuthUser,
    coach_id: Uuid,
) -> Result<ClockRecordResponse, AppError> {
    require_coach_access(db, auth, coach_id).await?;

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
    require_coach_access(db, auth, coach_id).await?;

    let records = repository::find_clock_records(db, coach_id, limit, offset).await?;
    Ok(records.into_iter().map(clock_record_to_response).collect())
}
