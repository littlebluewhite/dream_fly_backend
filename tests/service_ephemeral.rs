//! Integration tests for the `utils::ephemeral::EphemeralStore` seam.
//!
//! Every scenario is written once against `&dyn EphemeralStore` and run
//! against both adapters — the production `RedisEphemeralStore` (real Redis,
//! DB 0) and the in-memory one the HTTP harness uses
//! (`common::mocks::InMemoryEphemeralStore`) — so the two stay observably
//! interchangeable. Each scenario uses fresh `Uuid` keys, so leftover Redis
//! keys from earlier runs never collide. What a store failure *means* is the
//! owners' business, pinned in `service_auth.rs` / `middleware_rate_limit.rs`
//! with `FailingEphemeralStore`.

mod common;

use std::time::Duration;

use uuid::Uuid;

use dream_fly_backend::utils::ephemeral::{EphemeralStore, RedisEphemeralStore};

use common::mocks::InMemoryEphemeralStore;

fn key(name: &str) -> String {
    format!("ephemeral-test:{name}:{}", Uuid::now_v7())
}

// --- scenarios ---------------------------------------------------------

/// Counts from 1; the TTL is set when the key is created and a later incr
/// does not extend it (the count still dies 1s after the first incr).
async fn incr_does_not_extend_ttl(store: &dyn EphemeralStore) {
    let k = key("incr");
    assert_eq!(store.incr_with_ttl(&k, 1).await.expect("incr"), 1);

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(store.incr_with_ttl(&k, 1).await.expect("incr"), 2);

    // 1.4s after creation: gone if the TTL was kept, alive until 1.7s had
    // the second incr reset it.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(store.get(&k).await.expect("get"), None);
    assert_eq!(store.incr_with_ttl(&k, 1).await.expect("incr"), 1);
}

/// A value read after its TTL is gone.
async fn expired_value_reads_none(store: &dyn EphemeralStore) {
    let k = key("expire");
    store.set_ex(&k, "v", 1).await.expect("set_ex");
    assert_eq!(store.get(&k).await.expect("get"), Some("v".to_string()));

    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(store.get(&k).await.expect("get"), None);
    assert_eq!(store.getdel(&k).await.expect("getdel"), None);
}

/// set/get/del round trip; set overwrites; deleting a missing key is fine.
async fn set_get_del(store: &dyn EphemeralStore) {
    let k = key("setget");
    assert_eq!(store.get(&k).await.expect("get"), None);

    store.set_ex(&k, "first", 60).await.expect("set_ex");
    store.set_ex(&k, "second", 60).await.expect("set_ex");
    assert_eq!(
        store.get(&k).await.expect("get"),
        Some("second".to_string())
    );

    store.del(&k).await.expect("del");
    assert_eq!(store.get(&k).await.expect("get"), None);
    store.del(&k).await.expect("del of a missing key");
}

/// getdel hands a value out exactly once.
async fn getdel_returns_value_once(store: &dyn EphemeralStore) {
    let k = key("getdel");
    store.set_ex(&k, "token-owner", 60).await.expect("set_ex");

    assert_eq!(
        store.getdel(&k).await.expect("getdel"),
        Some("token-owner".to_string())
    );
    assert_eq!(store.getdel(&k).await.expect("getdel"), None);
    assert_eq!(store.get(&k).await.expect("get"), None);
}

async fn redis_store() -> RedisEphemeralStore {
    RedisEphemeralStore::new(common::test_redis().await)
}

macro_rules! both_adapters {
    ($($scenario:ident),* $(,)?) => {$(
        mod $scenario {
            use super::*;

            #[tokio::test]
            async fn in_memory() {
                super::$scenario(&InMemoryEphemeralStore::new()).await;
            }

            #[tokio::test]
            async fn redis() {
                super::$scenario(&redis_store().await).await;
            }
        }
    )*};
}

both_adapters!(
    incr_does_not_extend_ttl,
    expired_value_reads_none,
    set_get_del,
    getdel_returns_value_once,
);
