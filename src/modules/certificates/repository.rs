use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use super::model::{Certificate, CertificateRow, EnrolmentCourseCoach, ReportCard, ReportCardRow};

// ---------------------------------------------------------------------------
// report_cards
// ---------------------------------------------------------------------------

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

/// Insert a new `report_cards` row. Duplicate `(enrolment_id, term_label)`
/// trips the table's UNIQUE constraint — `service` catches that as a 23505
/// and maps it to a friendly 409.
pub async fn insert_report_card(
    db: &PgPool,
    enrolment_id: Uuid,
    term_label: &str,
    comment: Option<&str>,
    rating: Option<i16>,
    created_by: Uuid,
) -> Result<ReportCard, sqlx::Error> {
    sqlx::query_as::<_, ReportCard>(
        "INSERT INTO report_cards \
         (id, enrolment_id, term_label, comment, rating, created_by, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, NOW()) \
         RETURNING id, enrolment_id, term_label, comment, rating, created_by, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(enrolment_id)
    .bind(term_label)
    .bind(comment)
    .bind(rating)
    .bind(created_by)
    .fetch_one(db)
    .await
}

/// One `report_cards` row JOINed with its enrolment's course name and the
/// issuing user's name — used to build the `POST /report-cards` response
/// right after insert.
pub async fn find_report_card_row(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<ReportCardRow>, sqlx::Error> {
    sqlx::query_as::<_, ReportCardRow>(
        "SELECT rc.id, e.course_id, c.name AS course_name, rc.term_label, rc.comment, \
                rc.rating, u.name AS created_by_name, rc.created_at \
         FROM report_cards rc \
         JOIN enrolments e ON e.id = rc.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         JOIN users u ON u.id = rc.created_by \
         WHERE rc.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// This user's report cards (via their enrolments), newest first — same
/// joined shape as [`find_report_card_row`], one query, no N+1.
pub async fn find_my_report_cards(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<ReportCardRow>, sqlx::Error> {
    sqlx::query_as::<_, ReportCardRow>(
        "SELECT rc.id, e.course_id, c.name AS course_name, rc.term_label, rc.comment, \
                rc.rating, u.name AS created_by_name, rc.created_at \
         FROM report_cards rc \
         JOIN enrolments e ON e.id = rc.enrolment_id \
         JOIN courses c ON c.id = e.course_id \
         JOIN users u ON u.id = rc.created_by \
         WHERE e.user_id = $1 \
         ORDER BY rc.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

// ---------------------------------------------------------------------------
// certificates
// ---------------------------------------------------------------------------

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

/// Insert a new `certificates` row.
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
) -> Result<Certificate, sqlx::Error> {
    sqlx::query_as::<_, Certificate>(
        "INSERT INTO certificates \
         (id, user_id, course_id, title, level, issued_on, issued_by, note, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW()) \
         RETURNING id, user_id, course_id, title, level, issued_on, issued_by, note, created_at",
    )
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

/// One `certificates` row JOINed with its (optional) course's name — used to
/// build the `POST /certificates` response right after insert.
pub async fn find_certificate_row(
    db: &PgPool,
    id: Uuid,
) -> Result<Option<CertificateRow>, sqlx::Error> {
    sqlx::query_as::<_, CertificateRow>(
        "SELECT ce.id, ce.course_id, c.name AS course_name, ce.title, ce.level, \
                ce.issued_on, ce.note, ce.created_at \
         FROM certificates ce \
         LEFT JOIN courses c ON c.id = ce.course_id \
         WHERE ce.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// This user's certificates, newest first — same joined shape as
/// [`find_certificate_row`], one query, no N+1.
pub async fn find_my_certificates(
    db: &PgPool,
    user_id: Uuid,
) -> Result<Vec<CertificateRow>, sqlx::Error> {
    sqlx::query_as::<_, CertificateRow>(
        "SELECT ce.id, ce.course_id, c.name AS course_name, ce.title, ce.level, \
                ce.issued_on, ce.note, ce.created_at \
         FROM certificates ce \
         LEFT JOIN courses c ON c.id = ce.course_id \
         WHERE ce.user_id = $1 \
         ORDER BY ce.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}
