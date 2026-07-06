use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{Enrolment, EnrolmentWithCourse, MyEnrolmentRow};

/// Lock the course row for update and return its capacity (`max_students`).
/// Used by `enrol_from_purchase_tx` so the capacity check and the
/// subsequent insert serialize against concurrent enrolments for the same
/// course. Returns `None` if the course doesn't exist.
pub async fn lock_course_capacity_tx(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar::<_, i32>("SELECT max_students FROM courses WHERE id = $1 FOR UPDATE")
        .bind(course_id)
        .fetch_optional(&mut **tx)
        .await
}

/// Count of active enrolments for a course. Read after the course row lock
/// above so it reflects a consistent snapshot for the capacity check.
pub async fn count_active_tx(
    tx: &mut Transaction<'_, Postgres>,
    course_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM enrolments WHERE course_id = $1 AND status = 'active'",
    )
    .bind(course_id)
    .fetch_one(&mut **tx)
    .await
}

/// Pre-check for a friendly duplicate-enrolment message. The partial unique
/// index `uniq_enrolments_active` is the race-proof authoritative guard —
/// this SELECT just avoids the round-trip-to-error path in the common
/// (non-racing) case.
pub async fn exists_active_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    course_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM enrolments WHERE user_id = $1 AND course_id = $2 AND status = 'active')",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn insert_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    course_id: Uuid,
    order_id: Uuid,
) -> Result<Enrolment, sqlx::Error> {
    sqlx::query_as::<_, Enrolment>(
        "INSERT INTO enrolments (id, user_id, course_id, order_id, status, enrolled_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'active'::enrolment_status, NOW(), NOW(), NOW()) \
         RETURNING id, user_id, course_id, order_id, status, enrolled_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(course_id)
    .bind(order_id)
    .fetch_one(&mut **tx)
    .await
}

/// Transactional lookup with a row lock, used by the cancel path's
/// ownership check.
pub async fn find_by_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<Enrolment>, sqlx::Error> {
    sqlx::query_as::<_, Enrolment>(
        "SELECT id, user_id, course_id, order_id, status, enrolled_at, created_at, updated_at \
         FROM enrolments WHERE id = $1 \
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Conditional cancel, JOINed with `courses` so the response can be built
/// straight from the row this UPDATE produces. Returns `None` if the
/// enrolment was already cancelled.
pub async fn cancel_if_active_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<EnrolmentWithCourse>, sqlx::Error> {
    sqlx::query_as::<_, EnrolmentWithCourse>(
        "UPDATE enrolments e SET status = 'cancelled'::enrolment_status, updated_at = NOW() \
         FROM courses c \
         WHERE e.id = $1 AND e.course_id = c.id AND e.status <> 'cancelled'::enrolment_status \
         RETURNING e.id, e.course_id, c.name AS course_name, c.level AS course_level, \
                   c.schedule_text, e.status, e.enrolled_at",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// This user's enrolments JOINed with course info, newest first, plus
/// per-enrolment attendance stats aggregated with a single `LEFT JOIN
/// attendance_records` (no N+1 — one query for the whole list). `attended`
/// counts that enrolment's `present` records; `total` counts all of its
/// attendance_records regardless of status (i.e. sessions marked so far).
pub async fn find_by_user_with_course(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<MyEnrolmentRow>, sqlx::Error> {
    sqlx::query_as::<_, MyEnrolmentRow>(
        "SELECT e.id, e.course_id, c.name AS course_name, c.level AS course_level, \
                c.schedule_text, e.status, e.enrolled_at, \
                COUNT(CASE WHEN ar.status = 'present'::attendance_status THEN 1 END) AS attended, \
                COUNT(ar.id) AS total \
         FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         LEFT JOIN attendance_records ar ON ar.enrolment_id = e.id \
         WHERE e.user_id = $1 \
         GROUP BY e.id, c.name, c.level, c.schedule_text, e.status, e.enrolled_at \
         ORDER BY e.enrolled_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}
