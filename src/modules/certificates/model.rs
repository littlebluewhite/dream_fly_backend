use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// report_cards
// ---------------------------------------------------------------------------

/// Bare `report_cards` table row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReportCard {
    pub id: Uuid,
    pub enrolment_id: Uuid,
    pub term_label: String,
    pub comment: Option<String>,
    pub rating: Option<i16>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

/// The target enrolment's `course_id` plus that course's `coach_id` —
/// everything `POST /report-cards`'s coach-ownership check needs (mirrors
/// `leave::model::SessionContext`'s narrow-context-for-authz shape). `None`
/// from the repository lookup means the enrolment doesn't exist.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EnrolmentCourseCoach {
    pub course_id: Uuid,
    pub coach_id: Option<Uuid>,
}

/// One `report_cards` row JOINed with its enrolment's course name and the
/// issuing user's name — the shape `GET /report-cards/me` and the
/// `POST /report-cards` response share (see `dto::ReportCardResponse`).
#[derive(Debug, sqlx::FromRow)]
pub struct ReportCardRow {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub term_label: String,
    pub comment: Option<String>,
    pub rating: Option<i16>,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// certificates
// ---------------------------------------------------------------------------

/// Bare `certificates` table row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Certificate {
    pub id: Uuid,
    pub user_id: Uuid,
    pub course_id: Option<Uuid>,
    pub title: String,
    pub level: Option<String>,
    pub issued_on: NaiveDate,
    pub issued_by: Uuid,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One `certificates` row JOINed with its (optional) course's name — the
/// shape `GET /certificates/me` and the `POST /certificates` response share
/// (see `dto::CertificateResponse`).
#[derive(Debug, sqlx::FromRow)]
pub struct CertificateRow {
    pub id: Uuid,
    pub course_id: Option<Uuid>,
    pub course_name: Option<String>,
    pub title: String,
    pub level: Option<String>,
    pub issued_on: NaiveDate,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}
