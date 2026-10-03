use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ts_rs::TS)]
#[sqlx(type_name = "inquiry_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum InquiryStatus {
    New,
    InProgress,
    Resolved,
    Closed,
}

impl InquiryStatus {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` and the allowed-values text both derive from this
    /// instead of hand-copying the list.
    pub const ALL: [Self; 4] = [Self::New, Self::InProgress, Self::Resolved, Self::Closed];

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
        // 既有 wire 政策：大小寫不敏感，先轉小寫再比對。
        let s = s.to_lowercase();
        Self::ALL.into_iter().find(|v| v.as_str() == s).ok_or(())
    }
}

/// Round 4 Task B5's trial-booking value set — application-layer only, no
/// DB enum/CHECK (`contact_inquiries.inquiry_type` stays a bare `TEXT`
/// column; see `ContactInquiry::inquiry_type`'s own doc). Mirrors
/// [`InquiryStatus`]'s `as_str`/`FromStr` shape, with one deliberate
/// deviation: `FromStr` here does NOT lowercase before matching — the
/// existing validation this replaces was case-sensitive (only the exact
/// strings `"general"`/`"trial"` are accepted, per
/// docs/api/integration-contract.md §3.17), and this refactor preserves
/// that behavior rather than silently loosening it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum InquiryType {
    General,
    Trial,
}

impl InquiryType {
    /// Every variant, in wire-spelling order — single owner of the value
    /// domain; `FromStr` derives from this instead of hand-copying the list.
    pub const ALL: [Self; 2] = [Self::General, Self::Trial];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Trial => "trial",
        }
    }
}

impl std::str::FromStr for InquiryType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|v| v.as_str() == s).ok_or(())
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
