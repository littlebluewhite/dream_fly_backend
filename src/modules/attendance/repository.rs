use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{AttendanceStatus, MyStudentRow, RosterRow, SessionCourseRow};

/// A session's course id + that course's assigned coach id (may be `None`
/// if the course has no coach yet) — used by `service` to authorize
/// `GET /sessions/{id}/roster` / `PUT /sessions/{id}/attendance`. Returns
/// `None` if the session doesn't exist.
pub async fn find_session_course(
    db: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionCourseRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionCourseRow>(
        "SELECT cs.course_id, c.coach_id \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         WHERE cs.id = $1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
}

/// The session's roster: the course's active enrolments JOINed with `users`,
/// LEFT JOINed with this specific session's `attendance_records` row (`NULL`
/// when unmarked). Single query, no N+1.
pub async fn find_roster(
    db: &PgPool,
    course_id: Uuid,
    session_id: Uuid,
) -> Result<Vec<RosterRow>, sqlx::Error> {
    sqlx::query_as::<_, RosterRow>(
        "SELECT e.id AS enrolment_id, u.id AS user_id, u.name AS user_name, \
                ar.status AS attendance_status \
         FROM enrolments e \
         JOIN users u ON u.id = e.user_id \
         LEFT JOIN attendance_records ar ON ar.session_id = $2 AND ar.enrolment_id = e.id \
         WHERE e.course_id = $1 AND e.status = 'active' \
         ORDER BY u.name, e.id",
    )
    .bind(course_id)
    .bind(session_id)
    .fetch_all(db)
    .await
}

/// Of the given `enrolment_ids`, return the subset that both belong to
/// `course_id` and are `active`. The caller compares this against the full
/// requested set — anything missing is either foreign to the course or not
/// active, and must reject the whole bulk-upsert batch before any write.
pub async fn find_active_enrolment_ids_in(
    db: &PgPool,
    course_id: Uuid,
    enrolment_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM enrolments \
         WHERE course_id = $1 AND status = 'active' AND id = ANY($2::uuid[])",
    )
    .bind(course_id)
    .bind(enrolment_ids)
    .fetch_all(db)
    .await
}

/// Upsert a single attendance mark within an already-open transaction —
/// `service` loops this once per record (mirrors
/// `coaches::repository::replace_schedules`'s per-row loop-in-tx style).
/// `ON CONFLICT DO UPDATE` never touches `created_at`, so the original
/// insert time survives repeated re-marking.
pub async fn upsert_attendance_tx(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
    enrolment_id: Uuid,
    status: AttendanceStatus,
    marked_by: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO attendance_records \
         (id, session_id, enrolment_id, status, marked_by, marked_at, created_at) \
         VALUES ($1, $2, $3, $4::attendance_status, $5, NOW(), NOW()) \
         ON CONFLICT (session_id, enrolment_id) DO UPDATE \
         SET status = EXCLUDED.status, marked_by = EXCLUDED.marked_by, marked_at = EXCLUDED.marked_at",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(enrolment_id)
    .bind(status.as_str())
    .bind(marked_by)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Distinct students across a coach's active courses' active enrolments,
/// each with a `jsonb_agg`-aggregated `courses` list — one query for the
/// whole roster, not one per student. `WHERE` clause is the same predicate
/// as [`count_my_students`]'s, both owned by this module (see that
/// function's doc for why).
pub async fn find_my_students(
    db: &PgPool,
    coach_id: Uuid,
) -> Result<Vec<MyStudentRow>, sqlx::Error> {
    sqlx::query_as::<_, MyStudentRow>(
        "SELECT u.id AS user_id, u.name, u.phone, \
                jsonb_agg(jsonb_build_object('course_id', c.id, 'course_name', c.name, \
                                             'enrolment_id', e.id) \
                          ORDER BY c.name) AS courses \
         FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         JOIN users u ON u.id = e.user_id \
         WHERE c.coach_id = $1 AND c.is_active = true AND e.status = 'active' \
         GROUP BY u.id, u.name, u.phone \
         ORDER BY u.name, u.id",
    )
    .bind(coach_id)
    .fetch_all(db)
    .await
}

/// Distinct student count across a coach's active courses' active
/// enrolments — the `COUNT` variant of [`find_my_students`]'s roster query,
/// kept beside it so any future `WHERE` drift between the two is visible in
/// one file instead of split across modules. This module owns the
/// predicate; current caller: `reports::service::coach_report`.
pub async fn count_my_students(db: &PgPool, coach_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT e.user_id) FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         WHERE c.coach_id = $1 AND c.is_active = true AND e.status = 'active'",
    )
    .bind(coach_id)
    .fetch_one(db)
    .await
}
