use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Canonical topic names. Kept in one place so producers and consumers can
/// reference the same constants.
pub mod topics {
    pub const ORDERS_CREATED: &str = "dreamfly.orders.created";
    pub const ORDERS_STATUS_CHANGED: &str = "dreamfly.orders.status_changed";
    pub const BOOKINGS_CREATED: &str = "dreamfly.bookings.created";
    pub const BOOKINGS_CANCELLED: &str = "dreamfly.bookings.cancelled";
    pub const USERS_REGISTERED: &str = "dreamfly.users.registered";
    pub const AUDIT_LOG: &str = "dreamfly.audit.log";
}

/// Canonical event type strings that appear in the envelope `event_type`
/// field. Kept as constants so producers and downstream consumers agree.
pub mod event_types {
    pub const ORDER_CREATED: &str = "order_created";
    pub const ORDER_STATUS_CHANGED: &str = "order_status_changed";
    pub const BOOKING_CREATED: &str = "booking_created";
    pub const BOOKING_CANCELLED: &str = "booking_cancelled";
    pub const USER_REGISTERED: &str = "user_registered";
}

/// Current envelope version. Bump when the envelope shape changes in a way
/// consumers need to dispatch on (e.g. renaming `data` or adding required
/// fields). Payload shape changes — adding fields to `OrderCreatedPayload`
/// etc. — are additive and do NOT require a version bump because every
/// consumer reads payload fields defensively.
pub const CURRENT_ENVELOPE_VERSION: u32 = 1;

/// Outer envelope shared by every Kafka event emitted by this service.
///
/// - `version` — envelope schema version, see [`CURRENT_ENVELOPE_VERSION`]
/// - `event_id` — unique per event (UUID v7 so it sorts by time)
/// - `event_type` — machine-readable kind (see [`event_types`])
/// - `timestamp` — when the event was produced (producer wall clock)
/// - `correlation_id` — originating request-id (opaque string), so
///   consumer-side logs can be tied back to the HTTP request that caused
///   this event. Optional because internal background tasks (e.g. scheduled
///   jobs) don't have an incoming request to tag.
/// - `data` — the domain-specific payload
#[derive(Debug, Serialize)]
pub struct KafkaEvent<T: Serialize> {
    pub version: u32,
    pub event_id: Uuid,
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub data: T,
}

impl<T: Serialize> KafkaEvent<T> {
    pub fn new(event_type: impl Into<String>, data: T) -> Self {
        Self {
            version: CURRENT_ENVELOPE_VERSION,
            event_id: Uuid::now_v7(),
            event_type: event_type.into(),
            timestamp: Utc::now(),
            correlation_id: None,
            data,
        }
    }

    /// Attach a correlation id (typically the `x-request-id` header value
    /// from the HTTP request that triggered this event). Chainable.
    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Payload types
// ---------------------------------------------------------------------------
//
// These are intentionally decoupled from API response DTOs so that changing
// an HTTP response shape does not silently change the wire format that other
// services consume.

#[derive(Debug, Serialize)]
pub struct OrderCreatedPayload {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub order_number: String,
    pub total_cents: i64,
    pub discount_cents: i64,
    pub coupon_code: Option<String>,
    pub points_used: i64,
    pub points_earned: i64,
}

#[derive(Debug, Serialize)]
pub struct OrderStatusChangedPayload {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct BookingCreatedPayload {
    pub booking_id: Uuid,
    pub user_id: Uuid,
    pub time_slot_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct BookingCancelledPayload {
    pub booking_id: Uuid,
    pub user_id: Uuid,
    pub time_slot_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct UserRegisteredPayload {
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
}
