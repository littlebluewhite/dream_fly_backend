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
/// `last_error` — see `migrations/20260410000001_init.sql` — plus
/// `next_attempt_at`, defaulting to `NOW()` so the row is due at once),
/// bypassing `insert_event_tx` so `created_at` can be pinned for
/// deterministic oldest-first publishing.
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

async fn next_attempt_at(db: &PgPool, id: Uuid) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT next_attempt_at FROM events_outbox WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .expect("events_outbox row")
}

// ---------------------------------------------------------------------------

#[sqlx::test]
async fn drain_once_publishes_first_row_and_backs_off_failed_second(db: PgPool) {
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

    let retry_at = next_attempt_at(&db, row2).await;
    assert!(
        retry_at > Utc::now(),
        "a failed row backs off: next_attempt_at is pushed into the future"
    );

    // Re-drain immediately: row2 is backing off, so it must not be re-sent.
    drain_once(&db, &fake)
        .await
        .expect("immediate re-drain should succeed");
    assert_eq!(
        fake.call_count(),
        2,
        "a row still in backoff must not be published again"
    );
    let state2_backing_off = outbox_row_state(&db, row2).await;
    assert!(state2_backing_off.published_at.is_none());
    assert_eq!(state2_backing_off.attempts, 1);

    // Once its backoff is over, the retry (the fake's 3rd call overall)
    // succeeds and the previously-failed row is published.
    sqlx::query("UPDATE events_outbox SET next_attempt_at = NOW() WHERE id = $1")
        .bind(row2)
        .execute(&db)
        .await
        .expect("expire row2's backoff");
    drain_once(&db, &fake)
        .await
        .expect("re-drain after backoff should succeed");

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

/// Blocks inside `publish` until released, so a test can hold a drain
/// mid-publish and observe what a concurrent drain does meanwhile.
struct GatedPublisher {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: AtomicUsize,
}

#[async_trait]
impl EventPublisher for GatedPublisher {
    async fn publish(&self, _topic: &str, _key: &str, _payload: &str) -> Result<(), KafkaError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[sqlx::test]
async fn concurrent_drain_does_not_reclaim_a_row_being_published(db: PgPool) {
    let id = Uuid::now_v7();
    insert_outbox_row(&db, id, Utc::now()).await;

    let gated = std::sync::Arc::new(GatedPublisher {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    });
    let first = {
        let db = db.clone();
        let gated = gated.clone();
        tokio::spawn(async move { drain_once(&db, gated.as_ref()).await })
    };
    gated.entered.notified().await;

    // The first drain is mid-publish on the only row; a second drain must
    // not claim it.
    let other = FakePublisher {
        fail_second: false,
        calls: AtomicUsize::new(0),
    };
    drain_once(&db, &other)
        .await
        .expect("concurrent drain should succeed");
    assert_eq!(
        other.call_count(),
        0,
        "a row claimed by an in-flight drain must not be published again by a concurrent drain"
    );

    gated.release.notify_one();
    first
        .await
        .expect("first drain task")
        .expect("first drain should succeed");
    assert_eq!(gated.calls.load(Ordering::SeqCst), 1);
    assert!(outbox_row_state(&db, id).await.published_at.is_some());
}

/// Takes longer to publish than the test pool's
/// `idle_in_transaction_session_timeout`.
struct SlowPublisher;

#[async_trait]
impl EventPublisher for SlowPublisher {
    async fn publish(&self, _topic: &str, _key: &str, _payload: &str) -> Result<(), KafkaError> {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        Ok(())
    }
}

/// Production pins `idle_in_transaction_session_timeout = '30s'`
/// (`main.rs`); a publish outliving it must not cost the bookkeeping. The
/// hand-built pool is not owned by `#[sqlx::test]`, so it is closed
/// explicitly (see `tests/service_orders.rs`).
#[sqlx::test]
async fn bookkeeping_lands_when_publish_outlives_idle_in_transaction_timeout(db: PgPool) {
    let id = Uuid::now_v7();
    insert_outbox_row(&db, id, Utc::now()).await;

    let connect_opts = db.connect_options().as_ref().clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                use sqlx::Executor;
                conn.execute("SET idle_in_transaction_session_timeout = '1s'")
                    .await?;
                Ok(())
            })
        })
        .connect_with(connect_opts)
        .await
        .expect("build pool with a 1s idle-in-transaction timeout");

    let result = drain_once(&pool, &SlowPublisher).await;
    pool.close().await;

    assert!(result.is_ok(), "drain_once failed: {result:?}");
    assert!(
        outbox_row_state(&db, id).await.published_at.is_some(),
        "a publish slower than idle_in_transaction_session_timeout must still get marked published"
    );
}
