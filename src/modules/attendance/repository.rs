use sqlx::PgPool;
use uuid::Uuid;

use super::model::{MyStudentRow, RosterRow, SessionCourseRow};

/// A session's course id + that course's assigned coach id (may be `None`
/// if the course has no coach yet) — used by `service` to authorize
/// `GET /sessions/{id}/roster` / `PUT /sessions/{id}/attendance`. Returns
/// `None` if the session doesn't exist.
pub async fn find_session_course(
    db: &PgPool,
    session_id: Uuid,
) -> Result<Option<SessionCourseRow>, sqlx::Error> {
    sqlx::query_as::<_, SessionCourseRow>(
        "SELECT cs.course_id, c.coach_id, cs.session_date, cs.start_time \
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
         FROM active_enrolments e \
         JOIN users u ON u.id = e.user_id \
         LEFT JOIN attendance_records ar ON ar.session_id = $2 AND ar.enrolment_id = e.id \
         WHERE e.course_id = $1 \
         ORDER BY u.name, e.id",
    )
    .bind(course_id)
    .bind(session_id)
    .fetch_all(db)
    .await
}

/// Distinct students across 教練名下全部課程（含已下架，ADR-0012）的 active
/// enrolments,each with a `jsonb_agg`-aggregated `courses` list — one query
/// for the whole roster, not one per student. `WHERE` 子句中 enrolment-active
/// 的篩選已下沉為 `active_enrolments` view(migration `20260711000001`);剩下
/// 的 `c.coach_id` 條件與 [`count_my_students`] 是同一份謂詞,兩者一致性仍由
/// 本模組維護(原因見該函式 doc)。
pub async fn find_my_students(
    db: &PgPool,
    coach_id: Uuid,
) -> Result<Vec<MyStudentRow>, sqlx::Error> {
    sqlx::query_as::<_, MyStudentRow>(
        "SELECT u.id AS user_id, u.name, u.phone, \
                jsonb_agg(jsonb_build_object('course_id', c.id, 'course_name', c.name, \
                                             'enrolment_id', e.id) \
                          ORDER BY c.name) AS courses \
         FROM active_enrolments e \
         JOIN courses c ON c.id = e.course_id \
         JOIN users u ON u.id = e.user_id \
         WHERE c.coach_id = $1 \
         GROUP BY u.id, u.name, u.phone \
         ORDER BY u.name, u.id",
    )
    .bind(coach_id)
    .fetch_all(db)
    .await
}

/// Distinct student count across 教練名下全部課程（含已下架，ADR-0012）的
/// active enrolments — the `COUNT` variant of [`find_my_students`]'s roster
/// query,kept beside it so any future `WHERE` drift between the two is
/// visible in one file instead of split across modules. Enrolment-active
/// 這段謂詞現由 `active_enrolments` view 單一持有;`c.coach_id` 這段仍是本
/// 模組所有;current caller: `reports::service::coach_report`.
pub async fn count_my_students(db: &PgPool, coach_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT e.user_id) FROM active_enrolments e \
         JOIN courses c ON c.id = e.course_id \
         WHERE c.coach_id = $1",
    )
    .bind(coach_id)
    .fetch_one(db)
    .await
}
