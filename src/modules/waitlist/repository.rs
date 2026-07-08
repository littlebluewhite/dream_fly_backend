use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::model::{WaitlistEntry, WaitlistEntryWithCourse};

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
        "SELECT EXISTS(SELECT 1 FROM waitlist_entries WHERE user_id = $1 AND course_id = $2 AND status = 'waiting')",
    )
    .bind(user_id)
    .bind(course_id)
    .fetch_one(db)
    .await
}

pub async fn insert(db: &PgPool, user_id: Uuid, course_id: Uuid) -> Result<WaitlistEntry, sqlx::Error> {
    sqlx::query_as::<_, WaitlistEntry>(
        "INSERT INTO waitlist_entries (id, user_id, course_id, status, created_at, updated_at) \
         VALUES ($1, $2, $3, 'waiting'::waitlist_status, NOW(), NOW()) \
         RETURNING id, user_id, course_id, status, created_at, updated_at",
    )
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

/// Conditional cancel, JOINed with `courses` so a response could be built
/// straight from the row this UPDATE produces. Returns `None` if the entry
/// was not `waiting` (i.e. already cancelled) — the service maps that to
/// 404, not 409 like enrolments, since a cancelled waitlist entry is no
/// longer addressable (re-joining is the supported way back in).
pub async fn cancel_if_waiting_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<WaitlistEntryWithCourse>, sqlx::Error> {
    sqlx::query_as::<_, WaitlistEntryWithCourse>(
        "UPDATE waitlist_entries w SET status = 'cancelled'::waitlist_status, updated_at = NOW() \
         FROM courses c \
         WHERE w.id = $1 AND w.course_id = c.id AND w.status = 'waiting'::waitlist_status \
         RETURNING w.id, w.course_id, c.name AS course_name, w.status, w.created_at",
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
    sqlx::query_as::<_, WaitlistEntryWithCourse>(
        "SELECT w.id, w.course_id, c.name AS course_name, w.status, w.created_at \
         FROM waitlist_entries w \
         JOIN courses c ON c.id = w.course_id \
         WHERE w.user_id = $1 \
         ORDER BY w.created_at DESC",
    )
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
    sqlx::query_as::<_, WaitlistEntryWithCourse>(
        "SELECT w.id, w.course_id, c.name AS course_name, w.status, w.created_at \
         FROM waitlist_entries w \
         JOIN courses c ON c.id = w.course_id \
         WHERE w.course_id = $1 AND w.status = 'waiting'::waitlist_status \
         ORDER BY w.created_at ASC",
    )
    .bind(course_id)
    .fetch_all(db)
    .await
}
