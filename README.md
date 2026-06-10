# Dream Fly Backend

Dream Fly 體操館的後端服務 — 以 Rust + Axum 0.8 打造的模組化 REST API，涵蓋會員、課程、場館預約、電商訂單、貼文與通知等完整業務領域。

## 技術棧

| 類別 | 技術 |
| --- | --- |
| 語言 / 版本 | Rust 2024 edition (rustc ≥ 1.85) |
| Web 框架 | [Axum](https://github.com/tokio-rs/axum) 0.8 + Tower / Tower-HTTP |
| 資料庫 | PostgreSQL 17 + [sqlx](https://github.com/launchbadge/sqlx) 0.8 |
| 快取 / Rate limit | Redis 7 |
| 訊息佇列（選用） | Apache Kafka (Confluent 7.7.0) |
| 認證 | JWT (HS256) + Argon2 密碼雜湊 + Google OAuth2 |
| 外部服務 | SMTP (lettre) / Twilio SMS (reqwest) |
| 設定 | `config` crate + `.env` 階層式覆寫 |

## 架構總覽

### 請求流程

```
HTTP → Rate Limit (Redis) → CORS → Tracing → Compression → Body Limit (2MB)
     → Router (/api/v1/*) → AuthUser extractor (JWT + RBAC)
     → Handler → Service → Repository → PostgreSQL
     → JSON response
```

### 模組化的領域層

每一個業務領域都位於 `src/modules/{name}/`，並遵循統一結構：

- `model.rs` — sqlx `FromRow` struct 與 enum
- `dto.rs` — Request / Response DTO（含 `validator` 驗證）
- `repository.rs` — 資料庫查詢
- `service.rs` — 業務邏輯，銜接 handler 與 repository
- `handlers.rs` — Axum handler（使用 `State<AppState>`、`AuthUser`、`ValidatedJson<T>` 等 extractor）
- `routes.rs` — 匯出 `router() -> Router<AppState>`，由 `src/startup.rs` 組合進主路由

目前包含 14 個領域模組：

```
auth · users · permissions · coaches · courses · venues
schedule · bookings · products · cart · orders · posts
notifications · contact
```

### 關鍵檔案

| 檔案 | 用途 |
| --- | --- |
| `src/main.rs` | 啟動入口：載入 env、tracing、DB pool、Redis、（選用）Kafka producer/consumer |
| `src/startup.rs` | 組建 Axum Router、掛載中介層、`/api/v1/health` 健康檢查 |
| `src/state/mod.rs` | `AppState { db, redis, kafka_producer, config }` |
| `src/config/mod.rs` | 階層式設定：`config/default.toml` → `config/{APP_ENV}.toml` → `APP__*` 環境變數 |
| `src/error/mod.rs` | `AppError` enum → HTTP 狀態碼，統一錯誤回應格式 |

### 認證與授權

- `src/extractors/auth.rs` 的 `AuthUser` extractor 驗證 Bearer JWT，從 Redis（`user_roles:{id}`，TTL 15 分鐘，fallback 回 DB）載入使用者角色
- 受保護的 handler 加入 `auth: AuthUser` 參數，未加入則為公開端點
- 角色檢查：`auth.require_role("admin")?` 或 `auth.is_admin()`
- 內建角色（由 migration seed）：`admin`、`coach`、`member`、`guest`

## 專案結構

```
.
├── Cargo.toml
├── docker-compose.yml       # PostgreSQL / Redis / Kafka 本機服務
├── config/                  # default.toml + 環境覆寫設定
├── migrations/              # sqlx migration SQL
├── src/
│   ├── main.rs
│   ├── startup.rs
│   ├── config/
│   ├── state/
│   ├── error/
│   ├── extractors/          # auth / pagination / validation
│   ├── middleware/          # rate limit 等
│   ├── utils/               # jwt / password / email / sms
│   ├── kafka/               # producer / consumer / events
│   └── modules/             # 14 個業務領域
└── tests/                   # 整合測試（TestApp harness）
```

## 快速開始

### 1. 先決條件

- Rust 工具鏈（rustc ≥ 1.85，建議用 [rustup](https://rustup.rs/)）
- Docker 與 Docker Compose
- `sqlx-cli`（用於執行 migration）

  ```bash
  cargo install sqlx-cli --no-default-features --features rustls,postgres
  ```

### 2. 啟動基礎設施

```bash
# 啟動 PostgreSQL + Redis（預設）
docker-compose up -d

# 若需要 Kafka，加上 kafka profile（會一併啟動 Zookeeper / Kafka / Kafka-UI）
docker-compose --profile kafka up -d
```

服務埠：

| Service | Port |
| --- | --- |
| PostgreSQL | 5432 |
| Redis | 6379 |
| Kafka | 9092 |
| Zookeeper | 2181 |
| Kafka-UI | 8080 |

### 3. 設定環境變數

```bash
cp .env.example .env
# 依需要修改 .env（JWT secret、Google OAuth、SMTP、Twilio 等）
```

### 4. 執行資料庫 Migration

```bash
cargo sqlx migrate run
```

### 5. 建置並啟動服務

```bash
cargo build
cargo run
```

服務將監聽於 `http://0.0.0.0:3000`，健康檢查端點：

```bash
curl http://localhost:3000/api/v1/health
```

## 常用指令

```bash
# 快速型別檢查（不實際編譯）
cargo check

# 執行所有測試
cargo test

# 執行單一測試
cargo test test_name

# 執行特定模組的測試
cargo test module_name::

# 建立新的 migration
cargo sqlx migrate add <name>
```

## 設定系統

設定採階層式覆寫，載入順序（後者覆蓋前者）：

1. `config/default.toml`
2. `config/{APP_ENV}.toml`（由 `APP_ENV` 決定，預設 `development`）
3. 環境變數：`APP__*` 前綴，使用 `__` 作為巢狀分隔（例：`APP__DATABASE__URL`）

`AppConfig` 涵蓋：`ServerConfig` · `DatabaseConfig` · `RedisConfig` · `KafkaConfig` · `AuthConfig`（JWT + Google OAuth）· `EmailConfig` · `SmsConfig`。

## 資料庫慣例

- 使用執行時查詢 `sqlx::query_as::<_, Model>("SQL").bind(val).fetch_*()`，而非 `query_as!` 巨集，避免編譯期資料庫依賴
- Transaction：

  ```rust
  let mut tx = db.begin().await?;
  sqlx::query("...").execute(&mut *tx).await?;
  tx.commit().await?;
  ```
- PostgreSQL enum 對應：

  ```rust
  #[derive(sqlx::Type)]
  #[sqlx(type_name = "enum_name", rename_all = "snake_case")]
  enum Foo { ... }
  ```

  SQL 中需顯式 cast：`$1::enum_name`
- 型別對應：`TEXT[]` → `Vec<String>`；`JSONB` → `Option<serde_json::Value>`；`TIMESTAMPTZ` → `DateTime<Utc>`；`DATE` → `NaiveDate`；`TIME` → `NaiveTime`

## Kafka（選用）

Kafka 預設停用（`APP__KAFKA__ENABLED=false`）。啟用後：

- `src/kafka/producer.rs` — gzip 壓縮、`acks=all` 的 producer
- `src/kafka/events.rs` — `KafkaEvent<T>` 事件封包（UUID v7 `event_id` + timestamp + data）
- `src/kafka/consumer.rs` — 背景 tokio task，僅訂閱 `dreamfly.audit.log`，寫入 `audit_log` 資料表（純稽核用途；通知改由 `notifications::service` 同步寫入）
- Consumer 會在 `main.rs` 依 `APP__KAFKA__ENABLED` 自動啟動

## 錯誤回應格式

所有錯誤統一為：

```json
{ "error": "錯誤訊息" }
```

`AppError` 對應的 HTTP 狀態碼：`400` / `401` / `403` / `404` / `409` / `422` / `500`。

## 測試

整合測試位於 `tests/`，使用 `axum-test` 的 in-process HTTP 客戶端（不需綁定 TCP），並以 `wiremock` mock 外部服務（Google OAuth、Twilio）。執行：

```bash
cargo test
```
