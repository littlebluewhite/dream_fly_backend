use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use super::model::{CertificateRow, ReportCardRow};

// ---------------------------------------------------------------------------
// POST /report-cards, GET /report-cards/me
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct CreateReportCardRequest {
    pub enrolment_id: Uuid,
    #[validate(length(min = 1, max = 100))]
    pub term_label: String,
    pub comment: Option<String>,
    #[validate(range(min = 1, max = 5))]
    pub rating: Option<i16>,
}

#[derive(Debug, Serialize)]
pub struct ReportCardResponse {
    pub id: Uuid,
    pub course_id: Uuid,
    pub course_name: String,
    pub term_label: String,
    pub comment: Option<String>,
    pub rating: Option<i16>,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
}

impl From<ReportCardRow> for ReportCardResponse {
    fn from(r: ReportCardRow) -> Self {
        Self {
            id: r.id,
            course_id: r.course_id,
            course_name: r.course_name,
            term_label: r.term_label,
            comment: r.comment,
            rating: r.rating,
            created_by_name: r.created_by_name,
            created_at: r.created_at,
        }
    }
}

// ---------------------------------------------------------------------------
// POST /certificates, GET /certificates/me
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Validate)]
pub struct CreateCertificateRequest {
    pub user_id: Uuid,
    pub course_id: Option<Uuid>,
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    #[validate(length(max = 100))]
    pub level: Option<String>,
    pub issued_on: NaiveDate,
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CertificateResponse {
    pub id: Uuid,
    pub course_id: Option<Uuid>,
    pub course_name: Option<String>,
    pub title: String,
    pub level: Option<String>,
    pub issued_on: NaiveDate,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<CertificateRow> for CertificateResponse {
    fn from(r: CertificateRow) -> Self {
        Self {
            id: r.id,
            course_id: r.course_id,
            course_name: r.course_name,
            title: r.title,
            level: r.level,
            issued_on: r.issued_on,
            note: r.note,
            created_at: r.created_at,
        }
    }
}
