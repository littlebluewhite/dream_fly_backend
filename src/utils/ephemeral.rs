//! 短期狀態(Ephemeral State)——帶 TTL 的字串與計數:路由限流桶、登入失敗
//! 計數、忘記密碼計數、OTP(碼 + 請求/嘗試計數)、密碼重設 token。
//!
//! [`EphemeralStore`] 是 port,與正式 adapter [`RedisEphemeralStore`] 同檔
//! (比照 `utils::email`)。它只存字串/計數、回 `Result`;key 格式、TTL、門
//! 檻、故障時 fail-open 還是 fail-closed,全歸各 owner(`auth::rate_limit`、
//! `auth::otp`、`auth::reset_tokens`、`middleware::rate_limit`),adapter 不
//! 做任何決定。整合測試換 in-memory adapter(`tests/common/mocks.rs`)。
//!
//! **為什麼放在 `utils`**:`middleware` 不能依賴 `auth`(middleware 擋在每
//! 條路由前面,auth 只是它後面的一個模組),`auth` 也不該依賴 `middleware`
//! (auth 是領域邏輯,不是 HTTP 接線)——`utils` 是兩者都能依賴的共同下游,
//! 放這裡才不會造出跨層依賴。
//!
//! **為什麼不擴充 `auth::access::AccessCache`**:access 用不到
//! `incr_with_ttl`/`getdel`,併進去只會加寬它的 interface。兩個 Redis
//! adapter 共用同一個 `ConnectionManager`(`main.rs` 各包一份
//! `redis.clone()`)。

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;

/// 短期狀態的儲存 port。失敗一律回 `Err`;失敗代表什麼(放行或拒絕)由呼
/// 叫端決定。
#[async_trait]
pub trait EphemeralStore: Send + Sync {
    /// 把 `key` 的計數加一並回傳加完的值。TTL 只在這次加一「建立」了 key
    /// 時設定(計數變成 1 的那一次),之後的加一不會延長它。
    async fn incr_with_ttl(&self, key: &str, ttl_secs: u64) -> anyhow::Result<i64>;
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>>;
    /// 寫入值並設定 TTL(一個原子動作)。
    async fn set_ex(&self, key: &str, val: &str, ttl_secs: u64) -> anyhow::Result<()>;
    async fn del(&self, key: &str) -> anyhow::Result<()>;
    /// 原子地讀出並刪除 `key`——同一個值只可能被一個呼叫者取到。
    async fn getdel(&self, key: &str) -> anyhow::Result<Option<String>>;
}

/// INCR,且只在這次 INCR 建立 key 時設 EXPIRE。分成兩個指令會有競態:兩個
/// 指令之間 process 崩潰或連線中斷,計數就永遠沒有 TTL、無限增長;一支 Lua
/// script 讓兩步成為原子。
const INCR_EXPIRE_SCRIPT: &str = r#"
local current = redis.call('INCR', KEYS[1])
if current == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
end
return current
"#;

/// Production adapter over the shared Redis connection manager.
pub struct RedisEphemeralStore(ConnectionManager);

impl RedisEphemeralStore {
    pub fn new(conn: ConnectionManager) -> Self {
        Self(conn)
    }
}

#[async_trait]
impl EphemeralStore for RedisEphemeralStore {
    async fn incr_with_ttl(&self, key: &str, ttl_secs: u64) -> anyhow::Result<i64> {
        // `ConnectionManager` is a cheap multiplexed handle; commands need
        // `&mut`, so each call works on its own clone.
        let mut conn = self.0.clone();
        let count = redis::Script::new(INCR_EXPIRE_SCRIPT)
            .key(key)
            .arg(ttl_secs)
            .invoke_async::<i64>(&mut conn)
            .await?;
        Ok(count)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let mut conn = self.0.clone();
        Ok(conn.get(key).await?)
    }

    async fn set_ex(&self, key: &str, val: &str, ttl_secs: u64) -> anyhow::Result<()> {
        let mut conn = self.0.clone();
        conn.set_ex::<_, _, ()>(key, val, ttl_secs).await?;
        Ok(())
    }

    async fn del(&self, key: &str) -> anyhow::Result<()> {
        let mut conn = self.0.clone();
        conn.del::<_, ()>(key).await?;
        Ok(())
    }

    async fn getdel(&self, key: &str) -> anyhow::Result<Option<String>> {
        let mut conn = self.0.clone();
        Ok(conn.get_del(key).await?)
    }
}

/// Unit-test fake shared by the owners' key/TTL pin tests
/// (`auth::rate_limit`, `auth::otp`, `auth::reset_tokens`,
/// `middleware::rate_limit`): a plain map (TTLs are recorded, never
/// enforced) plus a log of every call with the exact key and TTL it carried.
#[cfg(test)]
pub(crate) mod recording {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::EphemeralStore;

    /// One store call. `SetEx` omits the value — owners store random
    /// codes/tokens; the pins are about keys and TTLs.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Call {
        Incr(String, u64),
        Get(String),
        SetEx(String, u64),
        Del(String),
        GetDel(String),
    }

    #[derive(Default)]
    pub(crate) struct RecordingStore {
        values: Mutex<HashMap<String, String>>,
        calls: Mutex<Vec<Call>>,
    }

    impl RecordingStore {
        /// Pre-seed a value without logging a call.
        pub(crate) fn with_value(self, key: &str, val: &str) -> Self {
            self.values.lock().unwrap().insert(key.into(), val.into());
            self
        }

        /// Drain the call log.
        pub(crate) fn take_calls(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }

        fn log(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }
    }

    #[async_trait]
    impl EphemeralStore for RecordingStore {
        async fn incr_with_ttl(&self, key: &str, ttl_secs: u64) -> anyhow::Result<i64> {
            self.log(Call::Incr(key.into(), ttl_secs));
            let mut values = self.values.lock().unwrap();
            let next = values.get(key).map_or(0, |v| v.parse::<i64>().unwrap()) + 1;
            values.insert(key.into(), next.to_string());
            Ok(next)
        }

        async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
            self.log(Call::Get(key.into()));
            Ok(self.values.lock().unwrap().get(key).cloned())
        }

        async fn set_ex(&self, key: &str, val: &str, ttl_secs: u64) -> anyhow::Result<()> {
            self.log(Call::SetEx(key.into(), ttl_secs));
            self.values.lock().unwrap().insert(key.into(), val.into());
            Ok(())
        }

        async fn del(&self, key: &str) -> anyhow::Result<()> {
            self.log(Call::Del(key.into()));
            self.values.lock().unwrap().remove(key);
            Ok(())
        }

        async fn getdel(&self, key: &str) -> anyhow::Result<Option<String>> {
            self.log(Call::GetDel(key.into()));
            Ok(self.values.lock().unwrap().remove(key))
        }
    }
}
