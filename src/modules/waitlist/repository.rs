use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{WaitlistEntry, WaitlistEntryWithCourse};

/// The waitlist read projection's select list — single owner of
/// [`WaitlistEntryWithCourse`]'s shape. `w` is the row source's alias: the
/// `waitlist_entries` table, the `waiting_entries` view, or [`insert`]'s
/// data-modifying CTE alike.
const VIEW_COLUMNS: &str = "w.id, w.course_id, c.name AS course_name, w.status, w.created_at";

/// [`VIEW_COLUMNS`]'s matching JOIN clause — assumes a `w` row source
/// already in scope.
const VIEW_JOINS: &str = "JOIN courses c ON c.id = w.course_id";

/// Pre-check for a friendly duplicate-waitlist message. The partial unique
/// index `uniq_waitlist_waiting` is the race-proof authoritative guard —
/// this SELECT just avoids the round-trip-to-error path in the common
/// (non-racing) case.
pub async fn exists_waiting(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM waiting_entries WHERE user_id = $1 AND course_id = $2)",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_one(db)
    .await
}

/// Insert a new `waiting` entry, returning its read projection row directly
/// — a data-modifying CTE (`WITH w AS (INSERT ... RETURNING *)`) feeds the
/// insert's own output back through [`VIEW_JOINS`]. The CTE's source is the
/// `RETURNING` output, never the `waiting_entries` view: a statement's
/// sub-queries see the snapshot from before it, so the view would not yet
/// contain the row being inserted. A second `waiting` row for the same
/// user+course trips the partial unique index `uniq_waitlist_waiting`
/// (23505) — `service` maps that to a friendly 409.
pub async fn insert(
    db: &PgPool,
    user_id: Uuid,
    course_id: Uuid,
) -> Result<WaitlistEntryWithCourse, sqlx::Error> {
    sqlx::query_as::<_, WaitlistEntryWithCourse>(sqlx::AssertSqlSafe(format!(
        "WITH w AS ( \
             INSERT INTO waitlist_entries \
             (id, user_id, course_id, status, created_at, updated_at) \
             VALUES ($1, $2, $3, 'waiting'::waitlist_status, NOW(), NOW()) \
             RETURNING * \
         ) \
         SELECT {VIEW_COLUMNS} FROM w {VIEW_JOINS}"
    )))
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(course_id)
    .fetch_one(db)
    .await
}

/// Transactional lookup with a row lock, used by the cancel path's
/// ownership check.
pub async fn find_by_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<WaitlistEntry>, sqlx::Error> {
    sqlx::query_as::<_, WaitlistEntry>(
        "SELECT id, user_id, course_id, status, created_at, updated_at \
         FROM waitlist_entries WHERE id = $1 \
         FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// Conditional cancel. Returns `None` if the entry was not `waiting` (i.e.
/// already cancelled) — the service maps that to 404, not 409 like
/// enrolments, since a cancelled waitlist entry is no longer addressable
/// (re-joining is the supported way back in). Returns the bare entry, not
/// the read projection — `DELETE` has no response body to project.
pub async fn cancel_if_waiting_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<WaitlistEntry>, sqlx::Error> {
    sqlx::query_as::<_, WaitlistEntry>(
        "UPDATE waitlist_entries SET status = 'cancelled'::waitlist_status, updated_at = NOW() \
         WHERE id = $1 AND status = 'waiting'::waitlist_status \
         RETURNING id, user_id, course_id, status, created_at, updated_at",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
}

/// This user's waitlist entries JOINed with course info, newest first.
/// Includes cancelled entries (mirrors `enrolments`' `/me` listing, which
/// shows full history rather than filtering to a single status).
pub async fn find_by_user_with_course(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<WaitlistEntryWithCourse>, sqlx::Error> {
    sqlx::query_as::<_, WaitlistEntryWithCourse>(sqlx::AssertSqlSafe(format!(
        "SELECT {VIEW_COLUMNS} \
         FROM waitlist_entries w \
         {VIEW_JOINS} \
         WHERE w.user_id = $1 \
         ORDER BY w.created_at DESC"
    )))
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Waiting entries for a course, oldest first — the admin-facing queue
/// order (first-in, first-served).
pub async fn find_by_course_waiting(
    db: &PgPool,
    course_id: Uuid,
) -> Result<Vec<WaitlistEntryWithCourse>, sqlx::Error> {
    sqlx::query_as::<_, WaitlistEntryWithCourse>(sqlx::AssertSqlSafe(format!(
        "SELECT {VIEW_COLUMNS} \
         FROM waiting_entries w \
         {VIEW_JOINS} \
         WHERE w.course_id = $1 \
         ORDER BY w.created_at ASC"
    )))
    .bind(course_id)
    .fetch_all(db)
    .await
}
