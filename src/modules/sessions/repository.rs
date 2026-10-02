use sqlx::PgPool;
use uuid::Uuid;

use super::calendar::{MaterializedDay, MaterializedRange};
use super::model::{CourseSession, MyScheduleRow, TodaySessionRow};

pub async fn find_sessions_in(
    db: &PgPool,
    mat: &MaterializedRange,
) -> Result<Vec<CourseSession>, sqlx::Error> {
    sqlx::query_as::<_, CourseSession>(
        "SELECT id, course_id, session_date, start_time, end_time, created_at \
         FROM course_sessions \
         WHERE course_id = ANY($1::uuid[]) AND session_date BETWEEN $2 AND $3 \
         ORDER BY session_date, start_time",
    )
    .bind(mat.course_ids())
    .bind(mat.from_date())
    .bind(mat.to_date())
    .fetch_all(db)
    .await
}

/// All course ids — the materialize/query scope for an admin's
/// `GET /sessions/today`. A plain `SELECT id FROM courses` naming the table
/// directly (not going through `courses::repository`) mirrors the existing
/// cross-module JOIN convention (e.g. `enrolments::repository` joins
/// `courses` the same way).
pub async fn find_all_course_ids(db: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM courses").fetch_all(db).await
}

/// `coach_name` JOINs the same way as `find_my_weekly_schedule` (courses ->
/// coaches -> users, LEFT so a coachless course still yields a row).
/// `venue` is the session's own snapshot (`course_sessions.venue`, written
/// by `calendar::materialize_range`) — no rejoin to `course_schedule_slots`, so a
/// later slot start_time/venue edit doesn't blank or re-label it.
///
/// 收 [`MaterializedDay`]——單日前提已在型別層成立,不再需要在此自行
/// 斷言。
pub async fn find_today_sessions_in(
    db: &PgPool,
    day: &MaterializedDay,
) -> Result<Vec<TodaySessionRow>, sqlx::Error> {
    if day.course_ids().is_empty() {
        return Ok(Vec::new());
    }

    // enrolled_count:座位 COUNT 謂詞的顯示用 inline 拷貝——owner 是 `active_enrolments` view(migration `20260711000001`),非 `courses::seats`(見其模組 doc)。
    sqlx::query_as::<_, TodaySessionRow>(
        "SELECT cs.id, cs.course_id, c.name AS course_name, u.name AS coach_name, \
         cs.start_time, cs.end_time, \
         (SELECT COUNT(*) FROM active_enrolments e WHERE e.course_id = cs.course_id) AS enrolled_count, \
         cs.venue \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         LEFT JOIN coaches co ON co.id = c.coach_id \
         LEFT JOIN users u ON u.id = co.user_id \
         WHERE cs.session_date = $1 AND cs.course_id = ANY($2::uuid[]) \
         ORDER BY cs.start_time",
    )
    .bind(day.date())
    .bind(day.course_ids())
    .fetch_all(db)
    .await
}

/// The caller's weekly schedule: every schedule slot belonging to a course
/// they hold an *active* enrolment in. Not materialized — a direct read of
/// the weekly pattern, per the task brief.
pub async fn find_my_weekly_schedule(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<MyScheduleRow>, sqlx::Error> {
    sqlx::query_as::<_, MyScheduleRow>(
        "SELECT c.id AS course_id, c.name AS course_name, u.name AS coach_name, \
         s.day_of_week, s.start_time, s.end_time, s.venue \
         FROM active_enrolments e \
         JOIN courses c ON c.id = e.course_id \
         JOIN course_schedule_slots s ON s.course_id = c.id \
         LEFT JOIN coaches co ON co.id = c.coach_id \
         LEFT JOIN users u ON u.id = co.user_id \
         WHERE e.user_id = $1 \
         ORDER BY s.day_of_week, s.start_time",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}
