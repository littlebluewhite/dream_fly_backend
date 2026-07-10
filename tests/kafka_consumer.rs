//! Integration tests for `kafka::consumer::handle_audit_event`.
//!
//! These exercise the handler directly (no Kafka broker involved) with
//! hand-built envelope payloads — either the real producer envelope types
//! serialized the same way the outbox dispatcher does, or raw JSON for
//! shapes the producer never emits (malformed/missing fields, unmapped
//! event types).
//!
//! Covered paths:
//! - domain resource mapping: order_created / booking_cancelled /
//!   user_registered each land the right `resource` / `resource_id`
//! - consuming the same `event_id` twice writes exactly one row (idempotency)
//! - missing `event_type` / malformed JSON both classify as `Poison` and
//!   write nothing
//! - an event_type outside the known domain families falls back to
//!   `data.resource` (or `"audit"` when absent) — the original
//!   `AUDIT_LOG`-topic behavior
//! - a missing `event_id` still writes successfully (fallback id)

mod common;

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::kafka::consumer::{ProcessingError, handle_audit_event};
use dream_fly_backend::kafka::events::{
    BookingCancelledPayload, BookingCreatedPayload, KafkaEvent, OrderCreatedPayload,
    UserRegisteredPayload, event_types,
};

/// Read back `(user_id, action, resource, resource_id)` for a single row.
async fn audit_row(db: &PgPool, id: Uuid) -> (Option<Uuid>, String, String, Option<Uuid>) {
    sqlx::query_as("SELECT user_id, action, resource, resource_id FROM audit_log WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .expect("audit_log row for event")
}

async fn audit_log_count(db: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(db)
        .await
        .expect("count audit_log")
}

// ---------------------------------------------------------------------------
// Resource mapping (3 cases)
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn order_created_maps_to_order_resource(db: PgPool) {
    let user_id = common::seed_member(&db, "order-map@example.com", "password123").await;
    let order_id = Uuid::now_v7();

    let envelope = KafkaEvent::new(
        event_types::ORDER_CREATED,
        OrderCreatedPayload {
            order_id,
            user_id,
            order_number: "ORD-1001".into(),
            total_cents: 5000,
            discount_cents: 0,
            coupon_code: None,
            points_used: 0,
            points_earned: 50,
        },
    );
    let event_id = envelope.event_id;
    let payload = serde_json::to_string(&envelope).expect("serialize envelope");

    handle_audit_event(&db, &payload)
        .await
        .expect("order_created event should be accepted");

    let (row_user_id, action, resource, resource_id) = audit_row(&db, event_id).await;
    assert_eq!(action, event_types::ORDER_CREATED);
    assert_eq!(resource, "order");
    assert_eq!(resource_id, Some(order_id));
    assert_eq!(row_user_id, Some(user_id));
}

#[sqlx::test]
async fn booking_cancelled_maps_to_booking_resource(db: PgPool) {
    let user_id = common::seed_member(&db, "booking-map@example.com", "password123").await;
    let booking_id = Uuid::now_v7();

    let envelope = KafkaEvent::new(
        event_types::BOOKING_CANCELLED,
        BookingCancelledPayload {
            booking_id,
            user_id,
            time_slot_id: Uuid::now_v7(),
        },
    );
    let event_id = envelope.event_id;
    let payload = serde_json::to_string(&envelope).expect("serialize envelope");

    handle_audit_event(&db, &payload)
        .await
        .expect("booking_cancelled event should be accepted");

    let (row_user_id, action, resource, resource_id) = audit_row(&db, event_id).await;
    assert_eq!(action, event_types::BOOKING_CANCELLED);
    assert_eq!(resource, "booking");
    assert_eq!(resource_id, Some(booking_id));
    assert_eq!(row_user_id, Some(user_id));
}

#[sqlx::test]
async fn user_registered_maps_to_user_resource(db: PgPool) {
    let user_id = common::seed_member(&db, "user-map@example.com", "password123").await;

    let envelope = KafkaEvent::new(
        event_types::USER_REGISTERED,
        UserRegisteredPayload {
            user_id,
            email: "user-map@example.com".into(),
            name: "User Map".into(),
        },
    );
    let event_id = envelope.event_id;
    let payload = serde_json::to_string(&envelope).expect("serialize envelope");

    handle_audit_event(&db, &payload)
        .await
        .expect("user_registered event should be accepted");

    let (row_user_id, action, resource, resource_id) = audit_row(&db, event_id).await;
    assert_eq!(action, event_types::USER_REGISTERED);
    assert_eq!(resource, "user");
    assert_eq!(resource_id, Some(user_id));
    assert_eq!(row_user_id, Some(user_id));
}

// ---------------------------------------------------------------------------
// Idempotency
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn same_event_id_consumed_twice_writes_once(db: PgPool) {
    let user_id = common::seed_member(&db, "idempotent@example.com", "password123").await;

    let envelope = KafkaEvent::new(
        event_types::BOOKING_CREATED,
        BookingCreatedPayload {
            booking_id: Uuid::now_v7(),
            user_id,
            time_slot_id: Uuid::now_v7(),
        },
    );
    let event_id = envelope.event_id;
    let payload = serde_json::to_string(&envelope).expect("serialize envelope");

    handle_audit_event(&db, &payload)
        .await
        .expect("first delivery should succeed");
    handle_audit_event(&db, &payload)
        .await
        .expect("redelivery of the same event_id should also return Ok (no-op insert)");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE id = $1")
        .bind(event_id)
        .fetch_one(&db)
        .await
        .expect("count rows for event_id");
    assert_eq!(
        count, 1,
        "redelivering the same event_id must not duplicate the row"
    );
}

// ---------------------------------------------------------------------------
// Poison (2 cases) — malformed input, must not write
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn missing_event_type_is_poison_and_does_not_write(db: PgPool) {
    let payload = json!({
        "version": 1,
        "event_id": Uuid::now_v7().to_string(),
        "data": {}
    })
    .to_string();

    let result = handle_audit_event(&db, &payload).await;
    assert!(
        matches!(result, Err(ProcessingError::Poison(_))),
        "missing event_type must classify as Poison, got {result:?}"
    );
    assert_eq!(audit_log_count(&db).await, 0);
}

#[sqlx::test]
async fn malformed_json_is_poison_and_does_not_write(db: PgPool) {
    let payload = "{not valid json";

    let result = handle_audit_event(&db, payload).await;
    assert!(
        matches!(result, Err(ProcessingError::Poison(_))),
        "malformed JSON must classify as Poison, got {result:?}"
    );
    assert_eq!(audit_log_count(&db).await, 0);
}

// ---------------------------------------------------------------------------
// Fallback (2 cases) — event_type outside the known domain families
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn unknown_event_type_with_data_resource_uses_it(db: PgPool) {
    let user_id = common::seed_member(&db, "fallback-with@example.com", "password123").await;
    let resource_id = Uuid::now_v7();
    let event_id = Uuid::now_v7();

    let payload = json!({
        "version": 1,
        "event_id": event_id.to_string(),
        "event_type": "login_succeeded",
        "data": {
            "resource": "session",
            "resource_id": resource_id.to_string(),
            "user_id": user_id.to_string(),
        }
    })
    .to_string();

    handle_audit_event(&db, &payload)
        .await
        .expect("unmapped event_type with data.resource should be accepted");

    let (row_user_id, action, resource, row_resource_id) = audit_row(&db, event_id).await;
    assert_eq!(action, "login_succeeded");
    assert_eq!(resource, "session");
    assert_eq!(row_resource_id, Some(resource_id));
    assert_eq!(row_user_id, Some(user_id));
}

#[sqlx::test]
async fn unknown_event_type_without_data_resource_defaults_to_audit(db: PgPool) {
    let event_id = Uuid::now_v7();

    let payload = json!({
        "version": 1,
        "event_id": event_id.to_string(),
        "event_type": "login_succeeded",
        "data": {}
    })
    .to_string();

    handle_audit_event(&db, &payload)
        .await
        .expect("unmapped event_type without data.resource should be accepted");

    let (row_user_id, action, resource, row_resource_id) = audit_row(&db, event_id).await;
    assert_eq!(action, "login_succeeded");
    assert_eq!(resource, "audit");
    assert_eq!(row_resource_id, None);
    assert_eq!(row_user_id, None);
}

// ---------------------------------------------------------------------------
// Missing event_id — defensive fallback, not part of the real producer wire
// format, but must not crash the consumer
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn missing_event_id_still_writes(db: PgPool) {
    let user_id = common::seed_member(&db, "no-event-id@example.com", "password123").await;

    let payload = json!({
        "version": 1,
        "event_type": event_types::USER_REGISTERED,
        "data": {
            "user_id": user_id.to_string(),
            "email": "no-event-id@example.com",
            "name": "No Event Id",
        }
    })
    .to_string();

    handle_audit_event(&db, &payload)
        .await
        .expect("missing event_id should still write, using a generated fallback id");

    assert_eq!(audit_log_count(&db).await, 1);
}
