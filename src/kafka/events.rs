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

/// One domain event's producer/consumer contract in a single place: the
/// topic it's published on, the `event_type` string in its envelope, and
/// the `(resource, id_field)` pair the audit consumer uses to populate
/// `audit_log.resource` / `resource_id`. This is the single source of truth
/// both [`DomainEvent`] impls (producer side) and
/// [`crate::kafka::consumer::domain_resource`] (consumer side) read from,
/// replacing what used to be two independently hand-maintained mappings.
#[derive(Debug)]
pub struct EventSpec {
    pub topic: &'static str,
    pub event_type: &'static str,
    pub resource: &'static str,
    pub id_field: &'static str,
}

pub const ORDER_CREATED_SPEC: EventSpec = EventSpec {
    topic: topics::ORDERS_CREATED,
    event_type: event_types::ORDER_CREATED,
    resource: "order",
    id_field: "order_id",
};

pub const ORDER_STATUS_CHANGED_SPEC: EventSpec = EventSpec {
    topic: topics::ORDERS_STATUS_CHANGED,
    event_type: event_types::ORDER_STATUS_CHANGED,
    resource: "order",
    id_field: "order_id",
};

pub const BOOKING_CREATED_SPEC: EventSpec = EventSpec {
    topic: topics::BOOKINGS_CREATED,
    event_type: event_types::BOOKING_CREATED,
    resource: "booking",
    id_field: "booking_id",
};

pub const BOOKING_CANCELLED_SPEC: EventSpec = EventSpec {
    topic: topics::BOOKINGS_CANCELLED,
    event_type: event_types::BOOKING_CANCELLED,
    resource: "booking",
    id_field: "booking_id",
};

pub const USER_REGISTERED_SPEC: EventSpec = EventSpec {
    topic: topics::USERS_REGISTERED,
    event_type: event_types::USER_REGISTERED,
    resource: "user",
    id_field: "user_id",
};

/// Every known domain `EventSpec`. Consumers derive their subscription
/// topic list from this instead of hand-writing a second topic array (see
/// `kafka::consumer::start_consumer`).
pub const ALL_SPECS: [&EventSpec; 5] = [
    &ORDER_CREATED_SPEC,
    &ORDER_STATUS_CHANGED_SPEC,
    &BOOKING_CREATED_SPEC,
    &BOOKING_CANCELLED_SPEC,
    &USER_REGISTERED_SPEC,
];

/// Look up the `EventSpec` for a given envelope `event_type`, e.g.
/// `"order_created"`. Returns `None` for anything not in [`ALL_SPECS`]
/// (hand-authored audit events, or a not-yet-modeled domain event type).
pub fn spec_for_event_type(event_type: &str) -> Option<&'static EventSpec> {
    ALL_SPECS
        .iter()
        .copied()
        .find(|s| s.event_type == event_type)
}

/// A payload type that knows its own outbox routing. Implementing this for
/// a payload struct is what lets [`crate::kafka::outbox::insert_domain_event_tx`]
/// derive `topic`/`event_type` from `SPEC` and the partition key from
/// [`kafka_key`](DomainEvent::kafka_key), instead of the caller repeating
/// them by hand at every publish site.
pub trait DomainEvent: Serialize {
    const SPEC: &'static EventSpec;

    /// The Kafka partition key for this event — matches what each publish
    /// site used before this trait existed (the affected resource's own id
    /// for order/booking events, `user_id` for user events).
    fn kafka_key(&self) -> String;
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

impl DomainEvent for OrderCreatedPayload {
    const SPEC: &'static EventSpec = &ORDER_CREATED_SPEC;

    fn kafka_key(&self) -> String {
        self.order_id.to_string()
    }
}

#[derive(Debug, Serialize)]
pub struct OrderStatusChangedPayload {
    pub order_id: Uuid,
    pub user_id: Uuid,
    pub status: String,
}

impl DomainEvent for OrderStatusChangedPayload {
    const SPEC: &'static EventSpec = &ORDER_STATUS_CHANGED_SPEC;

    fn kafka_key(&self) -> String {
        self.order_id.to_string()
    }
}

#[derive(Debug, Serialize)]
pub struct BookingCreatedPayload {
    pub booking_id: Uuid,
    pub user_id: Uuid,
    pub time_slot_id: Uuid,
}

impl DomainEvent for BookingCreatedPayload {
    const SPEC: &'static EventSpec = &BOOKING_CREATED_SPEC;

    fn kafka_key(&self) -> String {
        self.booking_id.to_string()
    }
}

#[derive(Debug, Serialize)]
pub struct BookingCancelledPayload {
    pub booking_id: Uuid,
    pub user_id: Uuid,
    pub time_slot_id: Uuid,
}

impl DomainEvent for BookingCancelledPayload {
    const SPEC: &'static EventSpec = &BOOKING_CANCELLED_SPEC;

    fn kafka_key(&self) -> String {
        self.booking_id.to_string()
    }
}

#[derive(Debug, Serialize)]
pub struct UserRegisteredPayload {
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
}

impl DomainEvent for UserRegisteredPayload {
    const SPEC: &'static EventSpec = &USER_REGISTERED_SPEC;

    fn kafka_key(&self) -> String {
        self.user_id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table-driven check of the 5 specs' event_type → (topic, resource,
    /// id_field) mapping, and that `spec_for_event_type` finds each one by
    /// its `event_type` string. No DB involved.
    #[test]
    fn all_specs_map_event_type_to_topic_and_resource() {
        let cases: [(&EventSpec, &str, &str, &str, &str); 5] = [
            (
                &ORDER_CREATED_SPEC,
                event_types::ORDER_CREATED,
                topics::ORDERS_CREATED,
                "order",
                "order_id",
            ),
            (
                &ORDER_STATUS_CHANGED_SPEC,
                event_types::ORDER_STATUS_CHANGED,
                topics::ORDERS_STATUS_CHANGED,
                "order",
                "order_id",
            ),
            (
                &BOOKING_CREATED_SPEC,
                event_types::BOOKING_CREATED,
                topics::BOOKINGS_CREATED,
                "booking",
                "booking_id",
            ),
            (
                &BOOKING_CANCELLED_SPEC,
                event_types::BOOKING_CANCELLED,
                topics::BOOKINGS_CANCELLED,
                "booking",
                "booking_id",
            ),
            (
                &USER_REGISTERED_SPEC,
                event_types::USER_REGISTERED,
                topics::USERS_REGISTERED,
                "user",
                "user_id",
            ),
        ];

        for (spec, event_type, topic, resource, id_field) in cases {
            assert_eq!(spec.event_type, event_type);
            assert_eq!(spec.topic, topic);
            assert_eq!(spec.resource, resource);
            assert_eq!(spec.id_field, id_field);

            let looked_up = spec_for_event_type(event_type)
                .unwrap_or_else(|| panic!("spec_for_event_type must find {event_type}"));
            assert_eq!(looked_up.topic, topic);
            assert_eq!(looked_up.resource, resource);
            assert_eq!(looked_up.id_field, id_field);
        }

        assert_eq!(ALL_SPECS.len(), 5);
        assert!(spec_for_event_type("order_refunded").is_none());
    }

    /// Table-driven check that each payload's `kafka_key()` matches what
    /// every publish site used before `DomainEvent` existed: the affected
    /// resource's own id for order/booking events, `user_id` for user
    /// events.
    #[test]
    fn each_payload_kafka_key_matches_pre_domain_event_convention() {
        let order_id = Uuid::now_v7();
        let order_created = OrderCreatedPayload {
            order_id,
            user_id: Uuid::now_v7(),
            order_number: "ORD-1".into(),
            total_cents: 100,
            discount_cents: 0,
            coupon_code: None,
            points_used: 0,
            points_earned: 0,
        };
        assert_eq!(order_created.kafka_key(), order_id.to_string());

        let order_status_changed = OrderStatusChangedPayload {
            order_id,
            user_id: Uuid::now_v7(),
            status: "paid".into(),
        };
        assert_eq!(order_status_changed.kafka_key(), order_id.to_string());

        let booking_id = Uuid::now_v7();
        let booking_created = BookingCreatedPayload {
            booking_id,
            user_id: Uuid::now_v7(),
            time_slot_id: Uuid::now_v7(),
        };
        assert_eq!(booking_created.kafka_key(), booking_id.to_string());

        let booking_cancelled = BookingCancelledPayload {
            booking_id,
            user_id: Uuid::now_v7(),
            time_slot_id: Uuid::now_v7(),
        };
        assert_eq!(booking_cancelled.kafka_key(), booking_id.to_string());

        let user_id = Uuid::now_v7();
        let user_registered = UserRegisteredPayload {
            user_id,
            email: "a@example.com".into(),
            name: "A".into(),
        };
        assert_eq!(user_registered.kafka_key(), user_id.to_string());
    }
}
