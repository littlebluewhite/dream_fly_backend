use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::model::{AdminCoachRow, AdminCourseRow};

// ---------------------------------------------------------------------------
// GET /reports/admin
// ---------------------------------------------------------------------------

/// 12 monthly revenue buckets ending at `now`'s `tz_name`-local month
/// (oldest first). `orders.paid_at` is converted to `tz_name`'s wall-clock
/// time before truncating to a month, so bucketing follows the studio's
/// calendar rather than whatever timezone the DB session happens to be in.
/// Revenue counts only the "paid family" (`paid`/`processing`/`completed`)
/// — `refunded`/`cancelled`/`pending` orders never contribute (a refunded
/// order keeps its original `paid_at` per `orders::repository::
/// update_status_and_paid_at_tx`, so this is a status filter, not a
/// `paid_at IS NOT NULL` filter). The `generate_series` LEFT JOIN
/// guarantees exactly 12 rows even when `orders` is empty — no month can
/// "disappear" for lack of matching rows.
pub async fn revenue_trend(
    db: &PgPool,
    now: DateTime<Utc>,
    tz_name: &str,
) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT to_char(m.month_start, 'YYYY-MM') AS month, \
                COALESCE(SUM(o.total_cents), 0)::bigint AS revenue_cents \
         FROM generate_series( \
                date_trunc('month', $1::timestamptz AT TIME ZONE $2) - interval '11 months', \
                date_trunc('month', $1::timestamptz AT TIME ZONE $2), \
                interval '1 month' \
              ) AS m(month_start) \
         LEFT JOIN orders o \
           ON date_trunc('month', o.paid_at AT TIME ZONE $2) = m.month_start \
          AND o.status IN ('paid', 'processing', 'completed') \
         GROUP BY m.month_start \
         ORDER BY m.month_start",
    )
    .bind(now)
    .bind(tz_name)
    .fetch_all(db)
    .await
}

/// `(total, new_this_month, active)` — one row, three scalar aggregates.
/// `total`/`new_this_month` count every `users` row (no role filter, per
/// task brief); `active` = distinct users holding at least one `active`
/// enrolment.
pub async fn member_stats(
    db: &PgPool,
    now: DateTime<Utc>,
    tz_name: &str,
) -> Result<(i64, i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT COUNT(*), \
                COUNT(*) FILTER ( \
                  WHERE date_trunc('month', u.created_at AT TIME ZONE $2) \
                      = date_trunc('month', $1::timestamptz AT TIME ZONE $2) \
                ), \
                (SELECT COUNT(DISTINCT user_id) FROM enrolments WHERE status = 'active') \
         FROM users u",
    )
    .bind(now)
    .bind(tz_name)
    .fetch_one(db)
    .await
}

/// Every course with its live `enrolled`/`waitlist_count` (correlated
/// subqueries — same pattern as `courses::model::Course.enrolled_count`),
/// so `fill_rate` can be derived in `service` without a second query.
pub async fn course_reports(db: &PgPool) -> Result<Vec<AdminCourseRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminCourseRow>(
        "SELECT c.id AS course_id, c.name, \
                (SELECT COUNT(*) FROM enrolments e \
                  WHERE e.course_id = c.id AND e.status = 'active') AS enrolled, \
                c.max_students, \
                (SELECT COUNT(*) FROM waitlist_entries w \
                  WHERE w.course_id = c.id AND w.status = 'waiting') AS waitlist_count \
         FROM courses c \
         ORDER BY c.name, c.id",
    )
    .fetch_all(db)
    .await
}

/// Every coach with their `course_count`/`student_count` (correlated
/// subqueries, one query, no N+1).
pub async fn coach_reports(db: &PgPool) -> Result<Vec<AdminCoachRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminCoachRow>(
        "SELECT co.id AS coach_id, u.name, \
                (SELECT COUNT(*) FROM courses c \
                  WHERE c.coach_id = co.id) AS course_count, \
                (SELECT COUNT(DISTINCT e.user_id) FROM enrolments e \
                   JOIN courses c ON c.id = e.course_id \
                  WHERE c.coach_id = co.id AND e.status = 'active') AS student_count \
         FROM coaches co \
         JOIN users u ON u.id = co.user_id \
         ORDER BY u.name, co.id",
    )
    .fetch_all(db)
    .await
}

// ---------------------------------------------------------------------------
// GET /reports/coach
// ---------------------------------------------------------------------------

/// `(today_sessions, pending_attendance)` for `coach_id`'s courses on
/// `today`. Caller must materialize `today` first (see
/// `sessions::repository::materialize_range`) — this only counts
/// already-existing `course_sessions` rows.
pub async fn coach_today_and_pending(
    db: &PgPool,
    coach_id: Uuid,
    today: NaiveDate,
) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*), \
                COUNT(*) FILTER (WHERE NOT EXISTS ( \
                  SELECT 1 FROM attendance_records ar WHERE ar.session_id = cs.id \
                )) \
         FROM course_sessions cs \
         JOIN courses c ON c.id = cs.course_id \
         WHERE c.coach_id = $1 AND cs.session_date = $2",
    )
    .bind(coach_id)
    .bind(today)
    .fetch_one(db)
    .await
}

/// Total unread messages across every conversation `user_id` participates
/// in, on either side — mirrors `messages::repository::
/// find_my_conversations`'s per-conversation `unread_count` correlated
/// subquery, aggregated here to one grand total instead of one row per
/// conversation.
pub async fn unread_message_count(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM messages m \
         JOIN conversations c ON c.id = m.conversation_id \
         WHERE (c.member_id = $1 OR c.coach_id = $1) \
           AND m.sender_id <> $1 AND m.read_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// Distinct students across `coach_id`'s *active* courses' *active*
/// enrolments. Mirrors `attendance::repository::find_my_students`'s WHERE
/// clause (copied, not shared — that module's own comment documents this
/// as the established convention) but as a bare count, without the
/// `jsonb_agg` course list `find_my_students` builds for its own response.
pub async fn coach_student_count(db: &PgPool, coach_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT e.user_id) FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         WHERE c.coach_id = $1 AND c.is_active = true AND e.status = 'active'",
    )
    .bind(coach_id)
    .fetch_one(db)
    .await
}

/// `(present_count, absent_count)` across `coach_id`'s courses' sessions in
/// `[from, to]`. `leave` rows are never selected into either bucket — the
/// brief's "leave 不入分母" rule.
pub async fn coach_attendance_in_range(
    db: &PgPool,
    coach_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*) FILTER (WHERE ar.status = 'present'), \
                COUNT(*) FILTER (WHERE ar.status = 'absent') \
         FROM attendance_records ar \
         JOIN course_sessions cs ON cs.id = ar.session_id \
         JOIN courses c ON c.id = cs.course_id \
         WHERE c.coach_id = $1 AND cs.session_date BETWEEN $2 AND $3",
    )
    .bind(coach_id)
    .bind(from)
    .bind(to)
    .fetch_one(db)
    .await
}

// ---------------------------------------------------------------------------
// GET /reports/me
// ---------------------------------------------------------------------------

/// `(present_count, absent_count)` across every session ever marked for any
/// of `user_id`'s enrolments — not filtered by enrolment status, since a
/// cancelled enrolment doesn't erase attendance history that already
/// happened. `leave` rows are never selected into either bucket.
pub async fn member_attendance(db: &PgPool, user_id: Uuid) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT COUNT(*) FILTER (WHERE ar.status = 'present'), \
                COUNT(*) FILTER (WHERE ar.status = 'absent') \
         FROM attendance_records ar \
         JOIN enrolments e ON e.id = ar.enrolment_id \
         WHERE e.user_id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

pub async fn points_balance(db: &PgPool, user_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT points_balance FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// Course ids of `user_id`'s *active* enrolments. `uniq_enrolments_active`
/// guarantees at most one active enrolment per (user, course), so the
/// returned row count already equals `active_enrolments` — the caller
/// doesn't need a separate COUNT query.
pub async fn my_active_enrolment_course_ids(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT course_id FROM enrolments WHERE user_id = $1 AND status = 'active'",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Count of `course_ids`' materialized sessions in `[from, to]`. Caller
/// must materialize that range first (see
/// `sessions::repository::materialize_range`).
pub async fn upcoming_session_count(
    db: &PgPool,
    course_ids: &[Uuid],
    from: NaiveDate,
    to: NaiveDate,
) -> Result<i64, sqlx::Error> {
    if course_ids.is_empty() {
        return Ok(0);
    }
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM course_sessions \
         WHERE course_id = ANY($1::uuid[]) AND session_date BETWEEN $2 AND $3",
    )
    .bind(course_ids)
    .bind(from)
    .bind(to)
    .fetch_one(db)
    .await
}
