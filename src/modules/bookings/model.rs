use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "booking_status", rename_all = "snake_case")]
pub enum BookingStatus {
    Pending,
    Confirmed,
    Cancelled,
    Completed,
    NoShow,
}

impl BookingStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
            Self::NoShow => "no_show",
        }
    }

    /// True iff the booking is in a state that can still be cancelled by
    /// the user or an admin. Any "terminal" state (already cancelled,
    /// completed, or no-show) is explicitly rejected — cancelling a
    /// completed booking must never decrement `time_slots.booked` again.
    pub fn is_cancellable(&self) -> bool {
        matches!(self, Self::Pending | Self::Confirmed)
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Booking {
    pub id: Uuid,
    pub user_id: Uuid,
    pub time_slot_id: Uuid,
    pub status: BookingStatus,
    pub note: Option<String>,
    /// Round 4 Task P4-B2 (`bookings.price_cents`, migration
    /// `20260708000006`). Snapshot of `time_slots.price_cents` at the
    /// moment this booking was created — a later price change on the slot
    /// must NOT retroactively change this value, and cancelling a booking
    /// must NOT clear/zero it either (report aggregation filters by
    /// `status`, not by this column).
    pub price_cents: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 場租計收的 booking 狀態(Round 4 Phase 4 口徑,ADR-0004):場租計收 =
/// status ∈ confirmed/completed 的 bookings 之 `price_cents` 快照,歸屬 slot
/// 使用日(非下訂日);`pending`/`cancelled`/`no_show` 一律不入。「哪些狀態算
/// 場租營收」的單一歸屬點——改這裡,報表跟著變。與 `orders::model::
/// REVENUE_STATUSES` 是不同狀態機的各自口徑,刻意不共用。
pub const VENUE_REVENUE_STATUSES: [&str; 2] = ["confirmed", "completed"];
