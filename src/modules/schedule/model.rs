use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// A time slot's read-time-derived booking status — see [`SlotStatus::derive`].
/// Not a database column and not a state machine: `time_slots` no longer has
/// a status column or backing SQL enum type (dropped by migration
/// `20260717000001_time_slots_status_read_time_derive`) — every read
/// recomputes this from `booked`/`capacity`/`is_closed`, so it can never go
/// stale the way the old stored CASE-expression status could (anything that
/// touched `booked` without going through `increment_booked_tx`/
/// `decrement_booked_tx` left the stored status out of sync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    Available,
    Limited,
    Full,
    Closed,
}

impl SlotStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Limited => "limited",
            Self::Full => "full",
            Self::Closed => "closed",
        }
    }

    /// Derive a slot's booking status from current facts. `is_closed` (an
    /// admin intent flag set via `PATCH /schedule/slots/{id}`) takes
    /// priority over the booked/capacity computation — an admin-closed slot
    /// reads as `closed` even if it technically still has open seats.
    ///
    /// The `limited` threshold is integer arithmetic mirroring the SQL this
    /// replaces (`(capacity * 0.8)::int`, which *rounds* rather than
    /// truncates): `((capacity as i64) * 8 + 5) / 10` is the standard
    /// round-half-away-from-zero trick for `round(capacity * 0.8)` done in
    /// integer math, so it agrees with the old SQL at every capacity —
    /// including ones not divisible by 5 (e.g. 3 → 2, 4 → 3, 7 → 6), where a
    /// naive `booked as f64 >= capacity as f64 * 0.8` float comparison would
    /// disagree. Widened to `i64` before multiplying by 8 so a `capacity`
    /// near `i32::MAX` can't overflow the multiplication —
    /// `schedule::service::MAX_SLOT_CAPACITY` (10,000) keeps real callers
    /// far below that, but the database only rejects negative capacity, so
    /// this pure function defends the boundary independently.
    pub fn derive(booked: i32, capacity: i32, is_closed: bool) -> SlotStatus {
        if is_closed {
            return SlotStatus::Closed;
        }
        if booked >= capacity {
            return SlotStatus::Full;
        }
        let limited_threshold = ((capacity as i64) * 8 + 5) / 10;
        if (booked as i64) >= limited_threshold {
            SlotStatus::Limited
        } else {
            SlotStatus::Available
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct TimeSlot {
    pub id: Uuid,
    pub date: NaiveDate,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
    pub venue_id: Option<Uuid>,
    pub course_id: Option<Uuid>,
    pub capacity: i32,
    pub booked: i32,
    /// Admin intent flag — see [`SlotStatus::derive`]. Replaces the old
    /// `status = 'closed'` variant; the booked/capacity-driven states
    /// (`available`/`limited`/`full`) are handled purely by the derive
    /// function and were never stored as their own source of truth.
    pub is_closed: bool,
    /// Round 4 Task P4-B2 (`time_slots.price_cents`, migration
    /// `20260708000006`). Venue-rental price for this slot; `bookings`
    /// snapshots this value at booking time (see `bookings::model::Booking`).
    pub price_cents: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SlotStatus::derive table (仿 courses::seats::remaining_table 款式) ---

    #[test]
    fn derive_table() {
        // (booked, capacity, is_closed) → expected
        let cases: [(i32, i32, bool, SlotStatus); 12] = [
            // 0 booked,遠低於門檻 → available
            (0, 10, false, SlotStatus::Available),
            // 門檻邊界,非 5 倍數容量——SQL `(capacity * 0.8)::int` 是四捨
            // 五入,不是截斷(`>= capacity as f64 * 0.8` 在這幾組會算錯):
            // cap=3 → threshold 2(3*0.8=2.4 round → 2)
            (1, 3, false, SlotStatus::Available),
            (2, 3, false, SlotStatus::Limited),
            // cap=4 → threshold 3(4*0.8=3.2 round → 3)
            (2, 4, false, SlotStatus::Available),
            (3, 4, false, SlotStatus::Limited),
            // cap=7 → threshold 6(7*0.8=5.6 round → 6,非截斷會得到的 5)
            (5, 7, false, SlotStatus::Available),
            (6, 7, false, SlotStatus::Limited),
            // 滿:booked == capacity
            (7, 7, false, SlotStatus::Full),
            (10, 10, false, SlotStatus::Full),
            // closed 優先於 full/limited/available 的判斷
            (0, 10, true, SlotStatus::Closed),
            (6, 7, true, SlotStatus::Closed),
            (10, 10, true, SlotStatus::Closed),
        ];
        for (booked, capacity, is_closed, expected) in cases {
            assert_eq!(
                SlotStatus::derive(booked, capacity, is_closed),
                expected,
                "(booked={booked}, capacity={capacity}, is_closed={is_closed})"
            );
        }
    }
}
