use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "inquiry_status", rename_all = "snake_case")]
pub enum InquiryStatus {
    New,
    InProgress,
    Resolved,
    Closed,
}

impl InquiryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::InProgress => "in_progress",
            Self::Resolved => "resolved",
            Self::Closed => "closed",
        }
    }
}

impl std::str::FromStr for InquiryStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "new" => Ok(Self::New),
            "in_progress" => Ok(Self::InProgress),
            "resolved" => Ok(Self::Resolved),
            "closed" => Ok(Self::Closed),
            _ => Err(()),
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct ContactInquiry {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub subject: String,
    pub message: String,
    pub status: InquiryStatus,
    pub assigned_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Round 4 Task B5 — trial-booking specialization. `general` (default)
    /// or `trial`, validated in the application layer (see
    /// `dto::validate_inquiry_type`), not a DB enum/CHECK.
    pub inquiry_type: String,
    /// Opaque JSONB payload for the trial-booking structured fields
    /// (category/student_age/preferred_day/preferred_slot/parent_name/
    /// parent_phone/student_name/note) — stored as-is, no per-field
    /// validation.
    pub metadata: Option<serde_json::Value>,
}
