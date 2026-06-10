use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

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
            created_at: i.created_at,
            updated_at: i.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InquiryListResponse {
    pub inquiries: Vec<InquiryResponse>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
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
}
