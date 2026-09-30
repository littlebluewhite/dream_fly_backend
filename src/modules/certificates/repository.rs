use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use super::model::{CertificateRow, EnrolmentCourseCoach, ReportCardRow};

// ---------------------------------------------------------------------------
// report_cards
// ---------------------------------------------------------------------------

/// The report-card read projection's select list — single owner of
/// [`super::model::ReportCardRow`]'s shape. `rc` is the row source's alias,
/// real table or a data-modifying CTE (see [`insert_report_card`]) alike.
const REPORT_CARD_VIEW_COLUMNS: &str = "rc.id, e.course_id, c.name AS course_name, rc.term_label, \
    rc.comment, rc.rating, u.name AS created_by_name, rc.created_at";

/// [`REPORT_CARD_VIEW_COLUMNS`]'s matching JOIN clause — assumes a `rc` row
/// source already in scope (the `report_cards` table or a `WITH rc AS (...)`
/// CTE).
const REPORT_CARD_VIEW_JOINS: &str = "JOIN enrolments e ON e.id = rc.enrolment_id \
    JOIN courses c ON c.id = e.course_id JOIN users u ON u.id = rc.created_by";

/// The target enrolment's `course_id` + that course's `coach_id` — everything
/// `POST /report-cards`'s coach-ownership check needs. `None` if the
/// enrolment doesn't exist.
pub async fn find_enrolment_course_coach(
    db: &PgPool,
    enrolment_id: Uuid,
) -> Result<Option<EnrolmentCourseCoach>, sqlx::Error> {
    sqlx::query_as::<_, EnrolmentCourseCoach>(
        "SELECT e.course_id, c.coach_id \
         FROM enrolments e \
         JOIN courses c ON c.id = e.course_id \
         WHERE e.id = $1",
    )
    .bind(enrolment_id)
    .fetch_optional(db)
    .await
}

/// Insert a new `report_cards` row, returning its read projection row
/// directly — a data-modifying CTE (`WITH rc AS (INSERT ... RETURNING *)`)
/// feeds the insert's own output back through [`REPORT_CARD_VIEW_JOINS`], so
/// the caller gets the same shape [`find_my_report_cards`] would without a
/// second query. Duplicate `(enrolment_id, term_label)` trips the table's
/// UNIQUE constraint — `service` catches that as a 23505 and maps it to a
/// friendly 409 (the single statement writes zero rows).
pub async fn insert_report_card(
    db: &PgPool,
    enrolment_id: Uuid,
    term_label: &str,
    comment: Option<&str>,
    rating: Option<i16>,
    created_by: Uuid,
) -> Result<ReportCardRow, sqlx::Error> {
    sqlx::query_as::<_, ReportCardRow>(sqlx::AssertSqlSafe(format!(
        "WITH rc AS ( \
             INSERT INTO report_cards \
             (id, enrolment_id, term_label, comment, rating, created_by, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, NOW()) \
             RETURNING * \
         ) \
         SELECT {REPORT_CARD_VIEW_COLUMNS} FROM rc {REPORT_CARD_VIEW_JOINS}"
    )))
    .bind(Uuid::now_v7())
    .bind(enrolment_id)
    .bind(term_label)
    .bind(comment)
    .bind(rating)
    .bind(created_by)
    .fetch_one(db)
    .await
}

/// This user's report cards (via their enrolments), newest first — the
/// report-card read projection, one query, no N+1.
pub async fn find_my_report_cards(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<ReportCardRow>, sqlx::Error> {
    sqlx::query_as::<_, ReportCardRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {REPORT_CARD_VIEW_COLUMNS} \
         FROM report_cards rc \
         {REPORT_CARD_VIEW_JOINS} \
         WHERE e.user_id = $1 \
         ORDER BY rc.created_at DESC"
    )))
    .bind(user_id)
    .fetch_all(db)
    .await
}

// ---------------------------------------------------------------------------
// certificates
// ---------------------------------------------------------------------------

/// The certificate read projection's select list — single owner of
/// [`super::model::CertificateRow`]'s shape. `ce` is the row source's alias,
/// real table or a data-modifying CTE (see [`insert_certificate`]) alike.
const CERTIFICATE_VIEW_COLUMNS: &str = "ce.id, ce.course_id, c.name AS course_name, ce.title, \
    ce.level, ce.issued_on, ce.note, ce.created_at";

/// [`CERTIFICATE_VIEW_COLUMNS`]'s matching JOIN clause — `LEFT JOIN` because
/// a certificate need not be tied to a course (`course_id` nullable).
/// Assumes a `ce` row source already in scope.
const CERTIFICATE_VIEW_JOINS: &str = "LEFT JOIN courses c ON c.id = ce.course_id";

/// Whether `user_id` has ANY enrolment (active or cancelled — historical
/// students may still be certified, contract §3.22) in a course taught by
/// `coach_id`.
pub async fn user_has_enrolment_with_coach(
    db: &PgPool,
    user_id: Uuid,
    coach_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS ( \
            SELECT 1 FROM enrolments e \
            JOIN courses c ON c.id = e.course_id \
            WHERE e.user_id = $1 AND c.coach_id = $2 \
         )",
    )
    .bind(user_id)
    .bind(coach_id)
    .fetch_one(db)
    .await
}

/// Insert a new `certificates` row, returning its read projection row
/// directly via the same data-modifying CTE shape as [`insert_report_card`].
#[allow(clippy::too_many_arguments)]
pub async fn insert_certificate(
    db: &PgPool,
    user_id: Uuid,
    course_id: Option<Uuid>,
    title: &str,
    level: Option<&str>,
    issued_on: NaiveDate,
    issued_by: Uuid,
    note: Option<&str>,
) -> Result<CertificateRow, sqlx::Error> {
    sqlx::query_as::<_, CertificateRow>(sqlx::AssertSqlSafe(format!(
        "WITH ce AS ( \
             INSERT INTO certificates \
             (id, user_id, course_id, title, level, issued_on, issued_by, note, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW()) \
             RETURNING * \
         ) \
         SELECT {CERTIFICATE_VIEW_COLUMNS} FROM ce {CERTIFICATE_VIEW_JOINS}"
    )))
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(course_id)
    .bind(title)
    .bind(level)
    .bind(issued_on)
    .bind(issued_by)
    .bind(note)
    .fetch_one(db)
    .await
}

/// This user's certificates, newest first — the certificate read
/// projection, one query, no N+1.
pub async fn find_my_certificates(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<CertificateRow>, sqlx::Error> {
    sqlx::query_as::<_, CertificateRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {CERTIFICATE_VIEW_COLUMNS} \
         FROM certificates ce \
         {CERTIFICATE_VIEW_JOINS} \
         WHERE ce.user_id = $1 \
         ORDER BY ce.created_at DESC"
    )))
    .bind(user_id)
    .fetch_all(db)
    .await
}
