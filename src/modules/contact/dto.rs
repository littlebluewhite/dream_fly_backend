use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;
use validator::{Validate, ValidationError};

use crate::extractors::pagination::PageMeta;

use super::model::ContactInquiry;

#[derive(Debug, Serialize)]
pub struct InquiryResponse {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub subject: String,
    pub message: String,
    pub status: String,
    pub assigned_to: Option<Uuid>,
    pub inquiry_type: String,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<ContactInquiry> for InquiryResponse {
    fn from(i: ContactInquiry) -> Self {
        Self {
            id: i.id,
            name: i.name,
            email: i.email,
            phone: i.phone,
            subject: i.subject,
            message: i.message,
            status: i.status.as_str().to_string(),
            assigned_to: i.assigned_to,
            inquiry_type: i.inquiry_type,
            metadata: i.metadata,
            created_at: i.created_at,
            updated_at: i.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InquiryListResponse {
    pub inquiries: Vec<InquiryResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}

fn default_inquiry_type() -> String {
    "general".to_string()
}

/// Only `general` (default) and `trial` are supported today — the
/// try-a-class booking flow (mobile TrialScreen, Round 4 Task B5) piggybacks
/// on the contact-inquiry table rather than a dedicated booking (see
/// docs/api/integration-contract.md §3.17). Enforced in the application
/// layer only — no DB CHECK/enum, matching this feature's migration.
fn validate_inquiry_type(v: &str) -> Result<(), ValidationError> {
    if v == "general" || v == "trial" {
        Ok(())
    } else {
        Err(ValidationError::new("invalid_inquiry_type"))
    }
}

#[derive(Debug, Deserialize, Validate)]
pub struct CreateInquiryRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    #[validate(email)]
    pub email: String,
    #[validate(length(max = 20))]
    pub phone: Option<String>,
    #[validate(length(min = 1, max = 200))]
    pub subject: String,
    #[validate(length(min = 1, max = 5000))]
    pub message: String,
    #[serde(default = "default_inquiry_type")]
    #[validate(custom(function = "validate_inquiry_type"))]
    pub inquiry_type: String,
    /// Opaque JSONB payload — the trial-booking structured fields
    /// (category/student_age/preferred_day/preferred_slot/parent_name/
    /// parent_phone/student_name/note) are assembled by the frontend and
    /// stored as-is; the backend does not validate individual keys.
    pub metadata: Option<serde_json::Value>,
}

/// Plain `Option<Option<T>>` cannot distinguish "key absent" from "key
/// present with JSON `null`" — serde's built-in `Option<T>` deserialize
/// collapses a `null` straight to the *outer* `None`, so a bare
/// `Option<Option<T>>` field could never actually clear a nullable column
/// back to `NULL` via PATCH. Paired with `#[serde(default)]`, this makes the
/// present-with-`null` case reach the *inner* `Option`, producing
/// `Some(None)` (clear) instead of `None` (don't touch) — mirrors
/// `venues::dto::deserialize_some` (venues d91ad85).
fn deserialize_some<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Partial update payload for `PATCH /contact/inquiries/{id}` (admin-only
/// follow-up, Round 4 Task B5). `status` is validated against
/// `InquiryStatus` in `service::update_inquiry` (422 on an unrecognized
/// value) — mirrors `courses::service::create_course`'s `level` parsing /
/// `leave::service`'s status parsing, not a DTO-level `#[validate]`.
/// `assigned_to` uses `Option<Option<Uuid>>` (paired with
/// `deserialize_some`) so callers can distinguish "don't touch" (`None`),
/// "unassign" (`Some(None)`), and "assign to this user" (`Some(Some(id))`).
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateInquiryRequest {
    pub status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_some")]
    pub assigned_to: Option<Option<Uuid>>,
}
