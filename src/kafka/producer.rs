use std::time::Duration;

use async_trait::async_trait;
use rdkafka::config::ClientConfig;
use rdkafka::error::KafkaError;
use rdkafka::producer::{FutureProducer, FutureRecord};

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

/// Trait-object facade for outbound Kafka publishing so the outbox
/// dispatcher can hold a `&dyn EventPublisher` / `Arc<dyn EventPublisher>`.
/// In production this is backed by `KafkaPublisher` (a real `FutureProducer`);
/// integration tests substitute an in-memory fake that never touches a
/// broker, so the dispatcher's retry/bookkeeping logic (attempts,
/// last_error, published_at) can be exercised without Kafka running.
#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish(&self, topic: &str, key: &str, payload: &str) -> Result<(), KafkaError>;
}

pub struct KafkaPublisher(pub FutureProducer);

#[async_trait]
impl EventPublisher for KafkaPublisher {
    async fn publish(&self, topic: &str, key: &str, payload: &str) -> Result<(), KafkaError> {
        let record = FutureRecord::to(topic).key(key).payload(payload);

        match self.0.send(record, Duration::from_secs(5)).await {
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
}
