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
//! looks up `event_type` in [`spec_for_event_type`] — the same table
//! producers use — and returns the `(resource, id_field)` pair used to
//! populate `audit_log.resource` / `resource_id`. An `event_type` not in
//! that table — including an unmodeled subtype of a known family, e.g. a
//! future `order_refunded` — returns `None`, and the caller falls back to
//! reading `data.resource` directly (defaulting to `"audit"`) — the
//! reserved `AUDIT_LOG`-topic behavior, ready for future hand-authored
//! events.
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
use rdkafka::message::BorrowedMessage;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio_stream::StreamExt;

use super::events::{ALL_SPECS, spec_for_event_type, topics};

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

/// Outcome of a single poll iteration, classified just enough for [`decide`]
/// to pick the next [`LoopAction`]. `StreamError` (the stream itself
/// yielding an error, not a message) rounds out the decision table even
/// though [`start_consumer`] never constructs it — see the comment at that
/// call site for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollOutcome {
    // Only constructed by the table test below — `start_consumer`'s
    // stream-Err branch implements this policy directly (see the comment
    // there) rather than constructing this variant, so it's allowed to be
    // otherwise-unused rather than a sign of dead code.
    #[allow(dead_code)]
    StreamError,
    NonUtf8Payload,
    EmptyPayload,
    HandledOk,
    HandledPoison,
    HandledTransient { attempts: u32 },
}

/// What the loop should do about the current message's offset and retry
/// bookkeeping, given a [`PollOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopAction {
    /// Commit the offset and drop this message's retry count.
    CommitAndClear,
    /// Leave the offset uncommitted (so the message is redelivered) and
    /// keep the bumped retry count.
    LeaveForRetry,
    /// Not a message — just poll again. There's no offset or retry key to
    /// act on.
    PollAgain,
}

/// The consumer loop's full decision table, isolated from IO so it can be
/// exhaustively unit tested (see `#[cfg(test)]` below) instead of living
/// only as scattered match arms in [`start_consumer`]. Never logs or
/// touches the network/DB — every branch's log text stays in the shell.
fn decide(outcome: &PollOutcome) -> LoopAction {
    match outcome {
        PollOutcome::StreamError => LoopAction::PollAgain,
        PollOutcome::NonUtf8Payload
        | PollOutcome::EmptyPayload
        | PollOutcome::HandledOk
        | PollOutcome::HandledPoison => LoopAction::CommitAndClear,
        PollOutcome::HandledTransient { attempts } => {
            if *attempts >= MAX_TRANSIENT_RETRIES {
                LoopAction::CommitAndClear
            } else {
                LoopAction::LeaveForRetry
            }
        }
    }
}

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
    // Derived from `ALL_SPECS` (the same table producers use) plus
    // `AUDIT_LOG`, instead of a hand-written array that could drift from
    // the producer side. Order matches the previous literal array.
    let topic_list: Vec<&str> = std::iter::once(topics::AUDIT_LOG)
        .chain(ALL_SPECS.iter().map(|spec| spec.topic))
        .collect();

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
                        // Policy: `decide(StreamError) → PollAgain`, pinned
                        // by the table test on `decide` below. This early
                        // `continue` is just the control-flow shortcut for
                        // that policy — a stream error isn't a message, so
                        // there's no `retry_key` yet and nothing to commit.
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
                        if let LoopAction::CommitAndClear = decide(&PollOutcome::NonUtf8Payload) {
                            commit_and_clear(&consumer, &message, &retry_key, &mut retry_counts);
                        }
                        continue;
                    }
                    None => {
                        tracing::warn!(topic = %topic, partition, offset, "empty Kafka payload, skipping");
                        if let LoopAction::CommitAndClear = decide(&PollOutcome::EmptyPayload) {
                            commit_and_clear(&consumer, &message, &retry_key, &mut retry_counts);
                        }
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
                        if let LoopAction::CommitAndClear = decide(&PollOutcome::HandledOk) {
                            commit_and_clear(&consumer, &message, &retry_key, &mut retry_counts);
                        }
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
                        if let LoopAction::CommitAndClear = decide(&PollOutcome::HandledPoison) {
                            commit_and_clear(&consumer, &message, &retry_key, &mut retry_counts);
                        }
                    }
                    Err(ProcessingError::Transient(reason)) => {
                        let attempts_slot = retry_counts.entry(retry_key.clone()).or_insert(0);
                        *attempts_slot += 1;
                        let attempts = *attempts_slot;

                        match decide(&PollOutcome::HandledTransient { attempts }) {
                            LoopAction::CommitAndClear => {
                                // Escape hatch: after N failed retries, commit
                                // and log loudly so the partition isn't stuck.
                                tracing::error!(
                                    topic = %topic,
                                    partition,
                                    offset,
                                    attempts,
                                    "transient handler failure exceeded retry cap; dropping: {reason}"
                                );
                                commit_and_clear(&consumer, &message, &retry_key, &mut retry_counts);
                            }
                            LoopAction::LeaveForRetry => {
                                tracing::warn!(
                                    topic = %topic,
                                    partition,
                                    offset,
                                    attempt = attempts,
                                    max = MAX_TRANSIENT_RETRIES,
                                    "transient handler failure, not committing (will retry): {reason}"
                                );
                            }
                            LoopAction::PollAgain => {
                                // Never returned for a `HandledTransient`
                                // outcome — `decide`'s table test pins the
                                // full mapping. No-op rather than a panic
                                // if that ever changed.
                            }
                        }
                    }
                }
            }
        }
    }

    tracing::info!("Kafka consumer loop exited");
}

/// Single implementation of the `LoopAction::CommitAndClear` action: commit
/// the offset and drop this message's retry count. Every branch that stops
/// retrying (a clean handle, a poison record, a non-UTF-8/empty payload, or
/// a transient failure that hit the retry cap) funnels through here instead
/// of five hand-copied commit+remove pairs.
///
/// A commit failure is logged and otherwise ignored — the offset is simply
/// retried on the next commit rather than treated as another processing
/// failure, so it does not go through `decide` again.
fn commit_and_clear(
    consumer: &StreamConsumer,
    message: &BorrowedMessage<'_>,
    retry_key: &(String, i32, i64),
    retry_counts: &mut HashMap<(String, i32, i64), u32>,
) {
    let topic = &retry_key.0;
    let partition = retry_key.1;
    let offset = retry_key.2;
    if let Err(e) = consumer.commit_message(message, CommitMode::Async) {
        tracing::error!(topic = %topic, partition, offset, "commit failed: {e}");
    }
    retry_counts.remove(retry_key);
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
/// A thin wrapper over [`spec_for_event_type`] — the same table the
/// producer side uses. An `event_type` not in [`ALL_SPECS`] returns `None`,
/// and the caller falls back to reading `data.resource` directly
/// (defaulting to `"audit"`) — the pre-existing `AUDIT_LOG`-topic behavior,
/// unchanged.
///
/// This used to also prefix-/exact-match `event_type` as a fallback when
/// the spec lookup missed (`order_*` → `"order"`, `booking_*` →
/// `"booking"`, `user_registered` → `"user"`). That fallback was dead code
/// against the current 5 event types — every `order_*`/`booking_*` type
/// that exists is already in `ALL_SPECS` — and has been removed so it can't
/// silently shadow a not-yet-modeled subtype of a known family (e.g. a
/// future `order_refunded`) into the wrong resource; such an event_type now
/// falls through to `data.resource` like any other unmodeled type.
fn domain_resource(event_type: &str) -> Option<(&'static str, &'static str)> {
    spec_for_event_type(event_type).map(|s| (s.resource, s.id_field))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Table-driven check of `decide`'s full `PollOutcome` → `LoopAction`
    /// mapping, including the `MAX_TRANSIENT_RETRIES` boundary: an attempt
    /// count *equal to* the cap must already give up (commit), not just
    /// counts strictly beyond it.
    #[test]
    fn decide_maps_every_poll_outcome_to_the_documented_loop_action() {
        assert_eq!(
            MAX_TRANSIENT_RETRIES, 5,
            "test cases below assume MAX_TRANSIENT_RETRIES == 5"
        );

        let cases: [(PollOutcome, LoopAction); 9] = [
            (PollOutcome::StreamError, LoopAction::PollAgain),
            (PollOutcome::NonUtf8Payload, LoopAction::CommitAndClear),
            (PollOutcome::EmptyPayload, LoopAction::CommitAndClear),
            (PollOutcome::HandledOk, LoopAction::CommitAndClear),
            (PollOutcome::HandledPoison, LoopAction::CommitAndClear),
            (
                PollOutcome::HandledTransient { attempts: 1 },
                LoopAction::LeaveForRetry,
            ),
            (
                PollOutcome::HandledTransient { attempts: 4 },
                LoopAction::LeaveForRetry,
            ),
            (
                // attempts == MAX_TRANSIENT_RETRIES: the boundary itself
                // must already give up, not just counts beyond it.
                PollOutcome::HandledTransient { attempts: 5 },
                LoopAction::CommitAndClear,
            ),
            (
                PollOutcome::HandledTransient { attempts: 6 },
                LoopAction::CommitAndClear,
            ),
        ];

        for (outcome, expected) in cases {
            assert_eq!(
                decide(&outcome),
                expected,
                "decide({outcome:?}) should be {expected:?}"
            );
        }
    }
}
