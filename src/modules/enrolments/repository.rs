use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{Enrolment, EnrolmentAttendanceRow, EnrolmentWithCourse, MyEnrolmentRow};

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
        "SELECT EXISTS(SELECT 1 FROM active_enrolments WHERE user_id = $1 AND course_id = $2)",
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
/// countable_attendance` (no N+1 — one query for the whole list; view
/// membership is `present`/`absent` only, `leave` excluded). `attended`
/// counts that enrolment's `present` rows; `total` counts `present` +
/// `absent` (the view's membership itself is the denominator) — `leave`
/// and never-marked sessions count toward neither.
pub async fn find_by_user_with_course(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<MyEnrolmentRow>, sqlx::Error> {
    sqlx::query_as::<_, MyEnrolmentRow>(
        "SELECT e.id, e.course_id, c.name AS course_name, c.level AS course_level, \
                c.schedule_text, e.status, e.enrolled_at, \
                COUNT(*) FILTER (WHERE ca.is_present) AS attended, \
                COUNT(ca.id) AS total \
         FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         LEFT JOIN countable_attendance ca ON ca.enrolment_id = e.id \
         WHERE e.user_id = $1 \
         GROUP BY e.id, c.name, c.level, c.schedule_text, e.status, e.enrolled_at \
         ORDER BY e.enrolled_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// This order's enrolments JOINed with course info, oldest first. Used by
/// `orders::service::fetch_artifacts` to assemble the checkout response
/// (fresh, replayed, or re-fetched via `GET /orders/{id}`) — distinct from
/// [`find_by_user_with_course`] above: filters by `order_id` instead of
/// `user_id`, no attendance aggregation, and ASC order (checkout wants
/// purchase order, not newest-first).
pub async fn find_by_order(
    db: &PgPool,
    order_id: Uuid,
) -> Result<Vec<EnrolmentWithCourse>, sqlx::Error> {
    sqlx::query_as::<_, EnrolmentWithCourse>(
        "SELECT e.id, e.course_id, c.name AS course_name, c.level AS course_level, \
                c.schedule_text, e.status, e.enrolled_at \
         FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         WHERE e.order_id = $1 \
         ORDER BY e.enrolled_at",
    )
    .bind(order_id)
    .fetch_all(db)
    .await
}

/// This enrolment's owning user id, or `None` if the enrolment doesn't
/// exist. Used by `service::get_attendance`'s ownership gate — kept as a
/// single scalar column (not the full `Enrolment` row) since that's all the
/// 404-masking check needs.
pub async fn find_owner(db: &PgPool, id: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM enrolments WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// This enrolment's marked sessions — `attendance_records` JOINed with
/// `course_sessions` for the date/time fields, oldest session first.
/// Sessions with no `attendance_records` row (unmarked) don't appear, since
/// the join is driven from `attendance_records`. Served by
/// `idx_attendance_records_enrolment` (no new index needed).
pub async fn find_attendance_timeline(
    db: &PgPool,
    enrolment_id: Uuid,
) -> Result<Vec<EnrolmentAttendanceRow>, sqlx::Error> {
    sqlx::query_as::<_, EnrolmentAttendanceRow>(
        "SELECT cs.session_date, cs.start_time, cs.end_time, ar.status, ar.marked_at \
         FROM attendance_records ar \
         JOIN course_sessions cs ON cs.id = ar.session_id \
         WHERE ar.enrolment_id = $1 \
         ORDER BY cs.session_date ASC, cs.start_time ASC",
    )
    .bind(enrolment_id)
    .fetch_all(db)
    .await
}
