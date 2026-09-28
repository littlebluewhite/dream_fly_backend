//! 健康探針(Health Probe)——`GET /health` 回報的依賴狀態。
//!
//! [`HealthProbe`] 是 port,與正式 adapter [`RedisHealthProbe`] 和 handler
//! [`health_check`] 同檔(比照 `utils::ephemeral`)。probe 只回答「Redis
//! PING 得通嗎」與「開機時 Kafka producer 建成了嗎」;逾時、回應 body、狀
//! 態碼全歸 [`health_check`]。DB 不是 seam:handler 直接對 `AppState::db`
//! 跑 `SELECT 1`。整合測試換 `StaticHealthProbe`(`tests/common/mocks.rs`),
//! 所以 HTTP 測試不需要 Redis。

use std::time::Duration;

use async_trait::async_trait;
use axum::{Json, extract::State, http::StatusCode};
use redis::aio::ConnectionManager;
use serde_json::{Value, json};

use crate::state::AppState;

/// `/health` 需要的依賴答案。
#[async_trait]
pub trait HealthProbe: Send + Sync {
    /// PING Redis。500ms 逾時由 [`health_check`] 界住,adapter 不自己設。
    async fn ping_redis(&self) -> anyhow::Result<()>;
    /// 開機時 Kafka producer 是否建成(靜態值,執行中不重新探測)。
    fn kafka_connected(&self) -> bool;
}

/// 正式 adapter:PING 共用的 Redis 連線;Kafka 旗標在開機時定下。
pub struct RedisHealthProbe {
    redis: ConnectionManager,
    kafka_connected: bool,
}

impl RedisHealthProbe {
    pub fn new(redis: ConnectionManager, kafka_connected: bool) -> Self {
        Self {
            redis,
            kafka_connected,
        }
    }
}

#[async_trait]
impl HealthProbe for RedisHealthProbe {
    async fn ping_redis(&self) -> anyhow::Result<()> {
        // `ConnectionManager` is a cheap multiplexed handle; commands need
        // `&mut`, so clone per call.
        redis::cmd("PING")
            .query_async::<String>(&mut self.redis.clone())
            .await?;
        Ok(())
    }

    fn kafka_connected(&self) -> bool {
        self.kafka_connected
    }
}

pub async fn health_check(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    // Bound each dependency probe so a wedged Redis/PG cannot hang liveness.
    let db_ok = tokio::time::timeout(
        Duration::from_millis(500),
        sqlx::query("SELECT 1").execute(&state.db),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some();

    let redis_ok = tokio::time::timeout(Duration::from_millis(500), state.health.ping_redis())
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some();

    let kafka_status = if state.health.kafka_connected() {
        "connected"
    } else {
        "disabled"
    };

    let healthy = db_ok && redis_ok;
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(json!({
            "status": if healthy { "healthy" } else { "degraded" },
            "services": {
                "database": if db_ok { "up" } else { "down" },
                "redis": if redis_ok { "up" } else { "down" },
                "kafka": kafka_status,
            }
        })),
    )
}
