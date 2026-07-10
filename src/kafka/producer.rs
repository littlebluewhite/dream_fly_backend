use std::time::Duration;

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
