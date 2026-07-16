//! Integration tests for `kafka::outbox::drain_once` against a `FakePublisher`
//! (this file's only consumer, so it is not promoted to `tests/common/mocks.rs`)
//! substituted for `KafkaPublisher`. This exercises the dispatcher's
//! retry/bookkeeping logic — attempts, last_error, published_at — without a
//! Kafka broker.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use rdkafka::error::{KafkaError, RDKafkaErrorCode};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use dream_fly_backend::kafka::outbox::drain_once;
use dream_fly_backend::kafka::producer::EventPublisher;

/// Records how many times `publish` has been called across this instance's
/// lifetime. When `fail_second` is set, exactly the second call overall
/// fails (simulating one row's send failing mid-batch); every other call —
/// including a retry on a later `drain_once` invocation reusing the same
/// instance — succeeds.
struct FakePublisher {
    fail_second: bool,
    calls: AtomicUsize,
}

impl FakePublisher {
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl EventPublisher for FakePublisher {
    async fn publish(&self, _topic: &str, _key: &str, _payload: &str) -> Result<(), KafkaError> {
        let call_number = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_second && call_number == 2 {
            return Err(KafkaError::MessageProduction(RDKafkaErrorCode::Fail));
        }
        Ok(())
    }
}

/// Insert a row directly into `events_outbox` (schema: `id`, `topic`,
/// `kafka_key`, `payload`, `created_at`, `published_at`, `attempts`,
/// `last_error` — see `migrations/20260410000001_init.sql`), bypassing
/// `insert_event_tx` so `created_at` can be pinned for deterministic
/// `ORDER BY created_at` draining.
async fn insert_outbox_row(db: &PgPool, id: Uuid, created_at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO events_outbox (id, topic, kafka_key, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind("dreamfly.orders.created")
    .bind(id.to_string())
    .bind(json!({"order_id": id.to_string()}))
    .bind(created_at)
    .execute(db)
    .await
    .expect("insert events_outbox row");
}

struct OutboxRowState {
    published_at: Option<DateTime<Utc>>,
    attempts: i32,
    last_error: Option<String>,
}

async fn outbox_row_state(db: &PgPool, id: Uuid) -> OutboxRowState {
    let (published_at, attempts, last_error): (Option<DateTime<Utc>>, i32, Option<String>) =
        sqlx::query_as("SELECT published_at, attempts, last_error FROM events_outbox WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await
            .expect("events_outbox row");
    OutboxRowState { published_at, attempts, last_error }
}

// ---------------------------------------------------------------------------

#[sqlx::test]
async fn drain_once_publishes_first_row_and_records_failed_attempt_on_second(db: PgPool) {
    let row1 = Uuid::now_v7();
    let row2 = Uuid::now_v7();
    let t0 = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    insert_outbox_row(&db, row1, t0).await;
    insert_outbox_row(&db, row2, t0 + chrono::Duration::seconds(1)).await;

    let fake = FakePublisher {
        fail_second: true,
        calls: AtomicUsize::new(0),
    };
    drain_once(&db, &fake)
        .await
        .expect("drain_once must not error even when one row's publish fails");

    let state1 = outbox_row_state(&db, row1).await;
    assert!(
        state1.published_at.is_some(),
        "row1 (earliest created_at, drained first) is the fake's first call and succeeds"
    );
    assert_eq!(state1.attempts, 0);
    assert!(state1.last_error.is_none());

    let state2 = outbox_row_state(&db, row2).await;
    assert!(
        state2.published_at.is_none(),
        "row2 is the fake's second call, which fails, so it stays unpublished"
    );
    assert_eq!(state2.attempts, 1);
    assert!(state2.last_error.is_some());

    // Re-drain with the same fake instance: only row2 is still pending, and
    // the fake only fails its second-ever call — so this retry (the fake's
    // 3rd call overall) succeeds and the previously-failed row is published.
    drain_once(&db, &fake)
        .await
        .expect("re-drain should succeed once the fake stops failing");

    let state2_after = outbox_row_state(&db, row2).await;
    assert!(
        state2_after.published_at.is_some(),
        "retry should publish the row that failed on the first drain"
    );
    assert_eq!(
        state2_after.attempts, 1,
        "a successful retry must not bump attempts again"
    );
}

#[sqlx::test]
async fn drain_once_on_empty_table_is_a_noop(db: PgPool) {
    let fake = FakePublisher {
        fail_second: false,
        calls: AtomicUsize::new(0),
    };

    drain_once(&db, &fake)
        .await
        .expect("drain_once on an empty outbox should succeed");

    assert_eq!(fake.call_count(), 0, "no rows means publish is never called");
}

#[sqlx::test]
async fn drain_once_does_not_republish_already_published_rows(db: PgPool) {
    let id = Uuid::now_v7();
    insert_outbox_row(&db, id, Utc::now()).await;
    sqlx::query("UPDATE events_outbox SET published_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .expect("mark row as already published");

    let fake = FakePublisher {
        fail_second: false,
        calls: AtomicUsize::new(0),
    };
    drain_once(&db, &fake)
        .await
        .expect("drain_once should succeed");

    assert_eq!(
        fake.call_count(),
        0,
        "a row with published_at already set must not be re-sent (WHERE published_at IS NULL)"
    );
    let state = outbox_row_state(&db, id).await;
    assert_eq!(state.attempts, 0, "a row the dispatcher never touched keeps attempts at 0");
}
