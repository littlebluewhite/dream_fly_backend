use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

use crate::extractors::pagination::PageMeta;

use super::model::Booking;

#[derive(Debug, Deserialize, Validate)]
pub struct CreateBookingRequest {
    pub time_slot_id: Uuid,
    #[validate(length(max = 500))]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BookingResponse {
    pub id: Uuid,
    pub user_id: Uuid,
    pub time_slot_id: Uuid,
    pub status: String,
    pub note: Option<String>,
    /// Round 4 Task P4-B2 — price snapshot captured at booking time (§1.5).
    pub price_cents: i64,
    pub created_at: DateTime<Utc>,
}

impl From<Booking> for BookingResponse {
    fn from(b: Booking) -> Self {
        Self {
            id: b.id,
            user_id: b.user_id,
            time_slot_id: b.time_slot_id,
            status: b.status.as_str().to_string(),
            note: b.note,
            price_cents: b.price_cents,
            created_at: b.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PaginatedBookingsResponse {
    pub bookings: Vec<BookingResponse>,
    #[serde(flatten)]
    pub meta: PageMeta,
}
