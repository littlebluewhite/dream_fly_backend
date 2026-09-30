//! Transactional outbox dispatcher for Kafka events.
//!
//! ## Why an outbox
//!
//! The naïve "publish to Kafka right after commit" pattern has an
//! irrecoverable gap: if the DB commit succeeds but the Kafka send fails
//! (broker down, network blip, process crash), the business state advances
//! while the downstream event is silently lost. Retries never happen
//! because nothing in the DB records that the event was meant to fire.
//!
//! The outbox moves the "remember to publish this" record into the same DB
//! transaction as the business write: either both are durable or neither
//! is. A background dispatcher then drains the outbox to Kafka and marks
//! each row published on broker ack. A crash before ack simply leaves the
//! row unpublished — the dispatcher re-claims it once its claim lease
//! expires (see [`drain_once`]) and retries. This is at-least-once: consumers must still be
//! idempotent, but no events are lost.
//!
//! ## How to use
//!
//! Inside a service-layer transaction that mutates business data, call
//! [`insert_domain_event_tx`] before `tx.commit()` with a payload that
//! implements [`DomainEvent`]; topic, event_type, and partition key all
//! come from the payload's [`DomainEvent::SPEC`] and
//! [`DomainEvent::kafka_key`], so there is nothing to hand-author at the
//! call site. For example, in `orders::service::checkout`:
//!
//! ```ignore
//! outbox::insert_domain_event_tx(
//!     &mut tx,
//!     OrderCreatedPayload { .. },
//!     correlation_id, // from x-request-id, optional
//! ).await?;
//! tx.commit().await?;
//! ```
//!
//! [`insert_event_tx`] remains available as the lower-level entry point for
//! hand-authored events that have no `DomainEvent` impl (e.g. audit events
//! published directly to [`topics::AUDIT_LOG`](super::events::topics::AUDIT_LOG)).
//!
//! The dispatcher is started once at process boot — see
//! [`start_dispatcher`], wired in `main.rs`.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::time::Instant;
use uuid::Uuid;

use super::events::{DomainEvent, KafkaEvent};
use super::producer::EventPublisher;

/// Poll interval for the dispatcher tick loop. Short enough that an event
/// published to the outbox is typically on Kafka within a second, long
/// enough to avoid hammering Postgres when there's no work.
const DISPATCHER_TICK: Duration = Duration::from_millis(500);

/// Maximum number of outbox rows claimed per dispatcher tick, so a backlog
/// (after a Kafka outage, say) is worked off in bounded bites.
const BATCH_SIZE: i64 = 100;

/// How long a claim keeps a row away from other drains. Longer than
/// [`PUBLISH_BUDGET`] plus the bookkeeping transaction, so a row is only
/// re-claimed when its drain died (or stopped at the deadline before
/// reaching it) — never while it is still being published.
const LEASE: Duration = Duration::from_secs(60);

/// Wall-clock budget for publishing one claimed batch. Also set as the
/// producer's `message.timeout.ms` (see `producer::create_producer`), which
/// caps each individual send at the same 15 s. A send the batch deadline cuts
/// off may still be delivered afterwards and is then re-sent on re-claim
/// (at-least-once; the consumer is idempotent, so the duplicate is harmless).
pub(crate) const PUBLISH_BUDGET: Duration = Duration::from_secs(15);

/// Upper bound on the retry backoff of a failing row.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

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

/// Insert a [`DomainEvent`] into the outbox, deriving `topic`/`event_type`
/// from its `SPEC` and the partition key from its `kafka_key()` — the
/// single source of truth for the 5 domain event mappings (see
/// [`crate::kafka::events::ALL_SPECS`]), instead of every publish site
/// repeating them by hand. Delegates to [`insert_event_tx`] so the actual
/// row shape is defined in exactly one place.
pub async fn insert_domain_event_tx<E: DomainEvent>(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: E,
    correlation_id: Option<String>,
) -> Result<(), sqlx::Error> {
    let key = event.kafka_key();
    insert_event_tx(
        tx,
        E::SPEC.topic,
        E::SPEC.event_type,
        &key,
        event,
        correlation_id,
    )
    .await
}

/// Start the background dispatcher task. Runs until `shutdown_rx` goes true.
///
/// If `producer` is `None` (Kafka disabled at boot), the dispatcher does
/// not start — events accumulate in the outbox table but aren't published.
/// Enabling Kafka and restarting the process will drain the backlog.
pub async fn start_dispatcher(
    db: PgPool,
    publisher: Arc<dyn EventPublisher>,
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
                if let Err(e) = drain_once(&db, publisher.as_ref()).await {
                    // Transient DB errors are normal during restarts; log
                    // and keep the loop alive. The next tick will retry.
                    tracing::warn!(error = %e, "outbox drain tick failed");
                }
            }
        }
    }
}

/// Drain up to [`BATCH_SIZE`] due rows in three steps, none of which holds a
/// transaction open across a Kafka round-trip:
///
/// 1. **Claim** — one autocommit `UPDATE … WHERE id IN (SELECT … FOR UPDATE
///    SKIP LOCKED) RETURNING …` pushes each due row's `next_attempt_at` out
///    by [`LEASE`], so concurrent dispatchers (horizontal scale) and later
///    ticks skip it while it is in flight.
/// 2. **Publish** — outside any transaction, oldest `created_at` first,
///    within [`PUBLISH_BUDGET`] (see [`publish_batch`]).
/// 3. **Bookkeeping** — one short transaction: published rows get
///    `published_at`; failed rows get `attempts + 1`, `last_error`, and
///    `next_attempt_at = NOW() + retry_delay(attempts)`. Rows the deadline
///    cut off are left alone and come back when their lease expires.
///
/// ## Poison rows
///
/// There is no terminal state: a row that keeps failing is retried forever,
/// its backoff capped at [`MAX_RETRY_DELAY`], and every failure is logged at
/// ERROR (ADR-0013). The outbox is the only copy of the event, so giving up
/// would lose it.
///
/// ## Ordering
///
/// No per-key ordering guarantee: a failed row backs off while later rows
/// with the same `kafka_key` are published.
///
/// `pub` so integration tests can drive it directly against a
/// [`super::producer::EventPublisher`] fake, exercising the retry/bookkeeping
/// logic (attempts, last_error, published_at) without a Kafka broker.
pub async fn drain_once(db: &PgPool, publisher: &dyn EventPublisher) -> Result<(), sqlx::Error> {
    let mut rows: Vec<ClaimedRow> = sqlx::query_as(
        "UPDATE events_outbox SET next_attempt_at = NOW() + $2 \
         WHERE id IN ( \
             SELECT id FROM events_outbox \
             WHERE published_at IS NULL AND next_attempt_at <= NOW() \
             ORDER BY next_attempt_at \
             LIMIT $1 \
             FOR UPDATE SKIP LOCKED \
         ) \
         RETURNING id, topic, kafka_key, payload, attempts, created_at",
    )
    .bind(BATCH_SIZE)
    .bind(LEASE)
    .fetch_all(db)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }
    rows.sort_by_key(|row| row.created_at);

    let deadline = Instant::now() + PUBLISH_BUDGET;
    let outcome = publish_batch(publisher, &rows, deadline).await;

    let mut tx = db.begin().await?;
    if !outcome.published.is_empty() {
        sqlx::query("UPDATE events_outbox SET published_at = NOW() WHERE id = ANY($1)")
            .bind(&outcome.published)
            .execute(&mut *tx)
            .await?;
    }
    for failure in &outcome.failed {
        let attempts = failure.attempts_before.saturating_add(1);
        let delay = retry_delay(u32::try_from(attempts).unwrap_or(u32::MAX));
        tracing::error!(
            id = %failure.id,
            attempts,
            retry_in_secs = delay.as_secs(),
            error = %failure.reason,
            "outbox publish failed"
        );
        sqlx::query(
            "UPDATE events_outbox \
             SET attempts = attempts + 1, last_error = $2, next_attempt_at = NOW() + $3 \
             WHERE id = $1",
        )
        .bind(failure.id)
        .bind(&failure.reason)
        .bind(delay)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    tracing::debug!(
        claimed = rows.len(),
        published = outcome.published.len(),
        failed = outcome.failed.len(),
        "outbox drain tick"
    );

    Ok(())
}

/// A row claimed by [`drain_once`].
#[derive(sqlx::FromRow)]
struct ClaimedRow {
    id: Uuid,
    topic: String,
    kafka_key: String,
    payload: serde_json::Value,
    attempts: i32,
    created_at: DateTime<Utc>,
}

struct FailedPublish {
    id: Uuid,
    /// `attempts` as claimed, before this failure is counted.
    attempts_before: i32,
    reason: String,
}

#[derive(Default)]
struct BatchOutcome {
    published: Vec<Uuid>,
    failed: Vec<FailedPublish>,
}

/// Publish `rows` in order until done or `deadline`. A publish still pending
/// at the deadline counts as failed and ends the batch: later rows are not
/// attempted and appear in neither list, keeping their lease.
async fn publish_batch(
    publisher: &dyn EventPublisher,
    rows: &[ClaimedRow],
    deadline: Instant,
) -> BatchOutcome {
    let mut outcome = BatchOutcome::default();
    for row in rows {
        let fail = |reason: String| FailedPublish {
            id: row.id,
            attempts_before: row.attempts,
            reason,
        };
        let body = match serde_json::to_string(&row.payload) {
            Ok(s) => s,
            Err(e) => {
                outcome
                    .failed
                    .push(fail(format!("payload re-serialize error: {e}")));
                continue;
            }
        };
        match tokio::time::timeout_at(
            deadline,
            publisher.publish(&row.topic, &row.kafka_key, &body),
        )
        .await
        {
            Ok(Ok(())) => outcome.published.push(row.id),
            Ok(Err(e)) => outcome.failed.push(fail(format!("kafka: {e}"))),
            Err(_) => {
                outcome.failed.push(fail(format!(
                    "publish exceeded the {}s batch budget",
                    PUBLISH_BUDGET.as_secs()
                )));
                break;
            }
        }
    }
    outcome
}

/// Backoff before retrying a row that has now failed `attempts` times:
/// `min(2^attempts s, 1 h)`.
fn retry_delay(attempts: u32) -> Duration {
    let secs = 1u64.checked_shl(attempts).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use rdkafka::error::KafkaError;

    use super::*;

    #[test]
    fn retry_delay_doubles_and_caps_at_one_hour() {
        assert_eq!(retry_delay(1), Duration::from_secs(2));
        assert_eq!(retry_delay(2), Duration::from_secs(4));
        assert_eq!(retry_delay(3), Duration::from_secs(8));
        assert_eq!(retry_delay(11), Duration::from_secs(2048));
        assert_eq!(retry_delay(12), MAX_RETRY_DELAY, "2^12 s exceeds 1 h");
        assert_eq!(
            retry_delay(64),
            MAX_RETRY_DELAY,
            "shift overflow still caps"
        );
        assert_eq!(retry_delay(u32::MAX), MAX_RETRY_DELAY);
    }

    /// The second call never completes; every other call succeeds at once.
    struct HangsOnSecond {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl EventPublisher for HangsOnSecond {
        async fn publish(&self, _: &str, _: &str, _: &str) -> Result<(), KafkaError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                std::future::pending::<()>().await;
            }
            Ok(())
        }
    }

    fn row(attempts: i32) -> ClaimedRow {
        ClaimedRow {
            id: Uuid::now_v7(),
            topic: "dreamfly.orders.created".into(),
            kafka_key: "k".into(),
            payload: serde_json::json!({}),
            attempts,
            created_at: Utc::now(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn publish_batch_stops_at_deadline() {
        let rows = [row(0), row(3), row(0)];
        let publisher = HangsOnSecond {
            calls: AtomicUsize::new(0),
        };
        let start = Instant::now();

        let outcome = publish_batch(&publisher, &rows, start + PUBLISH_BUDGET).await;

        assert_eq!(Instant::now() - start, PUBLISH_BUDGET);
        assert_eq!(outcome.published, vec![rows[0].id]);
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].id, rows[1].id);
        assert_eq!(outcome.failed[0].attempts_before, 3);
        assert!(outcome.failed[0].reason.contains("budget"));
        assert_eq!(
            publisher.calls.load(Ordering::SeqCst),
            2,
            "rows after the timed-out one are not attempted (they keep their lease)"
        );
    }
}
