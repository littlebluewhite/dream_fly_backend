//! Kafka consumer: the audit-log sink.
//!
//! ## Subscribed topics
//!
//! Subscribes to the 5 domain topics the rest of the service publishes to
//! (orders created / status-changed, bookings created / cancelled, users registered)
//! plus [`topics::AUDIT_LOG`] (reserved for hand-authored audit events; none published today).
//! Every subscribed topic is routed through the same [`handle_audit_event`]
//! handler — there is no per-topic branch beyond the resource mapping
//! described below.
//!
//! ## Audit-only invariant
//!
//! This consumer's only job is durably recording events into `audit_log`.
//! It must never drive other business side effects (notifications, points,
//! etc.) — those are written synchronously by their own service-layer code
//! at the point of mutation. Keeping this consumer audit-only means it can
//! be paused, replayed, or rebuilt from Kafka without affecting anything
//! else the system does.
//!
//! ## Resource mapping
//!
//! The 5 domain payloads don't carry a `data.resource` field the way
//! hand-authored audit events do (none published today), so without a mapping step every domain
//! event would collapse onto the generic `"audit"` fallback. [`domain_resource`]
//! maps `event_type` to the `(resource, id_field)` pair used to populate
//! `audit_log.resource` / `resource_id`. Anything it doesn't recognize
//! returns `None`, and the caller falls back to reading `data.resource`
//! directly (defaulting to `"audit"`) — the reserved `AUDIT_LOG`-topic
//! behavior, ready for future hand-authored events.
//!
//! ## Idempotency key
//!
//! The envelope's `event_id` (a UUIDv7, unique per produced event) is used
//! directly as `audit_log.id`, with `ON CONFLICT (id) DO NOTHING` on insert.
//! This makes redelivery (consumer restart before commit, rebalance,
//! at-least-once redelivery in general) a no-op rather than a duplicate row,
//! without a schema migration for a separate dedupe key. An envelope missing
//! `event_id` (defensive fallback only — the producer always sets one) gets
//! a fresh `Uuid::now_v7()`, which forgoes idempotency for that one record
//! rather than failing the whole write.
//!
//! ## Accepted risks
//!
//! - **First-deploy backfill**: `auto.offset.reset=earliest` means the
//!   first time this consumer group runs, it replays every event still in
//!   topic retention. Combined with the idempotency key above, this is a
//!   one-time, deterministic backfill, not an ongoing duplication risk.
//! - **`created_at` is consumption time, not event time**: the column is
//!   set to `NOW()` at insert, not the envelope's `timestamp`. This is the
//!   pre-existing behavior for the `AUDIT_LOG` topic and is left unchanged
//!   for the domain topics too, for consistency.
//! - **A user deleted before its event is consumed**: `audit_log.user_id`
//!   has a foreign key to `users`. If the referenced row is gone by the
//!   time the event is processed, the insert fails with a FK violation,
//!   which `From<sqlx::Error>` classifies as `Transient` and retries up to
//!   [`MAX_TRANSIENT_RETRIES`] times before being dropped loudly. This
//!   existing error classification is not changed here.

use std::collections::HashMap;

use rdkafka::Message;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio_stream::StreamExt;

use super::events::topics;

/// Classifies a handler failure so the main loop can decide whether to
/// retry (`Transient`) or give up and commit the offset (`Poison`).
///
/// - `Transient`: likely to succeed on retry (DB connection blip, Redis
///   timeout, transient network error). The consumer keeps the offset so
///   the message is redelivered after a restart or rebalance.
/// - `Poison`: deterministic failure that retries cannot fix (malformed
///   JSON, missing required field, impossible payload). The consumer logs
///   at ERROR and commits past the message so the consumer group is not
///   stuck in a redelivery loop. A DLQ would replace this once available.
#[derive(Debug)]
pub enum ProcessingError {
    Transient(String),
    Poison(String),
}

impl ProcessingError {
    fn transient(msg: impl Into<String>) -> Self {
        Self::Transient(msg.into())
    }

    fn poison(msg: impl Into<String>) -> Self {
        Self::Poison(msg.into())
    }
}

impl From<sqlx::Error> for ProcessingError {
    fn from(e: sqlx::Error) -> Self {
        // All sqlx errors that reach here are DB-level problems (connection
        // closed, constraint, timeout). Classify conservatively as
        // transient so we don't lose events on a hiccup; a truly poison
        // constraint violation will eventually hit the retry cap and be
        // dropped loudly.
        Self::transient(format!("sqlx error: {e}"))
    }
}

/// Ceiling on transient retries for a single message before we give up and
/// commit the offset. Set high enough that real transient errors recover
/// naturally (Postgres reconnect, Redis flush) but low enough that a truly
/// poison record doesn't wedge the partition forever.
const MAX_TRANSIENT_RETRIES: u32 = 5;

/// Build a Kafka consumer configured for at-least-once processing:
/// `enable.auto.commit=false` means we commit *after* we've successfully
/// written the message to the database. A crash mid-processing causes
/// the message to be re-delivered rather than silently lost.
pub fn create_consumer(
    brokers: &str,
    group_id: &str,
) -> Result<StreamConsumer, rdkafka::error::KafkaError> {
    ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("group.id", group_id)
        .set("auto.offset.reset", "earliest")
        // Manual commit: we call `commit_message` only when the handler has
        // durably written the record to Postgres.
        .set("enable.auto.commit", "false")
        .set("session.timeout.ms", "30000")
        .set("max.poll.interval.ms", "300000")
        .create()
}

/// Drive the consumer loop until a shutdown signal arrives on `shutdown_rx`.
///
/// Cancellation-safe: the `tokio::select!` races the message stream against
/// the shutdown channel, so a SIGTERM during handler execution still lets
/// the current message complete before the loop breaks.
pub async fn start_consumer(
    consumer: StreamConsumer,
    db: PgPool,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let topic_list = [
        topics::AUDIT_LOG,
        topics::ORDERS_CREATED,
        topics::ORDERS_STATUS_CHANGED,
        topics::BOOKINGS_CREATED,
        topics::BOOKINGS_CANCELLED,
        topics::USERS_REGISTERED,
    ];

    if let Err(e) = consumer.subscribe(&topic_list) {
        tracing::error!("Failed to subscribe to Kafka topics: {e}");
        return;
    }

    tracing::info!(
        "Kafka consumer started, subscribed to {} topics",
        topic_list.len()
    );

    let mut stream = consumer.stream();

    // Track transient retries by (topic, partition, offset). Lets the
    // consumer escape a truly poisoned record after `MAX_TRANSIENT_RETRIES`
    // failed attempts rather than redelivering it forever. The map clears
    // itself as messages are committed.
    let mut retry_counts: HashMap<(String, i32, i64), u32> = HashMap::new();

    loop {
        tokio::select! {
            biased;

            // Shutdown wins over new messages: don't pick up work we cannot
            // complete before the main task drops the DB pool.
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("Kafka consumer received shutdown, draining and exiting");
                    break;
                }
            }

            msg = stream.next() => {
                let Some(result) = msg else {
                    tracing::info!("Kafka stream ended, exiting consumer loop");
                    break;
                };

                let message = match result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!("Kafka consumer error: {e}");
                        continue;
                    }
                };

                let topic = message.topic().to_string();
                let partition = message.partition();
                let offset = message.offset();
                let retry_key = (topic.clone(), partition, offset);

                let payload = match message.payload_view::<str>() {
                    Some(Ok(text)) => text.to_string(),
                    Some(Err(e)) => {
                        // Non-UTF-8 payload will never decode on retry; commit
                        // past it loudly so the partition isn't wedged.
                        tracing::error!(
                            topic = %topic,
                            partition,
                            offset,
                            poison = "non_utf8_payload",
                            "dropping poison Kafka message: {e}"
                        );
                        let _ = consumer.commit_message(&message, CommitMode::Async);
                        retry_counts.remove(&retry_key);
                        continue;
                    }
                    None => {
                        tracing::warn!(topic = %topic, partition, offset, "empty Kafka payload, skipping");
                        let _ = consumer.commit_message(&message, CommitMode::Async);
                        retry_counts.remove(&retry_key);
                        continue;
                    }
                };

                tracing::debug!(topic = %topic, "Received Kafka message");

                // Every subscribed topic (AUDIT_LOG + the 5 domain topics)
                // is durably recorded the same way — see the module docs
                // for how domain payloads get mapped to a resource.
                let handler_result = handle_audit_event(&db, &payload).await;

                match handler_result {
                    Ok(()) => {
                        if let Err(e) = consumer.commit_message(&message, CommitMode::Async) {
                            tracing::error!(topic = %topic, partition, offset, "commit failed: {e}");
                        }
                        retry_counts.remove(&retry_key);
                    }
                    Err(ProcessingError::Poison(reason)) => {
                        // Deterministic failure (malformed JSON, missing
                        // required fields). Retrying will not help — commit
                        // past it with a loud error so ops can alert.
                        tracing::error!(
                            topic = %topic,
                            partition,
                            offset,
                            poison = %reason,
                            "dropping poison Kafka message"
                        );
                        let _ = consumer.commit_message(&message, CommitMode::Async);
                        retry_counts.remove(&retry_key);
                    }
                    Err(ProcessingError::Transient(reason)) => {
                        let attempts = retry_counts.entry(retry_key.clone()).or_insert(0);
                        *attempts += 1;

                        if *attempts >= MAX_TRANSIENT_RETRIES {
                            // Escape hatch: after N failed retries, commit
                            // and log loudly so the partition isn't stuck.
                            tracing::error!(
                                topic = %topic,
                                partition,
                                offset,
                                attempts = *attempts,
                                "transient handler failure exceeded retry cap; dropping: {reason}"
                            );
                            let _ = consumer.commit_message(&message, CommitMode::Async);
                            retry_counts.remove(&retry_key);
                        } else {
                            tracing::warn!(
                                topic = %topic,
                                partition,
                                offset,
                                attempt = *attempts,
                                max = MAX_TRANSIENT_RETRIES,
                                "transient handler failure, not committing (will retry): {reason}"
                            );
                        }
                    }
                }
            }
        }
    }

    tracing::info!("Kafka consumer loop exited");
}

/// Pull a required string field from a JSON value, returning Poison if
/// missing — these events are machine-generated by our own producer, so a
/// missing required field is a producer bug, not a transient issue.
fn required_str<'a>(
    event: &'a serde_json::Value,
    field: &str,
) -> Result<&'a str, ProcessingError> {
    event
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProcessingError::poison(format!("missing or non-string field `{field}`")))
}

/// Pull an optional UUID from `event.data.<field>` if present and parseable.
/// Unparseable UUIDs are treated as absent (logged elsewhere) rather than
/// poison so a partial payload still stores *something* useful.
fn optional_uuid_from_data(event: &serde_json::Value, field: &str) -> Option<uuid::Uuid> {
    event
        .get("data")
        .and_then(|d| d.get(field))
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
}

/// Map a domain event's `event_type` to the `(resource, id_field)` pair used
/// to populate `audit_log.resource` / `resource_id`. The 5 domain topics
/// (`order_*`, `booking_*`, `user_registered`) don't carry a `data.resource`
/// field the way hand-authored audit events do, so without this mapping
/// every domain event would collapse onto the generic `"audit"` fallback.
///
/// `order_*` and `booking_*` are prefix-matched (there are two event types
/// in each family); `user_registered` is matched exactly since it's the
/// only user-domain event type today. Anything not matched here returns
/// `None`, and the caller falls back to reading `data.resource` directly
/// (defaulting to `"audit"`) — the pre-existing `AUDIT_LOG`-topic behavior,
/// unchanged.
fn domain_resource(event_type: &str) -> Option<(&'static str, &'static str)> {
    if event_type.starts_with("order_") {
        Some(("order", "order_id"))
    } else if event_type.starts_with("booking_") {
        Some(("booking", "booking_id"))
    } else if event_type == "user_registered" {
        Some(("user", "user_id"))
    } else {
        None
    }
}

/// Resolve the row id for this audit_log insert: the envelope's `event_id`
/// when present and parseable — this is what makes redelivery idempotent —
/// or a fresh v7 UUID otherwise. A missing/invalid `event_id` should never
/// happen from our own producer, but degrading to "write once, without
/// idempotency" is safer than treating it as poison.
fn event_row_id(event: &serde_json::Value) -> uuid::Uuid {
    event
        .get("event_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::now_v7)
}

pub async fn handle_audit_event(db: &PgPool, payload: &str) -> Result<(), ProcessingError> {
    let event: serde_json::Value = serde_json::from_str(payload)
        .map_err(|e| ProcessingError::poison(format!("invalid JSON: {e}")))?;

    // `event_type` is required — if it's missing, the producer is broken.
    let action = required_str(&event, "event_type")?.to_string();

    // Domain events (order_*/booking_*/user_registered) map to a concrete
    // resource; anything else falls back to `data.resource` (defaulting to
    // "audit") — the original AUDIT_LOG-topic behavior, unchanged.
    let (resource, resource_id) = match domain_resource(&action) {
        Some((resource, id_field)) => (
            resource.to_string(),
            optional_uuid_from_data(&event, id_field),
        ),
        None => (
            event
                .get("data")
                .and_then(|d| d.get("resource"))
                .and_then(|v| v.as_str())
                .unwrap_or("audit")
                .to_string(),
            optional_uuid_from_data(&event, "resource_id"),
        ),
    };

    let user_id = optional_uuid_from_data(&event, "user_id");
    let new_value = event.get("data").cloned().unwrap_or(serde_json::json!({}));
    let id = event_row_id(&event);

    sqlx::query(
        "INSERT INTO audit_log (id, user_id, action, resource, resource_id, new_value, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(user_id)
    .bind(&action)
    .bind(&resource)
    .bind(resource_id)
    .bind(&new_value)
    .execute(db)
    .await?;

    tracing::debug!(%action, "Audit event recorded");
    Ok(())
}
