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

    /// Single owner of the "booked = 非 cancelled bookings 數" invariant:
    /// whether this booking currently occupies a seat on its time slot.
    /// `time_slots.booked` is a denormalized read cache of this predicate
    /// (maintained at runtime by the increment/decrement protocol on
    /// create/cancel); `src/bin/seed.rs` consumes this same predicate
    /// instead of hand-picking which literal statuses count as occupied.
    /// `Completed`/`NoShow` are terminal but still occupy — only
    /// `Cancelled` frees the seat.
    pub fn occupies_seat(&self) -> bool {
        match self {
            Self::Pending | Self::Confirmed | Self::Completed | Self::NoShow => true,
            Self::Cancelled => false,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venue_revenue_statuses_track_booking_status_variants() {
        // 手列全部 5 個變體(repo 無 EnumIter,不為此加依賴)。
        let all_statuses = [
            BookingStatus::Pending,
            BookingStatus::Confirmed,
            BookingStatus::Cancelled,
            BookingStatus::Completed,
            BookingStatus::NoShow,
        ];
        for status in all_statuses {
            // Tripwire:窮盡 match、無 `_` arm。新增 BookingStatus 變體時
            // 本行編譯錯誤,擋住上面手列的 5 變體清單悄悄過期(同
            // orders::model 的 ALL_STATUSES 手法)。
            match status {
                BookingStatus::Pending
                | BookingStatus::Confirmed
                | BookingStatus::Cancelled
                | BookingStatus::Completed
                | BookingStatus::NoShow => {}
            }
            // 每個常數字串都必須對應到某個變體的 as_str(),且該集合恰為
            // Confirmed/Completed——本系統沒有 is_venue_revenue() 謂詞,
            // 這裡直接與字面上的 Confirmed|Completed 比對。
            assert_eq!(
                VENUE_REVENUE_STATUSES.contains(&status.as_str()),
                matches!(status, BookingStatus::Confirmed | BookingStatus::Completed),
                "{status:?}: VENUE_REVENUE_STATUSES membership should be exactly Confirmed|Completed"
            );
        }
        // 長度斷言:逐變體比對防不了 VENUE_REVENUE_STATUSES 裡混進重複或
        // 不對應任何變體的字串——兩邊集合大小相等,才真正排除這個殘餘可
        // 能性。
        assert_eq!(VENUE_REVENUE_STATUSES.len(), 2);
    }

    // --- occupies_seat / is_cancellable table (仿 schedule::model::derive_table 款式) ---

    #[test]
    fn occupies_seat_table() {
        // (status, occupies_seat, is_cancellable)
        let cases: [(BookingStatus, bool, bool); 5] = [
            (BookingStatus::Pending, true, true),
            (BookingStatus::Confirmed, true, true),
            // 唯一釋出座位的狀態。
            (BookingStatus::Cancelled, false, false),
            // 終局狀態:佔位但不可再取消。
            (BookingStatus::Completed, true, false),
            (BookingStatus::NoShow, true, false),
        ];
        for (status, occupies_seat, is_cancellable) in cases {
            assert_eq!(
                status.occupies_seat(),
                occupies_seat,
                "{status:?}.occupies_seat()"
            );
            assert_eq!(
                status.is_cancellable(),
                is_cancellable,
                "{status:?}.is_cancellable()"
            );
        }
    }
}
