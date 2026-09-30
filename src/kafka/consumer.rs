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

use async_trait::async_trait;
use rdkafka::config::ClientConfig;
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use rdkafka::error::KafkaResult;
use rdkafka::{Message, Offset, TopicPartitionList};
use sqlx::PgPool;
use tokio::sync::watch;

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
        // Manual commit: `run` commits a message's offset only once the
        // handler has durably written the record to Postgres.
        .set("enable.auto.commit", "false")
        .set("session.timeout.ms", "30000")
        .set("max.poll.interval.ms", "300000")
        .create()
}

/// Drive the consumer loop until a shutdown signal arrives on `shutdown_rx`.
///
/// Subscribes, then hands the consumer to [`run`] behind the
/// [`MessageSource`] port, with the pool as the [`AuditHandler`].
pub async fn start_consumer(
    consumer: StreamConsumer,
    db: PgPool,
    shutdown_rx: watch::Receiver<bool>,
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

    run(KafkaSource(consumer), db, shutdown_rx).await;
}

/// One consumed record, copied out of the broker's borrowed message so the
/// loop owns it across retries and awaits.
#[derive(Debug, Clone)]
struct SourceMessage {
    topic: String,
    partition: i32,
    offset: i64,
    payload: Option<Vec<u8>>,
}

/// Port between [`run`] and the broker: yields records in order and commits
/// a record's offset once the loop is done with it. `next` returning `None`
/// means the source has ended and the loop exits.
#[async_trait]
trait MessageSource: Send {
    async fn next(&mut self) -> Option<SourceMessage>;
    fn commit(&self, msg: &SourceMessage) -> KafkaResult<()>;
}

/// Production [`MessageSource`] adapter over rdkafka's `StreamConsumer`.
struct KafkaSource(StreamConsumer);

#[async_trait]
impl MessageSource for KafkaSource {
    async fn next(&mut self) -> Option<SourceMessage> {
        loop {
            // `recv` is cancel-safe, so racing it against shutdown in `run`
            // never loses a message.
            match self.0.recv().await {
                Ok(m) => {
                    return Some(SourceMessage {
                        topic: m.topic().to_string(),
                        partition: m.partition(),
                        offset: m.offset(),
                        payload: m.payload().map(<[u8]>::to_vec),
                    });
                }
                // A stream error isn't a message: nothing to handle or
                // commit, so log it and read the next one.
                Err(e) => tracing::error!("Kafka consumer error: {e}"),
            }
        }
    }

    fn commit(&self, msg: &SourceMessage) -> KafkaResult<()> {
        let mut tpl = TopicPartitionList::new();
        // `+ 1` is essential: a Kafka committed offset names the *next*
        // record to consume, not the last one processed (this is what
        // `commit_message` did internally). Committing `msg.offset` itself
        // would redeliver this record after every restart or rebalance.
        tpl.add_partition_offset(&msg.topic, msg.partition, Offset::Offset(msg.offset + 1))?;
        self.0.commit(&tpl, CommitMode::Async)
    }
}

/// Seam for processing one payload. [`PgPool`] is the production adapter;
/// tests use a scripted handler so the paused-clock loop tests never touch
/// the database.
#[async_trait]
trait AuditHandler {
    async fn handle(&self, payload: &str) -> Result<(), ProcessingError>;
}

#[async_trait]
impl AuditHandler for PgPool {
    async fn handle(&self, payload: &str) -> Result<(), ProcessingError> {
        // Every subscribed topic (AUDIT_LOG + the 5 domain topics) is
        // durably recorded the same way — see the module docs for how
        // domain payloads get mapped to a resource.
        handle_audit_event(self, payload).await
    }
}

/// What [`run`] does with a message once [`resolve`] is done with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    /// Commit the offset: the message is handled, or will never succeed.
    Commit,
    /// Leave the offset uncommitted so the message is redelivered after a
    /// restart or rebalance.
    Skip,
}

/// The consumer loop: pull a message, [`resolve`] it, commit if told to.
///
/// Shutdown is only checked between messages (`biased`, so it wins over new
/// work): a SIGTERM during handler execution still lets the current message
/// complete before the loop breaks.
async fn run<S: MessageSource, H: AuditHandler>(
    mut source: S,
    handler: H,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // Track transient retries by (topic, partition, offset). Lets the
    // consumer escape a truly poisoned record after `MAX_TRANSIENT_RETRIES`
    // failed attempts rather than redelivering it forever. The map clears
    // itself as messages are committed.
    let mut retry_counts: HashMap<(String, i32, i64), u32> = HashMap::new();

    loop {
        let next = tokio::select! {
            biased;

            // Shutdown wins over new messages: don't pick up work we cannot
            // complete before the main task drops the DB pool.
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("Kafka consumer received shutdown, draining and exiting");
                    break;
                }
                continue;
            }

            next = source.next() => next,
        };

        let Some(msg) = next else {
            tracing::info!("Kafka stream ended, exiting consumer loop");
            break;
        };

        let retry_key = (msg.topic.clone(), msg.partition, msg.offset);
        if resolve(&msg, &handler, &mut retry_counts).await == Resolution::Commit {
            if let Err(e) = source.commit(&msg) {
                // Logged and otherwise ignored: the offset is simply
                // committed again with a later message.
                tracing::error!(
                    topic = %msg.topic,
                    partition = msg.partition,
                    offset = msg.offset,
                    "commit failed: {e}"
                );
            }
            retry_counts.remove(&retry_key);
        }
    }

    tracing::info!("Kafka consumer loop exited");
}

/// Decide one message's fate: decode the payload, call the handler, and
/// classify the result into a [`Resolution`]. Every branch's log line lives
/// here.
async fn resolve<H: AuditHandler>(
    msg: &SourceMessage,
    handler: &H,
    retry_counts: &mut HashMap<(String, i32, i64), u32>,
) -> Resolution {
    let (topic, partition, offset) = (&msg.topic, msg.partition, msg.offset);

    let payload = match msg.payload.as_deref().map(std::str::from_utf8) {
        Some(Ok(text)) => text,
        Some(Err(e)) => {
            // Non-UTF-8 payload will never decode on retry; commit past it
            // loudly so the partition isn't wedged.
            tracing::error!(
                topic = %topic,
                partition,
                offset,
                poison = "non_utf8_payload",
                "dropping poison Kafka message: {e}"
            );
            return Resolution::Commit;
        }
        None => {
            tracing::warn!(topic = %topic, partition, offset, "empty Kafka payload, skipping");
            return Resolution::Commit;
        }
    };

    tracing::debug!(topic = %topic, "Received Kafka message");

    match handler.handle(payload).await {
        Ok(()) => Resolution::Commit,
        Err(ProcessingError::Poison(reason)) => {
            // Deterministic failure (malformed JSON, missing required
            // fields). Retrying will not help — commit past it with a loud
            // error so ops can alert.
            tracing::error!(
                topic = %topic,
                partition,
                offset,
                poison = %reason,
                "dropping poison Kafka message"
            );
            Resolution::Commit
        }
        Err(ProcessingError::Transient(reason)) => {
            let attempts_slot = retry_counts
                .entry((topic.clone(), partition, offset))
                .or_insert(0);
            *attempts_slot += 1;
            let attempts = *attempts_slot;

            if attempts >= MAX_TRANSIENT_RETRIES {
                // Escape hatch: after N failed retries, commit and log
                // loudly so the partition isn't stuck.
                tracing::error!(
                    topic = %topic,
                    partition,
                    offset,
                    attempts,
                    "transient handler failure exceeded retry cap; dropping: {reason}"
                );
                Resolution::Commit
            } else {
                tracing::warn!(
                    topic = %topic,
                    partition,
                    offset,
                    attempt = attempts,
                    max = MAX_TRANSIENT_RETRIES,
                    "transient handler failure, not committing (will retry): {reason}"
                );
                Resolution::Skip
            }
        }
    }
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
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// What the loop did, in order: `H(offset)` = handler called for that
    /// message, `C(offset)` = that message's offset committed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        H(i64),
        C(i64),
    }

    type Events = Arc<Mutex<Vec<Event>>>;

    /// Yields its queued messages in order, then ends (`None`).
    struct ScriptedSource {
        messages: VecDeque<SourceMessage>,
        events: Events,
    }

    #[async_trait]
    impl MessageSource for ScriptedSource {
        async fn next(&mut self) -> Option<SourceMessage> {
            self.messages.pop_front()
        }

        fn commit(&self, msg: &SourceMessage) -> KafkaResult<()> {
            self.events.lock().unwrap().push(Event::C(msg.offset));
            Ok(())
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Step {
        Ok,
        Poison,
    }

    /// Payloads are the message's offset as text. Each offset has a script
    /// of steps; the last step repeats forever, and an unscripted offset
    /// succeeds.
    struct ScriptedHandler {
        script: Mutex<HashMap<i64, VecDeque<Step>>>,
        events: Events,
    }

    #[async_trait]
    impl AuditHandler for ScriptedHandler {
        async fn handle(&self, payload: &str) -> Result<(), ProcessingError> {
            let offset: i64 = payload.parse().expect("payload is an offset");
            self.events.lock().unwrap().push(Event::H(offset));
            let mut script = self.script.lock().unwrap();
            let step = match script.get_mut(&offset) {
                Some(steps) if steps.len() > 1 => steps.pop_front().unwrap(),
                Some(steps) => steps.front().copied().unwrap_or(Step::Ok),
                None => Step::Ok,
            };
            match step {
                Step::Ok => Ok(()),
                Step::Poison => Err(ProcessingError::poison("scripted poison")),
            }
        }
    }

    fn msg(offset: i64, payload: Option<Vec<u8>>) -> SourceMessage {
        SourceMessage {
            topic: "audit-log".to_string(),
            partition: 0,
            offset,
            payload,
        }
    }

    /// A message whose payload is its own offset, for [`ScriptedHandler`].
    fn text_msg(offset: i64) -> SourceMessage {
        msg(offset, Some(offset.to_string().into_bytes()))
    }

    fn fixture(
        messages: Vec<SourceMessage>,
        script: Vec<(i64, Vec<Step>)>,
    ) -> (ScriptedSource, ScriptedHandler, Events) {
        let events: Events = Arc::default();
        let source = ScriptedSource {
            messages: messages.into(),
            events: events.clone(),
        };
        let handler = ScriptedHandler {
            script: Mutex::new(
                script
                    .into_iter()
                    .map(|(offset, steps)| (offset, steps.into()))
                    .collect(),
            ),
            events: events.clone(),
        };
        (source, handler, events)
    }

    /// Runs the loop to completion (the source ending) and returns what
    /// happened. The shutdown sender is kept alive for the whole run.
    async fn run_to_end(messages: Vec<SourceMessage>, script: Vec<(i64, Vec<Step>)>) -> Vec<Event> {
        let (source, handler, events) = fixture(messages, script);
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        run(source, handler, shutdown_rx).await;
        events.lock().unwrap().clone()
    }

    use Event::{C, H};

    #[tokio::test(start_paused = true)]
    async fn ok_is_handled_then_committed() {
        let events = run_to_end(vec![text_msg(0), text_msg(1)], vec![]).await;
        assert_eq!(events, vec![H(0), C(0), H(1), C(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn poison_is_committed_and_the_next_message_follows() {
        let events = run_to_end(
            vec![text_msg(0), text_msg(1)],
            vec![(0, vec![Step::Poison])],
        )
        .await;
        assert_eq!(events, vec![H(0), C(0), H(1), C(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn non_utf8_payload_is_committed_without_calling_the_handler() {
        let events = run_to_end(vec![msg(0, Some(vec![0xff, 0xfe])), text_msg(1)], vec![]).await;
        assert_eq!(events, vec![C(0), H(1), C(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn empty_payload_is_committed_without_calling_the_handler() {
        let events = run_to_end(vec![msg(0, None), text_msg(1)], vec![]).await;
        assert_eq!(events, vec![C(0), H(1), C(1)]);
    }

    #[tokio::test(start_paused = true)]
    async fn source_end_exits_the_loop() {
        let events = run_to_end(vec![], vec![]).await;
        assert_eq!(events, vec![]);
    }
}
