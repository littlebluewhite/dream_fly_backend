use std::time::Duration;

use rdkafka::config::ClientConfig;
use rdkafka::error::KafkaError;
use rdkafka::producer::{FutureProducer, FutureRecord};
use serde::Serialize;

use super::events::KafkaEvent;

pub fn create_producer(brokers: &str) -> Result<FutureProducer, KafkaError> {
    ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("compression.type", "gzip")
        // `acks=all` + `enable.idempotence=true` together give exactly-once
        // delivery on the producer side: broker retries will not create
        // duplicates, and a failed write either produces exactly once or
        // returns an unambiguous error.
        .set("request.required.acks", "all")
        .set("enable.idempotence", "true")
        .set("max.in.flight.requests.per.connection", "5")
        .set("retry.backoff.ms", "500")
        .set("queue.buffering.max.messages", "100000")
        .set("queue.buffering.max.ms", "5")
        .create()
}

pub async fn publish(
    producer: &FutureProducer,
    topic: &str,
    key: &str,
    payload: &str,
) -> Result<(), KafkaError> {
    let record = FutureRecord::to(topic).key(key).payload(payload);

    match producer.send(record, Duration::from_secs(5)).await {
        Ok(delivery) => {
            tracing::debug!(
                topic,
                partition = delivery.partition,
                offset = delivery.offset,
                "Kafka message delivered"
            );
            Ok(())
        }
        Err((err, _)) => {
            tracing::error!("Kafka delivery failed: {err}");
            Err(err)
        }
    }
}

/// Best-effort event publish helper.
///
/// Semantics:
/// - If `producer` is `None` (e.g., `KAFKA_ENABLED=false`), this is a no-op.
/// - On serialization or send failure, logs at `error` level and swallows
///   the error. Event publish must never fail the calling HTTP request
///   because the underlying DB transaction is already committed by the
///   time we get here.
/// - This is at-most-once. For at-least-once, switch to an outbox pattern.
///
/// Callers typically pass:
/// - `topic` — one of the constants in [`crate::kafka::events::topics`]
/// - `event_type` — one of the constants in [`crate::kafka::events::event_types`]
/// - `key` — a string used for Kafka partitioning (usually user_id or resource id)
/// - `data` — any `Serialize` payload struct
pub async fn publish_event<T: Serialize>(
    producer: Option<&FutureProducer>,
    topic: &'static str,
    event_type: &'static str,
    key: &str,
    data: T,
) {
    let Some(producer) = producer else {
        return;
    };

    let envelope = KafkaEvent::new(event_type, data);
    let payload = match serde_json::to_string(&envelope) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(topic, "failed to serialize kafka event: {e}");
            return;
        }
    };

    if let Err(e) = publish(producer, topic, key, &payload).await {
        tracing::error!(topic, "kafka publish failed (event dropped): {e}");
    }
}
