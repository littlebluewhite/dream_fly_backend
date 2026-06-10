//! Transactional outbox dispatcher for Kafka events.
//!
//! ## Why an outbox
//!
//! The naïve `publish_event`-after-commit pattern has an irrecoverable gap:
//! if the DB commit succeeds but the Kafka send fails (broker down, network
//! blip, process crash), the business state advances while the downstream
//! event is silently lost. Retries never happen because nothing in the DB
//! records that the event was meant to fire.
//!
//! The outbox moves the "remember to publish this" record into the same DB
//! transaction as the business write: either both are durable or neither
//! is. A background dispatcher then drains the outbox to Kafka and marks
//! each row published on broker ack. A crash before ack simply leaves the
//! row unpublished — the dispatcher picks it up on the next tick (or after
//! a restart) and retries. This is at-least-once: consumers must still be
//! idempotent, but no events are lost.
//!
//! ## How to use
//!
//! Inside a service-layer transaction that mutates business data, call
//! [`insert_event_tx`] before `tx.commit()`. For example, in
//! `orders::service::checkout`:
//!
//! ```ignore
//! outbox::insert_event_tx(
//!     &mut tx,
//!     topics::ORDERS_CREATED,
//!     event_types::ORDER_CREATED,
//!     &order.id.to_string(),
//!     OrderCreatedPayload { .. },
//!     correlation_id, // from x-request-id, optional
//! ).await?;
//! tx.commit().await?;
//! ```
//!
//! The dispatcher is started once at process boot — see
//! [`start_dispatcher`], wired in `main.rs`.

use std::time::Duration;

use rdkafka::producer::FutureProducer;
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

use super::events::KafkaEvent;

/// Poll interval for the dispatcher tick loop. Short enough that an event
/// published to the outbox is typically on Kafka within a second, long
/// enough to avoid hammering Postgres when there's no work.
const DISPATCHER_TICK: Duration = Duration::from_millis(500);

/// Maximum number of outbox rows claimed per dispatcher tick. Prevents a
/// backlog (after a Kafka outage, say) from saturating a single tick's DB
/// transaction for so long that it blocks other writers.
const BATCH_SIZE: i64 = 100;

/// Insert a KafkaEvent envelope into the outbox inside the caller's
/// transaction. The event is guaranteed to be published at least once
/// (provided the tx commits) — never lost, never silently dropped.
///
/// `correlation_id` is typically the value of the request's `x-request-id`
/// header, so consumer-side logs can be tied back to the originating HTTP
/// request.
pub async fn insert_event_tx<T: Serialize>(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    topic: &'static str,
    event_type: &'static str,
    key: &str,
    data: T,
    correlation_id: Option<String>,
) -> Result<(), sqlx::Error> {
    let mut envelope = KafkaEvent::new(event_type, data);
    if let Some(id) = correlation_id {
        envelope = envelope.with_correlation_id(id);
    }
    let payload = serde_json::to_value(&envelope).map_err(|e| {
        // Serialization failure means the payload struct has a Serialize
        // impl that can fail — treat as a DB-layer protocol error so the
        // caller's `?` propagates naturally.
        sqlx::Error::Protocol(format!("failed to serialize outbox event: {e}"))
    })?;

    sqlx::query(
        "INSERT INTO events_outbox (id, topic, kafka_key, payload) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(topic)
    .bind(key)
    .bind(payload)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Start the background dispatcher task. Runs until `shutdown_rx` goes true.
///
/// If `producer` is `None` (Kafka disabled at boot), the dispatcher does
/// not start — events accumulate in the outbox table but aren't published.
/// Enabling Kafka and restarting the process will drain the backlog.
pub async fn start_dispatcher(
    db: PgPool,
    producer: FutureProducer,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tracing::info!("Kafka outbox dispatcher started");
    let mut ticker = tokio::time::interval(DISPATCHER_TICK);

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("Kafka outbox dispatcher received shutdown, exiting");
                    break;
                }
            }

            _ = ticker.tick() => {
                if let Err(e) = drain_once(&db, &producer).await {
                    // Transient DB errors are normal during restarts; log
                    // and keep the loop alive. The next tick will retry.
                    tracing::warn!(error = %e, "outbox drain tick failed");
                }
            }
        }
    }
}

/// Drain up to [`BATCH_SIZE`] pending rows. Each row is claimed with
/// `FOR UPDATE SKIP LOCKED` so running multiple dispatchers (for
/// horizontal scale) produces disjoint work partitions without conflict.
async fn drain_once(db: &PgPool, producer: &FutureProducer) -> Result<(), sqlx::Error> {
    // Phase 1: claim a batch of unpublished rows with row locks held for
    // the duration of the transaction. `SKIP LOCKED` lets concurrent
    // dispatchers pick different rows rather than blocking on each other.
    let mut tx = db.begin().await?;

    let rows: Vec<(Uuid, String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT id, topic, kafka_key, payload \
         FROM events_outbox \
         WHERE published_at IS NULL \
         ORDER BY created_at \
         LIMIT $1 \
         FOR UPDATE SKIP LOCKED",
    )
    .bind(BATCH_SIZE)
    .fetch_all(&mut *tx)
    .await?;

    if rows.is_empty() {
        // No work — commit to release the snapshot and move on.
        tx.commit().await?;
        return Ok(());
    }

    // Phase 2: attempt to publish each claimed row to Kafka. Track outcomes
    // in memory; we apply them to the DB at the end of the same tx so
    // successful rows are marked published and failed rows keep their
    // position for the next tick.
    let mut successes: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut failures: Vec<(Uuid, String)> = Vec::new();

    for (id, topic, key, payload) in rows {
        // Skip payloads that somehow serialized to a non-string (shouldn't
        // happen but guard so one bad row doesn't stall the batch).
        let body = match serde_json::to_string(&payload) {
            Ok(s) => s,
            Err(e) => {
                failures.push((id, format!("payload re-serialize error: {e}")));
                continue;
            }
        };

        match super::producer::publish(producer, &topic, &key, &body).await {
            Ok(()) => successes.push(id),
            Err(e) => failures.push((id, format!("kafka: {e}"))),
        }
    }

    // Phase 3: apply outcomes.
    if !successes.is_empty() {
        sqlx::query("UPDATE events_outbox SET published_at = NOW() WHERE id = ANY($1)")
            .bind(&successes)
            .execute(&mut *tx)
            .await?;
    }

    for (id, reason) in failures.iter() {
        sqlx::query(
            "UPDATE events_outbox SET attempts = attempts + 1, last_error = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    if !successes.is_empty() || !failures.is_empty() {
        tracing::debug!(
            published = successes.len(),
            failed = failures.len(),
            "outbox drain tick"
        );
    }

    Ok(())
}
