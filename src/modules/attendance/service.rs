use std::collections::HashSet;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::modules::coaches::service as coaches_service;

use super::dto::{AttendanceRecordEntry, MyStudentResponse, RosterEntryResponse};
use super::model::AttendanceStatus;
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

/// `PUT /sessions/{id}/attendance`. Validates every record's `status` and
/// enrolment ownership *before* writing anything: an invalid status string,
/// or any enrolment that doesn't belong to this session's course and isn't
/// active, rejects the whole batch with zero writes (422). Otherwise upserts
/// each record in one transaction and returns the updated roster.
pub async fn bulk_upsert_attendance(
    db: &PgPool,
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

    let mut parsed: Vec<(Uuid, AttendanceStatus)> = Vec::with_capacity(records.len());
    for r in &records {
        let status: AttendanceStatus = r
            .status
            .parse()
            .map_err(|_| AppError::Validation(format!("invalid attendance status: {}", r.status)))?;
        parsed.push((r.enrolment_id, status));
    }

    if !parsed.is_empty() {
        let requested: HashSet<Uuid> = parsed.iter().map(|(id, _)| *id).collect();
        let ids: Vec<Uuid> = requested.iter().copied().collect();
        let valid: HashSet<Uuid> =
            repository::find_active_enrolment_ids_in(db, session_course.course_id, &ids)
                .await?
                .into_iter()
                .collect();
        if valid != requested {
            return Err(AppError::Validation(
                "all enrolments must belong to this session's course and be active".into(),
            ));
        }
    }

    let mut tx = db.begin().await?;
    for (enrolment_id, status) in &parsed {
        repository::upsert_attendance_tx(&mut tx, session_id, *enrolment_id, *status, auth.user_id)
            .await?;
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
