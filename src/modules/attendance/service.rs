use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::coaches::service as coaches_service;
use crate::utils::studio_clock;

use super::dto::{AttendanceRecordEntry, MyStudentResponse, RosterEntryResponse};
use super::marking;
use super::repository;

/// `GET /sessions/{id}/roster`. 404 if the session doesn't exist; 403 if the
/// caller is neither admin nor that course's coach.
pub async fn get_roster(
    db: &PgPool,
    auth: &AuthUser,
    session_id: Uuid,
) -> Result<Vec<RosterEntryResponse>, AppError> {
    let session_course = repository::find_session_course(db, session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("session not found".into()))?;
    coaches_service::require_course_coach(
        db,
        auth,
        session_course.coach_id,
        "not the coach for this course",
    )
    .await?;

    let rows = repository::find_roster(db, session_course.course_id, session_id).await?;
    Ok(rows.into_iter().map(RosterEntryResponse::from).collect())
}

/// `PUT /sessions/{id}/attendance`. Requires the session to have already
/// started (studio-local wall clock via `studio_clock::require_started` —
/// the polarity-mirrored inverse of `leave`'s "not yet started" gate),
/// rejected with 422 *before* validating anything else — even an empty
/// `records` batch. Then validates every record's `status` and enrolment
/// ownership *before* writing anything: an invalid status string, or any
/// enrolment that doesn't belong to this session's course and isn't
/// active, rejects the whole batch with zero writes (422). Otherwise upserts
/// each record in one transaction and returns the updated roster.
pub async fn bulk_upsert_attendance(
    db: &PgPool,
    server: &ServerConfig,
    now: DateTime<Utc>,
    auth: &AuthUser,
    session_id: Uuid,
    records: Vec<AttendanceRecordEntry>,
) -> Result<Vec<RosterEntryResponse>, AppError> {
    let session_course = repository::find_session_course(db, session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("session not found".into()))?;
    coaches_service::require_course_coach(
        db,
        auth,
        session_course.coach_id,
        "not the coach for this course",
    )
    .await?;

    studio_clock::require_started(
        studio_clock::studio_tz(server),
        now,
        session_course.session_date,
        session_course.start_time,
        "session time",
        AppError::Validation("場次尚未開始，無法點名".into()),
    )?;

    let parsed = marking::parse(&records)?;

    let valid: HashSet<Uuid> = if parsed.is_empty() {
        HashSet::new()
    } else {
        let requested: HashSet<Uuid> = parsed.iter().map(|(id, _)| *id).collect();
        let ids: Vec<Uuid> = requested.iter().copied().collect();
        repository::find_active_enrolment_ids_in(db, session_course.course_id, &ids)
            .await?
            .into_iter()
            .collect()
    };

    let mut tx = db.begin().await?;

    // Approved-leave guard, whole-batch pre-check (核准恆勝, ADR-0008): read
    // the batch's approved-leave enrolments *inside* the write tx, then let
    // `marking::plan` reject the whole batch (422) if any of them is being
    // marked present/absent. `plan` is pure, so its `Err` short-circuits via
    // `?` before any upsert runs — the tx rolls back with zero writes, keeping
    // the existing "whole batch 422, zero writes" contract. The `ON CONFLICT`
    // guard in `upsert_attendance_tx` closes the residual TOCTOU window — its
    // zero-row block is converted into this same batch-wide 422 in the upsert
    // loop below. Gate order is unchanged: parse (above) precedes both the
    // membership check and this approved-guard, which `plan` evaluates
    // together.
    let approved: HashSet<Uuid> = if parsed.is_empty() {
        HashSet::new()
    } else {
        let ids: Vec<Uuid> = parsed.iter().map(|(id, _)| *id).collect();
        repository::find_approved_leave_enrolment_ids_tx(&mut tx, session_id, &ids)
            .await?
            .into_iter()
            .collect()
    };
    let plan = marking::plan(parsed, &valid, &approved)?;

    for (enrolment_id, status) in &plan.entries {
        let affected =
            repository::upsert_attendance_tx(&mut tx, session_id, *enrolment_id, *status, auth.user_id)
                .await?;
        // Zero rows has exactly one producer: the write-point guard blocking a
        // present/absent over an approved leave whose approval committed after
        // this tx's approved-set read above. Surface the same 422 as the
        // pre-check; returning here drops the tx, so the whole batch rolls
        // back unwritten — the contract holds inside the race window too.
        if affected == 0 {
            return Err(AppError::Validation(
                "cannot overwrite an approved leave with present/absent".into(),
            ));
        }
    }
    tx.commit().await?;

    let rows = repository::find_roster(db, session_course.course_id, session_id).await?;
    Ok(rows.into_iter().map(RosterEntryResponse::from).collect())
}

/// `GET /coaches/me/students`. Empty list (not an error) if the caller has
/// the `coach` role but no `coaches` row — mirrors `sessions::today_sessions`'s
/// convention for that same data anomaly.
pub async fn my_students(db: &PgPool, auth: &AuthUser) -> Result<Vec<MyStudentResponse>, AppError> {
    let students = match coaches_service::resolve(db, auth).await? {
        Some(coach) => repository::find_my_students(db, coach.id).await?,
        None => Vec::new(),
    };
    Ok(students.into_iter().map(MyStudentResponse::from).collect())
}
